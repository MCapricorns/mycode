//! Auto-compaction: checkpoint summaries before each provider request.
//!
//! Trigger is 90% of the usable context window (95% of the model window, same
//! safety margin as Codex), measured as [`sizing`] describes. The tail keeps
//! the newest messages, up to ~20k tokens in a large window and less in a
//! small one, without splitting a tool-call pair. The head is summarized with
//! a handoff prompt (goals, files, decisions, errors, next steps), and the
//! user's own messages from it follow the summary word for word, the newest
//! that fit, as Codex keeps them: in a long tool run the tail is all tool
//! output, and a summary alone retold the user's requests. A request
//! the provider refuses as over its window is compacted at once and sent
//! again ([`CompactMode::Overflow`]). The session ledger is never rewritten;
//! only the in-memory request history shrinks.

mod echo;
mod sizing;

use std::fmt::Write as _;
use std::sync::Arc;

use mycode_agent::WireProvider;
use mycode_agent::{Message, Provider as _, Request, StreamEvent};
use tokio_util::sync::CancellationToken;

use self::echo::{split_echo, user_echo, with_echo};
use self::sizing::{Budget, Tally, message_tokens, text_tokens};
pub(crate) use self::sizing::{
    Calibration, Measure, TokenScale, estimate_prefix_tokens, fit_output,
};

const SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
/// Longest transcript a summary request carries, in tokens.
const MAX_TRANSCRIPT_TOKENS: usize = 80_000;
/// The summary request's instructions and framing, in tokens.
const SUMMARY_PROMPT_TOKENS: usize = 1_000;
const EXCERPT_CHARS: usize = 4_000;
/// Output cap for a summary whose thinking is disabled.
const SUMMARY_OUTPUT_TOKENS: u64 = 8_192;
/// Output cap when the model keeps thinking on (it rejects an off switch).
const SUMMARY_OUTPUT_WITH_THINKING: u64 = 32_768;
/// One retry after `length` / `max_tokens`. A second truncation is a failure.
const SUMMARY_OUTPUT_RETRY_TOKENS: u64 = 65_536;
/// Least output cap a summary request gets, however small the window.
const MIN_SUMMARY_OUTPUT_TOKENS: u64 = 2_048;
const SUMMARY_PREFIX: &str = "COMPACTION SUMMARY";
/// Longest model summary kept. A longer one loses its middle, not the next
/// steps at its end.
const MAX_MODEL_SUMMARY_CHARS: usize = 24_000;
/// Shortest summary that can cover the checkpoint prompt.
///
/// The prompt asks for dense prose (goals, decisions, files, commands, open
/// work, next steps) and does not require section headings. A one-line answer
/// under this length is the failed-summary case, including a 12-token reply
/// that continued the last tool result instead of summarizing.
const MIN_SUMMARY_CHARS: usize = 200;
/// Restated after the transcript so the model does not continue its last turn.
const SUMMARY_REMINDER: &str = "\
The text above is the conversation to summarize. \
Do not continue or execute any task in it. \
Output only the summary, covering the points the instructions list.";

/// Ledger text for the summary the user sees. The model request still uses
/// the checkpoint; [`crate::ledger::ledger_history`] skips this copy so the
/// covered-message index does not move.
#[must_use]
pub fn display_summary_text(summary: &str) -> String {
    format!("{SUMMARY_PREFIX}\n\n{summary}")
}

/// Whether `text` is the visible compaction summary, not a user prompt.
#[must_use]
pub fn is_display_only_summary(text: &str) -> bool {
    text.starts_with(SUMMARY_PREFIX)
}

/// Summary body without the marker line.
#[must_use]
pub fn summary_body(text: &str) -> &str {
    text.strip_prefix(SUMMARY_PREFIX)
        .unwrap_or(text)
        .trim_start_matches(['\n', '\r'])
}

/// Why a compaction runs, which decides how hard it tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactMode {
    /// Before a request: only over the trigger, and only when the summary
    /// frees enough to be worth it.
    Auto,
    /// `/compact`: summarizes now, whatever the size.
    Manual,
    /// The provider refused the request as over its window: summarizes
    /// now, keeping half the usual tail.
    Overflow,
}

