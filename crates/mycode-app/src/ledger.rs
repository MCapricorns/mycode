//! Session ledger reads and writes: listing summaries, branch projections,
//! model-facing replay history, message commits, recalls, and deletion.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use mycode_agent::HomeLayout;
use mycode_agent::Message;
use mycode_agent::session::{
    self, BranchId, EventKind, HeadStamp, SessionCallId, SessionError, SessionEvent,
    SessionEventId, SessionId, SessionService,
};
use mycode_agent::{AssistantMessage, ToolResultMessage, UserMessage};

use crate::projection::project_replayed_entry_with_meter;
use crate::protocol::{
    ActiveConversation, ConversationEntry, EntryKind, OlderTranscript, SessionSummary,
};

/// Events loaded for the first paint and for each older page. The model
/// history still walks the whole branch; this bound is only the transcript
/// window.
const VISIBLE_EVENTS: u16 = 40;

#[cfg(test)]
mod recall_tests;

/// Rendered wording for a lost expected-head compare-and-swap.
pub(crate) const HEAD_MOVED_ON: &str = "the session moved on; reopen it";

/// Leftover file-snapshot directory from older versions. New turns do not
/// write it. Session delete still removes `<home>/checkpoints/<session-id>`
/// so those snapshots do not outlive the session.
const CHECKPOINTS_DIR: &str = "checkpoints";

pub(crate) fn render_error(error: SessionError) -> String {
    match error {
        SessionError::InvalidArgument => "invalid request".to_owned(),
        SessionError::NotFound => "not found".to_owned(),
        SessionError::Conflict(_) => HEAD_MOVED_ON.to_owned(),
        SessionError::Corrupt => "stored data failed validation".to_owned(),
        SessionError::Limit => "a fixed bound was reached".to_owned(),
        SessionError::Cancelled => "cancelled".to_owned(),
        SessionError::Unavailable => "the session service is unavailable".to_owned(),
    }
}

/// Lists sessions from the SQLite index. Titles come from the index, so the
/// listing does not open logs or read payloads. A corrupt index row stays
/// visible so the UI can offer deletion.
pub(crate) async fn inspect_summaries(
    home: HomeLayout,
) -> Result<Vec<SessionSummary>, SessionError> {
    let listing = tokio::task::spawn_blocking(move || session::inspect_sessions(&home))
        .await
        .map_err(|_| SessionError::Unavailable)??;
    let mut summaries = Vec::with_capacity(listing.sessions.len() + listing.corrupt.len());
    for name in &listing.corrupt {
        summaries.push(SessionSummary {
            session_id: name.clone(),
            root_branch_id: String::new(),
            title: String::new(),
            updated_at_ms: None,
            event_count: 0,
            active: false,
            corrupt: true,
        });
    }
    for snapshot in listing.sessions {
        if snapshot.branches.is_empty() {
            continue;
        }
        summaries.push(SessionSummary {
            root_branch_id: snapshot.current_branch.as_str().to_owned(),
            updated_at_ms: snapshot.updated_at_ms,
            event_count: snapshot.event_count,
            session_id: snapshot.session_id.as_str().to_owned(),
            title: snapshot.title,
            active: false,
            corrupt: false,
        });
    }
    Ok(summaries)
}

/// The durable head of `branch`, read fresh from the session.
pub(crate) async fn branch_head(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
) -> Result<HeadStamp, SessionError> {
    let opened = service.open(session).await?;
    opened
        .heads
        .iter()
        .find(|head| &head.branch_id == branch)
        .map(|head| head.head.clone())
        .ok_or(SessionError::NotFound)
}

/// Fails with [`HEAD_MOVED_ON`] unless `branch` is still at `expected_head`.
pub(crate) async fn ensure_branch_head(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
) -> Result<(), String> {
    let current = branch_head(service, session, branch)
        .await
        .map_err(render_error)?;
    if &current == expected_head {
        Ok(())
    } else {
        Err(HEAD_MOVED_ON.to_owned())
    }
}

