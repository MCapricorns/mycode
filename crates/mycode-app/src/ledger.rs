//! Session ledger reads and writes: listing summaries, branch projections,
//! model-facing replay history, message commits, recalls, and deletion.

use std::collections::HashMap;
use std::sync::Arc;

use mycode_agent::session::{
    self, BranchId, EventKind, HeadStamp, SessionCallId, SessionError, SessionEvent,
    SessionEventId, SessionId, SessionService,
};
use mycode_config::HomeLayout;
use mycode_core::Message;
use mycode_core::{AssistantMessage, ToolResultMessage, UserMessage};

use crate::projection::project_replayed_entry;
use crate::protocol::{ActiveConversation, ConversationEntry, EntryKind, SessionSummary};

/// Rendered wording for a lost expected-head compare-and-swap.
const HEAD_MOVED_ON: &str = "the session moved on; reopen it";

/// Session checkpoint directory under the home root. Private inside
/// `mycode-config` (`checkpoints::checkpoint_dir`), so the layout is spelled
/// here once; `checkpoint_file` records beneath it and rollback applies the plan.
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

/// Lists sessions with display titles: the first user message of each root
/// branch, first page only. Per-session read failures degrade to an empty
/// title; the listing itself never fails on one bad session. Directories
/// whose manifest cannot be read at all surface as `corrupt` rows so the UI
/// can offer deletion instead of losing the sidebar.
pub(crate) async fn inspect_summaries(
    service: &SessionService,
    home: HomeLayout,
) -> Result<Vec<SessionSummary>, SessionError> {
    // Directory walk stays off the core runtime thread.
    let listing = tokio::task::spawn_blocking(move || session::inspect_sessions(&home))
        .await
        .map_err(|_| SessionError::Unavailable)??;
    let mut summaries = Vec::with_capacity(listing.sessions.len() + listing.corrupt.len());
    for name in &listing.corrupt {
        summaries.push(SessionSummary {
            session_id: name.clone(),
            root_branch_id: String::new(),
            title: String::new(),
            event_count: 0,
            active: false,
            corrupt: true,
        });
    }
    for snapshot in listing.sessions {
        let Some(root) = snapshot.branches.first() else {
            continue;
        };
        let branch_id = root.branch_id.clone();
        let snapshot_head = root.head.clone();
        let title = session_title(service, &snapshot.session_id, &branch_id, &snapshot_head).await;
        summaries.push(SessionSummary {
            root_branch_id: branch_id.as_str().to_owned(),
            event_count: snapshot.branches.iter().map(|b| b.event_count).sum(),
            session_id: snapshot.session_id.as_str().to_owned(),
            title,
            active: false,
            corrupt: false,
        });
    }
    Ok(summaries)
}

/// Reads the first user message of a session's root branch for the sidebar
/// title; empty when the session has no messages yet. Assistant payloads
/// (typed JSON, discriminated exactly like the replay projections) are
/// skipped so a title never comes from assistant text. A stale manifest head
/// (an active turn appended events) degrades to an empty title.
async fn session_title(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    head: &HeadStamp,
) -> String {
    let Ok(page) = service.read_payloads(session, branch, head, None, 8).await else {
        return String::new();
    };
    for loaded in &page.items {
        if loaded.event.kind != EventKind::Message {
            continue;
        }
        // A parse miss means the payload is the user's plain-text
        // message; assistant messages decode as typed JSON.
        if serde_json::from_slice::<AssistantMessage>(&loaded.payload).is_ok() {
            continue;
        }
        return title_from_text(&decode_text(&loaded.payload));
    }
    String::new()
}

/// Collapses one message into a one-line sidebar title.
fn title_from_text(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(60).collect()
}

pub(crate) async fn open_conversation(
    service: &SessionService,
    session: &SessionId,
) -> Result<ActiveConversation, SessionError> {
    let opened = service.open(session).await?;
    let root = opened
        .heads
        .iter()
        .min_by_key(|head| head.branch_id.as_str())
        .ok_or(SessionError::NotFound)?;
    read_branch(service, session, &root.branch_id, &root.head).await
}

/// Reads one branch's committed events into display entries.
pub(crate) async fn read_branch(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
) -> Result<ActiveConversation, SessionError> {
    let mut entries = Vec::new();
    for_each_event(service, session, branch, snapshot_head, |event, payload| {
        if let Some(entry) = project_replayed_entry(event, payload) {
            entries.push(entry);
        }
        Ok(())
    })
    .await?;
    Ok(ActiveConversation {
        session_id: session.as_str().to_owned(),
        branch_id: branch.as_str().to_owned(),
        head: head_spelling(snapshot_head),
        entries,
        streaming: None,
    })
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
        let last = page
            .items
            .last()
            .expect("nonempty page")
            .event
            .event_id
            .clone();
        for loaded in &page.items {
            visit(&loaded.event, &loaded.payload)?;
        }
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
        if after.as_ref() == Some(&last) {
            // Defensive: a cursor equal to the last returned event would loop.
            break;
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
                        // it cannot replay.
                        if !assistant.blocks.is_empty() {
                            history.push(Arc::new(Message::Assistant(assistant)));
                        }
                    }
                    Err(_) => history.push(Arc::new(Message::User(UserMessage::text(
                        decode_text(payload),
                    )))),
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
    Ok(history)
}