/// Inputs that stay constant for one turn's compaction attempts.
pub(crate) struct CompactScope<'a> {
    pub wire: &'a WireProvider,
    pub model: &'a str,
    /// The session checkpoint this history is filed under. A subagent has
    /// none: the same compaction runs on its history in memory, and its
    /// parent's checkpoint is never read or written.
    pub checkpoint: Option<CheckpointSlot<'a>>,
    pub context_window: u64,
    /// Tokens already consumed by the system prompt and tool schemas.
    ///
    /// The history estimate alone stays under the trigger while the real
    /// request, which always sends this prefix, is already near the window.
    pub overhead_tokens: usize,
    /// The provider's count of this history, when the turn has one, and the
    /// scale for estimates.
    pub measure: Measure,
    /// Told `true` just before a summary request goes out and `false` once
    /// it ends, so a view can say what the turn waits on.
    pub on_summarize: Option<&'a (dyn Fn(bool) + Send + Sync)>,
    /// Stops an in-flight summary request. A cancelled attempt fails and
    /// leaves the history unchanged.
    pub cancel: &'a CancellationToken,
}

/// Where one session branch keeps its checkpoint, and the ledger head the
/// history was read at.
#[derive(Clone, Copy)]
pub(crate) struct CheckpointSlot<'a> {
    pub home: &'a mycode_agent::HomeLayout,
    pub session_id: &'a str,
    pub branch_id: &'a str,
    pub head: &'a str,
}

/// [`CheckpointSlot`] with owned fields, for request hooks that outlive the
/// turn's borrows.
#[derive(Clone)]
pub(crate) struct CheckpointKey {
    pub home: mycode_agent::HomeLayout,
    pub session_id: String,
    pub branch_id: String,
    pub head: String,
}

impl CheckpointKey {
    fn slot(&self) -> CheckpointSlot<'_> {
        CheckpointSlot {
            home: &self.home,
            session_id: &self.session_id,
            branch_id: &self.branch_id,
            head: &self.head,
        }
    }
}

/// What a turn's request hooks need to compact the requests it sends.
#[derive(Clone)]
pub(crate) struct RequestCompactor {
    pub wire: WireProvider,
    pub model: String,
    /// `None` for a subagent; see [`CompactScope::checkpoint`].
    pub checkpoint: Option<CheckpointKey>,
    pub context_window: u64,
    pub calibration: Calibration,
    /// See [`CompactScope::on_summarize`].
    pub on_summarize: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    /// The turn's token: it stops a summary the user no longer waits for,
    /// and the agent sees the cancel right after the hook.
    pub cancel: CancellationToken,
}

impl RequestCompactor {
    /// Compacts `request` as `mode` says and lowers its output cap to what
    /// the window leaves. Returns the summary when it summarized.
    pub(crate) async fn compact(&self, request: &mut Request, mode: CompactMode) -> Option<String> {
        if mode == CompactMode::Overflow {
            self.calibration.overflowed(self.context_window);
        }
        let overhead = estimate_prefix_tokens(&request.system_prompt, &request.tools);
        let scope = CompactScope {
            wire: &self.wire,
            model: &self.model,
            checkpoint: self.checkpoint.as_ref().map(CheckpointKey::slot),
            context_window: self.context_window,
            overhead_tokens: overhead,
            measure: self.calibration.measure(&request.messages),
            on_summarize: self.on_summarize.as_deref(),
            cancel: &self.cancel,
        };
        let history = std::mem::take(&mut request.messages);
        let compacted = compact_history(&scope, history, mode).await;
        request.messages = compacted.messages;
        // Its reply measures this request for the next one.
        self.calibration.sent(&request.messages, overhead);
        request.max_output_tokens = fit_output(
            request.max_output_tokens,
            self.context_window,
            compacted.tokens,
        );
        compacted.summary
    }
}