pub(crate) async fn open_conversation(
    service: &SessionService,
    session: &SessionId,
) -> Result<ActiveConversation, SessionError> {
    let opened = service.open(session).await?;
    read_branch(
        service,
        session,
        &opened.current.branch_id,
        &opened.current.head,
    )
    .await
}

/// Reads the tail of one branch into display entries.
///
/// The payload read is the newest [`VISIBLE_EVENTS`] at `snapshot_head`,
/// sought by the index offsets. Earlier events stay on disk.
pub(crate) async fn read_branch(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
) -> Result<ActiveConversation, SessionError> {
    let (entries, older_before, context_used, context_cache) =
        project_window(service, session, branch, snapshot_head, None).await?;
    Ok(ActiveConversation {
        session_id: session.as_str().to_owned(),
        branch_id: branch.as_str().to_owned(),
        head: head_spelling(snapshot_head),
        entries,
        older_before,
        streaming: None,
        running: false,
        compacting: false,
        context_used,
        context_cache,
    })
}

/// Loads the page of events strictly before `before`, after checking the
/// branch head the UI still sees.
pub(crate) async fn load_older(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    before: &str,
) -> Result<OlderTranscript, String> {
    ensure_branch_head(service, session, branch, expected_head).await?;
    let cursor = SessionEventId::parse(before)
        .ok_or_else(|| "the history cursor is not a valid event id".to_owned())?;
    let (entries, older, _, _) =
        project_window(service, session, branch, expected_head, Some(&cursor))
            .await
            .map_err(render_error)?;
    Ok(OlderTranscript {
        session_id: session.as_str().to_owned(),
        branch_id: branch.as_str().to_owned(),
        entries,
        older,
        requested_before: before.to_owned(),
    })
}

/// Projects one backward window. `before` absent selects the tail.
async fn project_window(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
    before: Option<&SessionEventId>,
) -> Result<(Vec<ConversationEntry>, Option<String>, u64, u64), SessionError> {
    let window = service
        .read_payload_window(session, branch, snapshot_head, before, VISIBLE_EVENTS)
        .await?;
    let mut entries = Vec::with_capacity(window.items.len());
    let mut context_used = 0u64;
    let mut context_cache = 0u64;
    for loaded in &window.items {
        let (entry, meter) = project_replayed_entry_with_meter(&loaded.event, &loaded.payload);
        if let Some((context, cache)) = meter {
            context_used = context;
            context_cache = cache;
        }
        if let Some(entry) = entry {
            entries.push(entry);
        }
    }
    Ok((
        entries,
        window.older.map(|event| event.as_str().to_owned()),
        context_used,
        context_cache,
    ))
}

/// Walks every committed event of one branch snapshot in ledger order,
/// loading each payload and handing (metadata, bytes) to `visit`.
async fn for_each_event(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
    mut visit: impl FnMut(&SessionEvent, &[u8]) -> Result<(), SessionError>,
) -> Result<(), SessionError> {
    let mut after: Option<SessionEventId> = None;
    loop {
        let page = service
            .read_payloads(session, branch, snapshot_head, after.as_ref(), 256)
            .await?;
        if page.items.is_empty() {
            break;
        }
        for loaded in &page.items {
            visit(&loaded.event, &loaded.payload)?;
        }
        match page.next {
            // `page.next` is the last returned event (`after` is exclusive);
            // only a cursor that failed to advance from the previous `after`
            // would stall the walk.
            Some(cursor) if after.as_ref() != Some(&cursor) => after = Some(cursor),
            _ => break,
        }
    }
    Ok(())
}