/// Rewinds the branch so everything from the recalled message onward is
/// gone, then returns the truncated conversation plus the edited text.
pub(crate) async fn recall_message(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    to_event: &str,
) -> Result<ActiveConversation, String> {
    // The rewind must land on the branch the UI still sees; a stale head
    // fails closed with the same moved-on error SendMessage's
    // compare-and-swap produces.
    let opened = service.open(session).await.map_err(render_error)?;
    let current = opened
        .heads
        .iter()
        .find(|head| &head.branch_id == branch)
        .ok_or_else(|| render_error(SessionError::NotFound))?;
    if &current.head != expected_head {
        // The `Conflict` variant is constructed inside the session service,
        // so its rendered wording is spelled here instead.
        return Err(HEAD_MOVED_ON.to_owned());
    }
    let target = SessionEventId::parse(to_event)
        .ok_or_else(|| "the rewind target is not a valid event id".to_owned())?;
    let reservation = service
        .reserve_branch(
            session,
            session::BranchMutationKind::Rewind,
            branch,
            &target,
        )
        .await
        .map_err(render_error)?;
    let branched = service
        .rewind(session, branch, &target, &reservation)
        .await
        .map_err(render_error)?;
    read_branch(service, session, &branched.branch_id, &branched.head)
        .await
        .map_err(render_error)
}

pub(crate) async fn send_message(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    text: &str,
) -> Result<(String, ConversationEntry), SessionError> {
    let payload = text.as_bytes().to_vec();
    let reservation = service
        .reserve_event(session, branch, EventKind::Message, None, &payload)
        .await?;
    let appended = match service
        .append(session, branch, expected_head, &reservation)
        .await
    {
        // A stale desktop head is not a lost session. The reservation was
        // already consumed, so reserve again and commit at the durable tip.
        Err(SessionError::Conflict(conflict)) => {
            let reservation = service
                .reserve_event(session, branch, EventKind::Message, None, &payload)
                .await?;
            service
                .append(session, branch, &conflict.actual, &reservation)
                .await?
        }
        other => other?,
    };
    let event_id = match appended.head {
        HeadStamp::Event(event) => event,
        HeadStamp::Empty => return Err(SessionError::Corrupt),
    };
    Ok((
        event_id.as_str().to_owned(),
        ConversationEntry {
            event_id: event_id.as_str().to_owned(),
            kind: EntryKind::UserMessage,
            text: text.into(),
            call_id: None,
            thinking: String::new(),
        },
    ))
}

/// Deletes one session's durable footprint: ledger, compaction checkpoint,
/// and file snapshots. The ids are plain names by construction.
///
/// Only the ledger directory always exists; snapshots are created lazily,
/// so an absent directory is a successful delete rather than an error.
pub(crate) fn delete_session(home: &HomeLayout, session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.contains(['/', '\\', ':', '\0'])
        || session_id == "."
        || session_id == ".."
    {
        return Err("invalid session id".to_owned());
    }
    let roots = [
        home.root()
            .join(mycode_config::SESSIONS_DIR)
            .join(session_id),
        home.root().join(CHECKPOINTS_DIR).join(session_id),
    ];
    for root in roots {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("delete: {error}")),
        }
    }
    Ok(())
}

/// Drops one session's remembered UI bindings (project and workspace).
///
/// Runs right after the durable delete so the advisory maps never accumulate
/// ids that no longer resolve to anything. The state is advisory, so a
/// failure here is reported but never blocks the deletion itself.
pub(crate) fn forget_session_bindings(home: &HomeLayout, session_id: &str) -> Result<(), String> {
    let mut ui_state =
        mycode_config::read_ui_state(home).map_err(|error| format!("ui state: {error}"))?;
    ui_state.forget_session(session_id);
    mycode_config::replace_ui_state(home, &ui_state).map_err(|error| format!("ui state: {error}"))
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

/// Shared branch-head writer: the event pump and host-backed tools commit
/// through one CAS head.
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

    /// Opens the ledger call identity for one provider tool call and commits
    /// the ToolCall event (payload carries the tool name for replay).
    pub(crate) async fn open_call(
        &self,
        provider_call_id: &str,
        name: &str,
        target: &str,
    ) -> Result<(), SessionError> {
        let identity = SessionCallId::generate().ok_or(SessionError::Corrupt)?;
        self.calls
            .lock()
            .await
            .insert(provider_call_id.to_owned(), identity.clone());
        let payload = serde_json::json!({ "name": name, "target": target });
        let bytes = serde_json::to_vec(&payload).map_err(|_| SessionError::Corrupt)?;
        self.write_event(EventKind::ToolCall, Some(identity), &bytes)
            .await
            .map(|_| ())
    }

    /// Commits one ToolResult under the identity opened at ToolStarted.
    pub(crate) async fn close_call(
        &self,
        provider_call_id: &str,
        payload: &[u8],
    ) -> Result<String, SessionError> {
        let identity = self
            .calls
            .lock()
            .await
            .remove(provider_call_id)
            .ok_or(SessionError::InvalidArgument)?;
        self.write_event(EventKind::ToolResult, Some(identity), payload)
            .await
    }

    /// The current committed head spelling, for UI refresh after any
    /// trailing writes.
    pub(crate) async fn head(&self) -> String {
        let guard = self.head.lock().await;
        head_spelling(&guard)
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