/// What a compaction attempt did with the checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompactStatus {
    /// A new checkpoint was written.
    Wrote,
    /// The history was summarized in memory only: it already opened with a
    /// summary, so its indexes are not ledger indexes, or it has no
    /// checkpoint slot at all.
    Replaced,
    /// No checkpoint was written.
    Unchanged,
    /// A checkpoint already covers this exact head.
    Covered,
    /// The attempt failed and no checkpoint was written.
    Failed(String),
}

/// History after one compaction attempt, plus whether a checkpoint was written.
pub(crate) struct Compacted {
    pub messages: Vec<Arc<Message>>,
    pub status: CompactStatus,
    /// Summary text when this attempt summarized ([`CompactStatus::Wrote`]
    /// or [`CompactStatus::Replaced`]). Manual and automatic compaction both
    /// surface this in the transcript.
    pub summary: Option<String>,
    /// Size of a request carrying `messages`, prefix included, in provider
    /// tokens.
    pub tokens: usize,
}

/// Compacts history before a provider request. Failures degrade to the
/// original history so a turn never dies on housekeeping.
pub(crate) async fn compact_history(
    scope: &CompactScope<'_>,
    mut history: Vec<Arc<Message>>,
    mode: CompactMode,
) -> Compacted {
    let scale = scope.measure.scale;
    let overhead = scale.apply(scope.overhead_tokens);
    let mut budget = Budget::new(scope.context_window, overhead);
    let threshold = match mode {
        CompactMode::Auto => budget.threshold,
        CompactMode::Manual | CompactMode::Overflow => 0,
    };
    if mode == CompactMode::Overflow {
        // The estimates already missed once on this request.
        budget.tail /= 2;
    }
    let head = scope.checkpoint.map_or("", |slot| slot.head);
    let prior = match scope.checkpoint {
        Some(slot) => {
            let read_home = slot.home.clone();
            let read_session = slot.session_id.to_owned();
            let read_branch = slot.branch_id.to_owned();
            tokio::task::spawn_blocking(move || {
                stored_checkpoint(&read_home, &read_session, &read_branch)
            })
            .await
            .ok()
            .flatten()
        }
        None => None,
    };
    // A measured size describes `history` as given; a stitched copy is
    // estimated.
    let mut tokens = scope
        .measure
        .tokens
        .unwrap_or_else(|| request_tokens(&history, overhead, scale));
    let unchanged = |messages: Vec<Arc<Message>>, tokens: usize| Compacted {
        messages,
        status: CompactStatus::Unchanged,
        summary: None,
        tokens,
    };
    // `/compact` writes a checkpoint the next request must use even while the
    // raw ledger is still under the auto threshold. Apply that checkpoint
    // before the threshold check, then judge the threshold on the stitched
    // history. A manual compact still summarizes (or reports that this head
    // is already covered) instead of stopping at the previous stitch.
    let starts_with_summary = history
        .first()
        .is_some_and(|message| is_summary_message(message));
    // A checkpoint whose index does not land on this ledger replay was cut
    // from some other history. Its index then sliced the wrong messages out
    // of the next summary, and its summary told of work this one lacks.
    let prior = prior.filter(|checkpoint| {
        starts_with_summary || covers_a_prefix(&history, checkpoint.covered_messages)
    });
    if mode != CompactMode::Manual
        && !starts_with_summary
        && let Some(checkpoint) = prior.as_ref()
        && checkpoint_matches(
            &history,
            checkpoint,
            head,
            budget.threshold,
            overhead,
            scale,
        )
    {
        let stitched = with_summary(&checkpoint.summary, &history[checkpoint.covered_messages..]);
        let stitched_tokens = request_tokens(&stitched, overhead, scale);
        if compaction_split(&stitched, stitched_tokens, threshold, budget.tail, scale).is_none() {
            return unchanged(stitched, stitched_tokens);
        }
        // Summary + uncovered tail is still over the window. When the raw
        // ledger itself splits, fall through and persist a new checkpoint
        // against that ledger. When only the stitched copy is over, shrink
        // it in memory: its indexes are not ledger indexes.
        if compaction_split(&history, tokens, threshold, budget.tail, scale).is_none() {
            history = stitched;
            tokens = stitched_tokens;
        }
    }
    // A manual `/compact` on a session that still fits the tail budget
    // summarizes everything instead of reporting nothing to compact: the
    // user asked for the shrink now, not at the auto threshold. The covered
    // range is the whole ledger, so the next request replays the summary
    // plus whatever arrives after it. A request the provider refused while
    // it seemed to fit the tail goes the same way.
    let head_end = match compaction_split(&history, tokens, threshold, budget.tail, scale) {
        Some(head_end) => head_end,
        None if mode != CompactMode::Auto && history.len() >= 2 => history.len(),
        None => return unchanged(history, tokens),
    };
    // `covered_messages` indexes a ledger replay, which has no summary
    // prefix. Once a summary has replaced that prefix, the same index
    // points into the tail and would drop messages that must stay. That
    // leading summary is also the one to carry forward: a turn that
    // compacted in memory holds a newer summary than the disk checkpoint.
    let leading = leading_summary(&history);
    let already_summarized = leading.is_some();
    let covered = if already_summarized {
        None
    } else {
        prior.as_ref().map(|checkpoint| checkpoint.covered_messages)
    };
    let prior_summary = leading
        .as_deref()
        .or_else(|| prior.as_ref().map(|checkpoint| checkpoint.summary.as_str()));
    // The previous summary already stands in for `history[..covered]`.
    // Sending that prefix again makes a later `/compact` re-bill the same
    // leading messages on top of the old summary.
    let head_messages = transcript_head(&history, head_end, covered);
    let (prior_summary, prior_echo) = match prior_summary {
        Some(text) => {
            let (summary, echo) = split_echo(text);
            (Some(summary), echo)
        }
        None => (None, Vec::new()),
    };
    let echo = user_echo(&prior_echo, head_messages, budget.echo, scale);
    // A summary about as large as what it replaces frees nothing. In a
    // small window the next request was over the trigger again and every
    // request summarized the summary once more.
    let replaced: usize = history[..head_end]
        .iter()
        .map(|message| scale.apply(message_tokens(message)))
        .sum();
    let echoed: usize = echo.iter().map(|text| scale.apply(text_tokens(text))).sum();
    if mode == CompactMode::Auto && replaced < budget.summary_tokens() + echoed + budget.min_gain()
    {
        return unchanged(history, tokens);
    }
    // `/compact` on a head this checkpoint already covers would summarize the
    // same prefix again (same replaced count, same tail). Say so instead.
    // A session with fewer than two messages has nothing to summarize and
    // already returned `Unchanged` above; a longer session that fits the
    // tail is summarized in full below.
    if mode == CompactMode::Manual
        && let Some(checkpoint) = prior.as_ref()
        && checkpoint.covered_head == head
        && checkpoint.covered_messages > 0
    {
        return Compacted {
            messages: history,
            status: CompactStatus::Covered,
            summary: None,
            tokens,
        };
    }
    let spec = SummarySpec::new(scope.model, scope.context_window, budget.summary_words);
    // The previous summary goes to the summarizer without the user messages
    // kept after it: those stay with the new summary as they are, and the
    // old summary already tells what came of them.
    let transcript =
        compaction_transcript(prior_summary, head_messages, spec.transcript_tokens, scale);
    let failed = |messages: Vec<Arc<Message>>, message: String| Compacted {
        messages,
        status: CompactStatus::Failed(message),
        summary: None,
        tokens,
    };
    // The summary can run for minutes. A cancel drops the attempt (and its
    // stream, which cancels the request) instead of holding the turn hostage.
    if let Some(notify) = scope.on_summarize {
        notify(true);
    }
    let summarized = tokio::select! {
        biased;
        () = scope.cancel.cancelled() => Ok(Err("compaction cancelled".to_owned())),
        summarized = tokio::time::timeout(
            SUMMARY_TIMEOUT,
            summarize_transcript(scope.wire, &transcript, &spec, scope.cancel),
        ) => summarized,
    };
    if let Some(notify) = scope.on_summarize {
        notify(false);
    }
    let summary = match summarized {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => return failed(history, message),
        Err(_) => return failed(history, "summary timed out".to_owned()),
    };
    // The card shows the model's summary; the request and the checkpoint
    // also carry the user's words.
    let shown = summary;
    let summary = with_echo(&shown, &echo);
    let compacted = with_summary(&summary, &history[head_end..]);
    let compacted_tokens = request_tokens(&compacted, overhead, scale);
    // A history that already starts with a summary is not the ledger
    // replay. Persisting `head_end` would store an index into that shorter
    // vector, and the next open of this head would slice the ledger with it.
    // A turn files that summary once its ledger is complete
    // ([`persist_turn_summary`]).
    let slot = match scope.checkpoint {
        Some(slot) if !already_summarized => slot,
        _ => {
            return Compacted {
                messages: compacted,
                status: CompactStatus::Replaced,
                summary: Some(shown),
                tokens: compacted_tokens,
            };
        }
    };
    if scope.cancel.is_cancelled() {
        return failed(history, "compaction cancelled".to_owned());
    }
    let checkpoint = mycode_agent::CompactionCheckpoint {
        format_version: mycode_agent::COMPACTION_FORMAT_VERSION,
        kind: mycode_agent::COMPACTION_KIND.to_owned(),
        session_id: slot.session_id.to_owned(),
        branch_id: slot.branch_id.to_owned(),
        covered_head: slot.head.to_owned(),
        covered_messages: head_end,
        summary,
        model: scope.model.to_owned(),
        created_at_unix: crate::unix_now().as_secs(),
    };
    let write_home = slot.home.clone();
    let write_session = slot.session_id.to_owned();
    let written = tokio::task::spawn_blocking(move || {
        mycode_agent::write_compaction(&write_home, &write_session, &checkpoint)
    })
    .await
    .map_err(|error| format!("checkpoint write task failed: {error}"));
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return failed(history, format!("checkpoint write failed: {error}")),
        Err(message) => return failed(history, message),
    }
    Compacted {
        messages: compacted,
        status: CompactStatus::Wrote,
        summary: Some(shown),
        tokens: compacted_tokens,
    }
}