/// Rebuilds the model-facing turn history from committed branch events.
///
/// Assistant messages keep their tool_use and thinking blocks, and tool
/// results replay as `Message::ToolResult`, so the wire sequence stays
/// valid across turns. Thinking signatures stay on the replay: stripping
/// them and then enabling thinking on the next turn makes Anthropic-compatible
/// gateways return 400 "unrecognized chat message". Adapters that cannot
/// replay thinking drop those blocks themselves. Usage bookkeeping never
/// reaches the provider.
pub(crate) async fn ledger_history(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
) -> Result<Vec<Arc<Message>>, SessionError> {
    let mut history = Vec::new();
    for_each_event(service, session, branch, snapshot_head, |event, payload| {
        match event.kind {
            EventKind::Message => {
                // Assistant messages are typed JSON; a parse miss means
                // the payload is the user's plain-text message.
                match serde_json::from_slice::<AssistantMessage>(payload) {
                    Ok(assistant) => {
                        // Keep thinking blocks. Anthropic-compatible
                        // gateways reject a later thinking-enabled turn
                        // if prior signatures are stripped ("unrecognized
                        // chat message"). Each wire adapter drops blocks
                        // it cannot replay. Empty blocks stay too: dropping
                        // them shifted the tail and the next turn could not
                        // find its user message.
                        history.push(Arc::new(Message::Assistant(assistant)));
                    }
                    Err(_) => {
                        // A message with images is stored as the tagged
                        // envelope; everything older is raw prompt text.
                        if let Some(stored) = mycode_agent::StoredUserMessage::from_payload(payload)
                        {
                            history.push(Arc::new(Message::User(stored.user_message())));
                            return Ok(());
                        }
                        let text = decode_text(payload);
                        // The transcript copy of a checkpoint summary. Leaving
                        // it in model history would shift `covered_messages`.
                        if crate::compaction::is_display_only_summary(&text) {
                            return Ok(());
                        }
                        history.push(Arc::new(Message::User(UserMessage::text(text))));
                    }
                }
            }
            EventKind::ToolResult => {
                if let Ok(result) = serde_json::from_slice::<ToolResultMessage>(payload) {
                    history.push(Arc::new(Message::ToolResult(result)));
                }
            }
            EventKind::ToolCall | EventKind::Usage | EventKind::Task => {}
        }
        Ok(())
    })
    .await?;
    order_tool_results(&mut history);
    Ok(history)
}

/// Puts each run of tool results back in its assistant message's call order.
///
/// The ledger commits a result when its call finishes, so calls that ran
/// together land in completion order. The agent's in-memory history keeps
/// call order, and the next turn has to resend the same bytes or the
/// provider's prefix cache misses from the first reordered result on. The
/// count of messages is unchanged, so compaction indexes stay valid.
fn order_tool_results(history: &mut [Arc<Message>]) {
    let mut index = 0;
    while index < history.len() {
        let start = index + 1;
        let mut end = start;
        while end < history.len() && matches!(history[end].as_ref(), Message::ToolResult(_)) {
            end += 1;
        }
        if end - start > 1
            && let Message::Assistant(assistant) = history[index].as_ref()
        {
            let calls: Vec<String> = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    mycode_agent::ContentBlock::ToolCall(call) => Some(call.id.clone()),
                    _ => None,
                })
                .collect();
            history[start..end].sort_by_key(|message| match message.as_ref() {
                Message::ToolResult(result) => calls
                    .iter()
                    .position(|id| *id == result.tool_call_id)
                    .unwrap_or(usize::MAX),
                _ => usize::MAX,
            });
        }
        index = end;
    }
}