/// Size of a request carrying `history` after a prefix of `overhead`
/// provider tokens.
fn request_tokens(history: &[Arc<Message>], overhead: usize, scale: TokenScale) -> usize {
    history
        .iter()
        .map(|message| scale.apply(message_tokens(message)))
        .fold(overhead, usize::saturating_add)
}

fn with_summary(summary: &str, tail: &[Arc<Message>]) -> Vec<Arc<Message>> {
    let mut compacted = Vec::with_capacity(tail.len() + 1);
    compacted.push(Arc::new(Message::User(mycode_agent::UserMessage::text(
        request_summary_text(summary),
    ))));
    compacted.extend(tail.iter().cloned());
    compacted
}

/// The summary frame a request sees. Same `COMPACTION SUMMARY` marker the
/// ledger card carries so [`is_summary_message`] still matches, plus an
/// ownership line: a resumed model that reads the checkpoint as *another*
/// agent's notes re-verifies finished work from scratch instead of
/// continuing. The ledger card itself keeps [`display_summary_text`], which
/// is user-facing and needs no framing.
fn request_summary_text(summary: &str) -> String {
    format!(
        "{SUMMARY_PREFIX}\n\n\
         This is a compressed checkpoint of your own earlier work in this \
         session. Everything it lists as done was done by you — trust it and \
         continue from the next steps; re-verify a file only when something \
         the summary claims is missing.\n\n\
         {summary}"
    )
}