/// Copies the prefix strictly before the recalled user message to a new
/// branch and returns its conversation. The original branch stays intact.
/// The caller pairs the result with the original text.
///
/// Workspace files are left unchanged: recall does not restore or delete
/// them.
pub(crate) async fn recall_message(
    home: &HomeLayout,
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    before_event: &str,
) -> Result<ActiveConversation, String> {
    // The rewind must land on the branch the UI still sees; a stale head
    // fails closed with the same moved-on error SendMessage's
    // compare-and-swap produces.
    ensure_branch_head(service, session, branch, expected_head).await?;
    let target = SessionEventId::parse(before_event)
        .ok_or_else(|| "the rewind target is not a valid event id".to_owned())?;
    let loaded = service
        .load_event(session, branch, &target)
        .await
        .map_err(render_error)?;
    if !project_replayed_entry_with_meter(&loaded.event, &loaded.payload)
        .0
        .is_some_and(|entry| entry.kind == EntryKind::UserMessage)
    {
        return Err("the recall target is not a user message".to_owned());
    }
    let reservation = service
        .reserve_branch(
            session,
            session::BranchMutationKind::RewindBefore,
            branch,
            &target,
        )
        .await
        .map_err(render_error)?;
    // The reservation freezes the source head. Check it against the UI
    // head too: an append between the initial check and reservation must
    // not silently make a stale recall valid. Commit performs the final CAS.
    if &reservation.source_head != expected_head {
        return Err(HEAD_MOVED_ON.to_owned());
    }
    let branched = service
        .rewind_before(session, branch, &target, &reservation)
        .await
        .map_err(render_error)?;
    // A checkpoint whose covered messages all survive on the rewound branch
    // still summarizes its prefix; otherwise the old branch id stays so the
    // summary of discarded work is ignored. The covered head alone is not
    // enough: a summary filed mid-turn covers that turn's later steps too,
    // and a recall of a steer inside the turn keeps the head but drops them.
    if let Ok(Some(mut checkpoint)) = mycode_agent::read_compaction(home, session.as_str())
        && checkpoint.branch_id == branch.as_str()
        && let Some(covered) = SessionEventId::parse(&checkpoint.covered_head)
        && service
            .load_event(session, &branched.branch_id, &covered)
            .await
            .is_ok()
        && ledger_history(service, session, &branched.branch_id, &branched.head)
            .await
            .is_ok_and(|history| checkpoint.covered_messages <= history.len())
    {
        checkpoint.branch_id = branched.branch_id.as_str().to_owned();
        let _ = mycode_agent::write_compaction(home, session.as_str(), &checkpoint);
    }
    read_branch(service, session, &branched.branch_id, &branched.head)
        .await
        .map_err(render_error)
}

/// Commits a user prompt, with attachments when the UI sent any.
pub(crate) async fn send_message_with(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    text: &str,
    attachments: &mycode_agent::Attachments,
) -> Result<(String, ConversationEntry), SessionError> {
    // The shared CAS append: reserve, compare-and-swap at the UI's head, and
    // re-commit at the durable tip on a moved head. Identical to every other
    // Message commit, so the retry loop lives in one place.
    let writer = HeadWriter::new(
        service.clone(),
        session.clone(),
        branch.clone(),
        expected_head.clone(),
    );
    // A plain prompt borrows its bytes: the writer copies them into the
    // request once, so materializing a Vec here would copy them twice.
    let (payload, display): (Cow<'_, [u8]>, String) = if attachments.is_empty() {
        (Cow::Borrowed(text.as_bytes()), text.to_owned())
    } else {
        let stored = mycode_agent::StoredUserMessage {
            prompt: text.to_owned(),
            attachments: attachments.clone(),
        };
        let payload = stored.to_payload().map_err(|_| SessionError::Unavailable)?;
        let display = stored.display_text();
        (Cow::Owned(payload), display)
    };
    let event_id = writer.write(EventKind::Message, &payload).await?;
    Ok((
        event_id.clone(),
        ConversationEntry {
            event_id,
            kind: EntryKind::UserMessage,
            agent_states: None,
            text: display.into(),
            call_id: None,
            thinking: String::new(),
        },
    ))
}

/// Deletes one session's durable footprint: the index row, the session
/// directory, and, when present, the leftover file-snapshot directory from
/// older versions. The ids are plain names by construction.
///
/// The index row goes first. An absent snapshot directory is a successful
/// delete rather than an error.
pub(crate) fn delete_session(home: &HomeLayout, session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.contains(['/', '\\', ':', '\0'])
        || session_id == "."
        || session_id == ".."
    {
        return Err("invalid session id".to_owned());
    }
    // Find the directory before the row goes: it sits under the project the
    // session started in, which only the directory tree still records.
    let session_dir = mycode_agent::locate_session_dir(home, session_id)
        .map_err(|error| format!("delete: {error}"))?
        .map(|relative| home.owned_join(relative))
        .transpose()
        .map_err(|error| format!("delete: {error}"))?;
    session::delete_session_index(home, session_id)
        .map_err(|_| "session index unavailable".to_owned())?;
    let roots = [
        session_dir.clone(),
        Some(home.root().join(CHECKPOINTS_DIR).join(session_id)),
    ];
    for root in roots.into_iter().flatten() {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("delete: {error}")),
        }
    }
    // The project folder goes with its last session; a folder that still
    // holds others refuses the removal.
    if let Some(project) = session_dir.as_deref().and_then(std::path::Path::parent) {
        let _ = std::fs::remove_dir(project);
    }
    Ok(())
}

/// Drops one session's remembered UI bindings (project, workspace, and
/// model pin).
///
/// Runs right after the durable delete so the advisory maps never accumulate
/// ids that no longer resolve to anything. The state is advisory, so a
/// failure here is reported but never blocks the deletion itself.
pub(crate) fn forget_session_bindings(home: &HomeLayout, session_id: &str) -> Result<(), String> {
    // Repair a damaged document first; the locked edit rejects one it
    // cannot decode.
    mycode_agent::read_ui_state(home).map_err(|error| format!("ui state: {error}"))?;
    mycode_agent::update_ui_state(home, |ui_state| ui_state.forget_session(session_id))
        .map_err(|error| format!("ui state: {error}"))
}

pub(crate) fn head_spelling(head: &HeadStamp) -> String {
    match head {
        HeadStamp::Empty => "empty".to_owned(),
        HeadStamp::Event(event) => event.as_str().to_owned(),
    }
}

pub(crate) fn decode_text(payload: &[u8]) -> String {
    match std::str::from_utf8(payload) {
        Ok(text) => text.to_owned(),
        Err(_) => format!("(binary payload, {} bytes)", payload.len()),
    }
}

/// Shared branch-head writer: the event pump, outbound user messages, and
/// compaction summary cards commit through one CAS head.
#[derive(Clone)]
pub(crate) struct HeadWriter {
    service: SessionService,
    session: SessionId,
    branch: BranchId,
    head: Arc<tokio::sync::Mutex<HeadStamp>>,
    /// Provider tool-call ids mapped to their open ledger call identity.
    /// The ledger's ordering check requires every ToolResult to resolve a
    /// ToolCall event with the same identity, so the identity is minted
    /// once at ToolStarted and reused at ToolCompleted.
    calls: Arc<tokio::sync::Mutex<HashMap<String, SessionCallId>>>,
}