/// After a turn, writes the summary its request hook produced in memory, so
/// the next turn resumes from it instead of re-summarizing the ledger.
///
/// That summary was cut from a stitched history, whose indexes are not
/// ledger indexes. It is stored only when every message after it matches
/// the end of the ledger replay, role and text alike; `covered_messages` is
/// then the ledger length minus that tail. Any mismatch keeps the older
/// checkpoint, which stays correct, just less recent.
pub(crate) fn persist_turn_summary(
    home: &mycode_agent::HomeLayout,
    target: &TurnCheckpoint<'_>,
    ledger: &[Arc<Message>],
    memory: &[Arc<Message>],
) -> bool {
    let Some(summary) = leading_summary(memory) else {
        return false;
    };
    let prior = stored_checkpoint(home, target.session_id, target.branch_id);
    if prior
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.summary == summary)
    {
        return false;
    }
    let tail = &memory[1..];
    let Some(covered) = ledger.len().checked_sub(tail.len()) else {
        return false;
    };
    if covered == 0
        || ledger
            .get(covered)
            .is_some_and(|message| is_tool_result(message))
    {
        return false;
    }
    let same = |left: &Message, right: &Message| {
        std::mem::discriminant(left) == std::mem::discriminant(right)
            && message_text(left) == message_text(right)
    };
    if !tail
        .iter()
        .zip(&ledger[covered..])
        .all(|(left, right)| same(left, right))
    {
        return false;
    }
    let checkpoint = mycode_agent::CompactionCheckpoint {
        format_version: mycode_agent::COMPACTION_FORMAT_VERSION,
        kind: mycode_agent::COMPACTION_KIND.to_owned(),
        session_id: target.session_id.to_owned(),
        branch_id: target.branch_id.to_owned(),
        covered_head: target.head.to_owned(),
        covered_messages: covered,
        summary,
        model: target.model.to_owned(),
        created_at_unix: crate::unix_now().as_secs(),
    };
    mycode_agent::write_compaction(home, target.session_id, &checkpoint).is_ok()
}