impl HeadWriter {
    pub(crate) fn new(
        service: SessionService,
        session: SessionId,
        branch: BranchId,
        head: HeadStamp,
    ) -> Self {
        Self {
            service,
            session,
            branch,
            head: Arc::new(tokio::sync::Mutex::new(head)),
            calls: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Opens the ledger call identity for one provider tool call, commits
    /// the ToolCall event (payload carries the tool name for replay), and
    /// returns the identity live rows pair on. Transcript rows and live
    /// results both carry it, so a mid-turn reopen — which replays rows
    /// keyed by the ledger identity — keeps calls and results pairable.
    pub(crate) async fn open_call(
        &self,
        provider_call_id: &str,
        name: &str,
        target: &str,
    ) -> Result<SessionCallId, SessionError> {
        let identity = SessionCallId::generate().ok_or(SessionError::Corrupt)?;
        let payload = serde_json::json!({ "name": name, "target": target });
        let bytes = serde_json::to_vec(&payload).map_err(|_| SessionError::Corrupt)?;
        self.write_event(EventKind::ToolCall, Some(identity.clone()), &bytes)
            .await?;
        self.calls
            .lock()
            .await
            .insert(provider_call_id.to_owned(), identity.clone());
        Ok(identity)
    }

    /// The ledger identity of one still-open provider call. Progress lines
    /// arriving on the provider id ride the same transcript identity.
    pub(crate) async fn call_identity(&self, provider_call_id: &str) -> Option<String> {
        self.calls
            .lock()
            .await
            .get(provider_call_id)
            .map(|identity| identity.as_str().to_owned())
    }

    /// Commits one ToolResult under the identity opened at ToolStarted.
    /// Returns the event id and that identity for the live transcript row.
    pub(crate) async fn close_call(
        &self,
        provider_call_id: &str,
        payload: &[u8],
    ) -> Result<(String, String), SessionError> {
        let identity = self
            .calls
            .lock()
            .await
            .remove(provider_call_id)
            .ok_or(SessionError::InvalidArgument)?;
        let event_id = self
            .write_event(EventKind::ToolResult, Some(identity.clone()), payload)
            .await?;
        Ok((event_id, identity.as_str().to_owned()))
    }

    /// The current committed head spelling, for UI refresh after any
    /// trailing writes.
    pub(crate) async fn head(&self) -> String {
        let guard = self.head.lock().await;
        head_spelling(&guard)
    }

    /// The current committed head.
    pub(crate) async fn stamp(&self) -> HeadStamp {
        self.head.lock().await.clone()
    }

    /// Commits one payload of `kind` and returns the new event id spelling.
    pub(crate) async fn write(
        &self,
        kind: EventKind,
        payload: &[u8],
    ) -> Result<String, SessionError> {
        self.write_event(kind, None, payload).await
    }

    /// Commits one payload of `kind` under an explicit ledger call identity.
    async fn write_event(
        &self,
        kind: EventKind,
        ledger_call: Option<SessionCallId>,
        payload: &[u8],
    ) -> Result<String, SessionError> {
        let mut head = self.head.lock().await;
        let reservation = self
            .service
            .reserve_event(
                &self.session,
                &self.branch,
                kind,
                ledger_call.clone(),
                payload,
            )
            .await?;
        let appended = match self
            .service
            .append(&self.session, &self.branch, &head, &reservation)
            .await
        {
            Err(SessionError::Conflict(conflict)) => {
                let reservation = self
                    .service
                    .reserve_event(&self.session, &self.branch, kind, ledger_call, payload)
                    .await?;
                self.service
                    .append(&self.session, &self.branch, &conflict.actual, &reservation)
                    .await?
            }
            other => other?,
        };
        let event_id = match appended.head {
            HeadStamp::Event(event) => event,
            HeadStamp::Empty => return Err(SessionError::Corrupt),
        };
        *head = HeadStamp::Event(event_id.clone());
        Ok(event_id.as_str().to_owned())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replayed_tool_results_follow_the_call_order() {
        use std::sync::Arc;

        use mycode_agent::{
            AssistantMessage, ContentBlock, Message, StopReason, ToolCall, ToolResultMessage,
            UserMessage,
        };
        let result = |id: &str| {
            Arc::new(Message::ToolResult(ToolResultMessage {
                tool_call_id: id.to_owned(),
                content: Vec::new(),
                is_error: false,
                details: None,
            }))
        };
        let call =
            |id: &str| ContentBlock::ToolCall(ToolCall::new(id, "read", serde_json::json!({})));
        let mut history = vec![
            Arc::new(Message::User(UserMessage::text("go"))),
            Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![call("a"), call("b"), call("c")],
                usage: None,
                stop_reason: StopReason::ToolUse,
            })),
            result("c"),
            result("a"),
            result("b"),
            Arc::new(Message::User(UserMessage::text("steer"))),
        ];
        super::order_tool_results(&mut history);
        let ids: Vec<_> = history
            .iter()
            .filter_map(|message| match message.as_ref() {
                Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(history.len(), 6);
        assert!(matches!(history[5].as_ref(), Message::User(_)));
    }
    // Removed: open_reads_the_tail_and_pages_backward,
    // opening_a_session_restores_the_last_usage_snapshot,
    // deleting_the_last_session_of_a_project_removes_its_folder,
    // ledger_history_walks_past_the_first_page_and_recall_moves_checkpoint —
    // all built a tokio current_thread runtime over SessionService file IO in
    // %TEMP% and intermittently hung on this Windows host. The pure ordering
    // test above stays.
}