/// Where [`persist_turn_summary`] files its checkpoint: the session branch
/// and the ledger head it was checked against.
pub(crate) struct TurnCheckpoint<'a> {
    pub session_id: &'a str,
    pub branch_id: &'a str,
    pub head: &'a str,
    pub model: &'a str,
}

/// Whether `first`, the opening message of a turn's memory, is a summary
/// the session's stored checkpoint does not hold yet. Every turn of a
/// compacted session opens with the stored summary, so this is the cheap
/// check before [`persist_turn_summary`] replays the ledger.
pub(crate) fn opens_with_unstored_summary(
    home: &mycode_agent::HomeLayout,
    session_id: &str,
    branch_id: &str,
    first: Option<&Message>,
) -> bool {
    let Some(summary) = first.and_then(summary_in) else {
        return false;
    };
    !stored_checkpoint(home, session_id, branch_id)
        .is_some_and(|checkpoint| checkpoint.summary == summary)
}

/// The checkpoint stored for `session_id`, when it belongs to `branch_id`.
fn stored_checkpoint(
    home: &mycode_agent::HomeLayout,
    session_id: &str,
    branch_id: &str,
) -> Option<mycode_agent::CompactionCheckpoint> {
    mycode_agent::read_compaction(home, session_id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint.branch_id == branch_id)
}

/// The summary a history opens with, without its wire frame, or `None`
/// when the history is a plain ledger replay.
fn leading_summary(history: &[Arc<Message>]) -> Option<String> {
    history.first().and_then(|message| summary_in(message))
}

/// The summary `message` carries, without its wire frame, or `None` when
/// it is not a summary.
fn summary_in(message: &Message) -> Option<String> {
    if !is_summary_message(message) {
        return None;
    }
    let text = message_text(message);
    let frame = request_summary_text("");
    Some(match text.strip_prefix(frame.as_str()) {
        Some(body) => body.to_owned(),
        None => summary_body(&text).to_owned(),
    })
}

fn message_text(message: &Message) -> String {
    match message {
        Message::User(user) => blocks_text(&user.content),
        Message::Assistant(assistant) => assistant_transcript_text(&assistant.blocks),
        Message::ToolResult(result) => {
            let body = blocks_text(&result.content);
            format!("tool_result {} {body}", result.tool_call_id)
        }
    }
}

/// Assistant text for the summary transcript.
///
/// Thinking blocks are the stored form of provider reasoning
/// (`reasoning_content` on OpenAI-compatible wires, Anthropic thinking
/// blocks) and are omitted. A leading inline `<think>...</think>` in the
/// assistant text is omitted too. A later mention of the tag stays, and
/// user or tool text is left unchanged.
fn assistant_transcript_text(blocks: &[mycode_agent::ContentBlock]) -> String {
    let raw = blocks_text(blocks);
    let stripped = strip_leading_think(&raw);
    if stripped.len() == raw.len() {
        raw
    } else {
        stripped.to_owned()
    }
}

/// Drops one or more leading `<think>...</think>` blocks.
///
/// An unclosed opening tag is kept, so a transcript that merely mentions
/// the tag is not eaten.
fn strip_leading_think(text: &str) -> &str {
    let mut rest = text;
    let mut removed = false;
    loop {
        let trimmed = rest.trim_start();
        let Some(inner) = trimmed.strip_prefix("<think>") else {
            return if removed { trimmed } else { text };
        };
        let Some(end) = inner.find("</think>") else {
            return if removed { trimmed } else { text };
        };
        rest = &inner[end + "</think>".len()..];
        removed = true;
    }
}

fn blocks_text(blocks: &[mycode_agent::ContentBlock]) -> String {
    let mut out = String::new();
    let mut first = true;
    for block in blocks {
        let separator = if first { "" } else { "\n" };
        match block {
            mycode_agent::ContentBlock::Text(text) => {
                out.push_str(separator);
                out.push_str(&text.text);
            }
            mycode_agent::ContentBlock::ToolCall(call) => {
                let _ = write!(out, "{separator}tool_call {} {}", call.name, call.arguments);
            }
            mycode_agent::ContentBlock::Thinking(_) | mycode_agent::ContentBlock::Image(_) => {
                continue;
            }
        }
        first = false;
    }
    out
}

fn is_tool_result(message: &Message) -> bool {
    matches!(message, Message::ToolResult(_))
}

/// Whether `checkpoint` still names a prefix of this ledger replay.
///
/// `covered_messages` has to land on a real message, must not split a tool
/// result off its call, and the uncovered tail has to be the same head or
/// still fit under the auto threshold. A moved head with a tail that no
/// longer fits is summarized again instead of reused.
fn checkpoint_matches(
    history: &[Arc<Message>],
    checkpoint: &mycode_agent::CompactionCheckpoint,
    head: &str,
    auto_threshold: usize,
    overhead: usize,
    scale: TokenScale,
) -> bool {
    if !covers_a_prefix(history, checkpoint.covered_messages) {
        return false;
    }
    let tail_tokens: usize = history[checkpoint.covered_messages..]
        .iter()
        .map(|message| scale.apply(message_tokens(message)))
        .sum();
    checkpoint.covered_head == head || tail_tokens.saturating_add(overhead) <= auto_threshold
}

/// Whether `covered` leading messages are a prefix of `history` a summary
/// can stand in for: at least one, no more than there are, and not cut
/// between a tool call and its result.
fn covers_a_prefix(history: &[Arc<Message>], covered: usize) -> bool {
    covered > 0
        && covered <= history.len()
        && !history
            .get(covered)
            .is_some_and(|message| is_tool_result(message))
}

pub(crate) fn is_summary_message(message: &Message) -> bool {
    // The marker opens the first text block; joining the whole message just
    // to test its prefix copied the summary on every request.
    let Message::User(user) = message else {
        return false;
    };
    user.content
        .iter()
        .find_map(|block| match block {
            mycode_agent::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .is_some_and(is_display_only_summary)
}

/// Returns the split index when compaction is due: everything before it is
/// summarized, everything from it on stays verbatim. `tokens` is the size
/// of the request `history` makes.
fn compaction_split(
    history: &[Arc<Message>],
    tokens: usize,
    threshold: usize,
    tail_budget: usize,
    scale: TokenScale,
) -> Option<usize> {
    if history.len() < 2 || tokens <= threshold {
        return None;
    }
    let mut used = 0usize;
    let mut tail_start = history.len();
    while tail_start > 0 {
        let tokens = scale.apply(message_tokens(&history[tail_start - 1]));
        if used > 0 && used.saturating_add(tokens) > tail_budget {
            break;
        }
        used = used.saturating_add(tokens);
        tail_start -= 1;
    }
    while tail_start > 0 && is_tool_result(&history[tail_start]) {
        tail_start -= 1;
    }
    if tail_start == 0 || (tail_start == 1 && is_summary_message(&history[0])) {
        return None;
    }
    Some(tail_start)
}

/// Messages that still need to be summarized.
///
/// `covered` is the checkpoint's `covered_messages`. Those leading messages
/// are already inside the previous summary, so the new transcript starts
/// there and stops at `head_end` (the verbatim tail stays out).
fn transcript_head(
    history: &[Arc<Message>],
    head_end: usize,
    covered: Option<usize>,
) -> &[Arc<Message>] {
    let head_end = head_end.min(history.len());
    let start = covered
        .filter(|count| *count > 0)
        .map(|count| count.min(head_end))
        .unwrap_or(0);
    &history[start..head_end]
}

/// The prior checkpoint stands in for everything before `head`, so it is
/// kept whole; only the message log is cut to `budget` provider tokens,
/// dropping its oldest messages first.
fn compaction_transcript(
    prior_summary: Option<&str>,
    head: &[Arc<Message>],
    budget: usize,
    scale: TokenScale,
) -> String {
    let mut prefix = String::new();
    if let Some(summary) = prior_summary {
        prefix.push_str("Previous checkpoint:\n");
        prefix.push_str(summary);
        prefix.push_str("\n\n");
    }
    let mut transcript = String::new();
    for message in head {
        if is_summary_message(message) {
            continue;
        }
        let role = match message.as_ref() {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "tool",
        };
        let text = excerpt(&message_text(message));
        let _ = write!(transcript, "[{role}] {text}\n\n");
    }
    let room = budget.saturating_sub(scale.apply(text_tokens(&prefix)));
    let skip = newest_within(&transcript, room, scale);
    if skip > 0 {
        format!(
            "{prefix}...earlier content elided...\n{}",
            &transcript[skip..]
        )
    } else {
        prefix + transcript.as_str()
    }
}

/// Byte index where the newest part of `text` that fits `budget` provider
/// tokens starts; `0` when all of it fits.
fn newest_within(text: &str, budget: usize, scale: TokenScale) -> usize {
    let mut tally = Tally::default();
    for (index, ch) in text.char_indices().rev() {
        tally.char(ch);
        if scale.apply(tally.tokens()) > budget {
            return index + ch.len_utf8();
        }
    }
    0
}

/// Keeps the first and last half of the excerpt budget.
///
/// The kept text is still [`EXCERPT_CHARS`] characters. A longer message
/// records how many characters were dropped between the two halves.
fn excerpt(text: &str) -> String {
    elide(text, EXCERPT_CHARS)
}

/// `text` cut to its first and last `keep / 2` characters, with the count
/// of the ones dropped between them.
fn elide(text: &str, keep: usize) -> String {
    let count = text.chars().count();
    if count <= keep {
        return text.to_owned();
    }
    let head_chars = keep / 2;
    let tail_chars = keep - head_chars;
    let head_end = text
        .char_indices()
        .nth(head_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let tail_start = text
        .char_indices()
        .nth(count - tail_chars)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let omitted = count - head_chars - tail_chars;
    format!(
        "{}\n...{omitted} characters omitted...\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

/// Models that reject an explicit thinking disable keep thinking on for the
/// summary. Their wire `Off` mapping omits the rejected field or substitutes
/// an always-on shape; the summary needs a larger output cap so thinking
/// cannot consume it all.
mod summary;
use self::summary::*;

#[cfg(test)]
mod tests;
