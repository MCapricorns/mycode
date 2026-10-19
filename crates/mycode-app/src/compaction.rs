//! Auto-compaction: Codex-style checkpoint summaries before each provider request.
//!
//! Trigger is 90% of the usable context window (95% of the model window, same
//! safety margin as Codex). The tail keeps the last ~20k tokens of messages
//! without splitting a tool-call pair. The head is summarized with a handoff
//! prompt (goals, files, decisions, errors, next steps). The session ledger
//! is never rewritten; only the in-memory request history shrinks.

use std::fmt::Write as _;
use std::sync::Arc;

use mycode_core::{Message, Provider as _, Request, StreamEvent};
use mycode_providers::WireProvider;
use tokio_util::sync::CancellationToken;

/// Fallback trigger when the catalog/settings have no context window.
const DEFAULT_THRESHOLD_TOKENS: usize = 48_000;
/// Codex keeps roughly this many recent tokens after the summary.
const TAIL_TOKEN_BUDGET: usize = 20_000;
/// Usable window as a percent of the raw model context (Codex 95%).
const USABLE_WINDOW_PERCENT: u64 = 95;
/// Auto-compact trigger as a percent of the usable window (Codex 90%).
const TRIGGER_PERCENT: u64 = 90;
const SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
const TRANSCRIPT_CAP_CHARS: usize = 300_000;
const EXCERPT_CHARS: usize = 4_000;
/// Output cap for a summary whose thinking is disabled.
const SUMMARY_OUTPUT_TOKENS: u64 = 8_192;
/// Output cap when the model keeps thinking on (it rejects an off switch).
const SUMMARY_OUTPUT_WITH_THINKING: u64 = 32_768;
/// One retry after `length` / `max_tokens`. A second truncation is a failure.
const SUMMARY_OUTPUT_RETRY_TOKENS: u64 = 65_536;
const SUMMARY_PREFIX: &str = "COMPACTION SUMMARY";
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

/// Token estimate for the system prompt and tool schemas sent with every request.
#[must_use]
pub(crate) fn estimate_prefix_tokens(system: &[String], tools: &[mycode_core::ToolSpec]) -> usize {
    let mut chars = 0usize;
    for part in system {
        chars = chars.saturating_add(part.chars().count());
    }
    for tool in tools {
        chars = chars.saturating_add(tool.name.chars().count());
        chars = chars.saturating_add(tool.description.chars().count());
        chars =
            chars.saturating_add(mycode_core::encoded_json_len(&tool.params_schema).unwrap_or(0));
    }
    mycode_config::estimate_token_count(chars)
}

/// Tokens that fire auto-compaction for this model window.
#[must_use]
pub(crate) fn compaction_threshold(context_window: u64) -> usize {
    if context_window == 0 {
        return DEFAULT_THRESHOLD_TOKENS;
    }
    let usable = context_window.saturating_mul(USABLE_WINDOW_PERCENT) / 100;
    (usable.saturating_mul(TRIGGER_PERCENT) / 100) as usize
}

/// Inputs that stay constant for one turn's compaction attempts.
pub(crate) struct CompactScope<'a> {
    pub home: &'a mycode_config::HomeLayout,
    pub wire: &'a WireProvider,
    pub model: &'a str,
    pub session_id: &'a str,
    pub branch_id: &'a str,
    pub head: &'a str,
    pub context_window: u64,
    /// Tokens already consumed by the system prompt and tool schemas.
    ///
    /// The history estimate alone stays under the trigger while the real
    /// request, which always sends this prefix, is already near the window.
    pub overhead_tokens: usize,
    /// Stops an in-flight summary request. A cancelled attempt fails and
    /// leaves the history unchanged.
    pub cancel: &'a CancellationToken,
}

/// What a compaction attempt did with the checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompactStatus {
    /// A new checkpoint was written.
    Wrote,
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
    /// Summary text when this attempt wrote a new checkpoint. Manual and
    /// automatic compaction both surface this in the transcript.
    pub summary: Option<String>,
}

/// Compacts history before a provider request. Failures degrade to the
/// original history so a turn never dies on housekeeping.
pub(crate) async fn compact_history(
    scope: &CompactScope<'_>,
    mut history: Vec<Arc<Message>>,
    force: bool,
) -> Compacted {
    let auto_threshold = compaction_threshold(scope.context_window);
    let threshold = if force { 0 } else { auto_threshold };
    let read_home = scope.home.clone();
    let read_session = scope.session_id.to_owned();
    let read_branch = scope.branch_id.to_owned();
    let prior = tokio::task::spawn_blocking(move || {
        mycode_config::read_compaction(&read_home, &read_session)
            .ok()
            .flatten()
            .filter(|checkpoint| checkpoint.branch_id == read_branch)
    })
    .await
    .ok()
    .flatten();
    // `/compact` writes a checkpoint the next request must use even while the
    // raw ledger is still under the auto threshold. Apply that checkpoint
    // before the threshold check, then judge the threshold on the stitched
    // history. A forced compact still summarizes (or reports that this head
    // is already covered) instead of stopping at the previous stitch.
    let starts_with_summary = history
        .first()
        .is_some_and(|message| is_summary_message(message));
    if !force
        && !starts_with_summary
        && let Some(checkpoint) = prior.as_ref()
        && checkpoint_matches(
            &history,
            checkpoint,
            scope.head,
            auto_threshold,
            scope.overhead_tokens,
        )
    {
        let stitched = with_summary(&checkpoint.summary, &history[checkpoint.covered_messages..]);
        if compaction_split(
            &stitched,
            threshold,
            TAIL_TOKEN_BUDGET,
            scope.overhead_tokens,
        )
        .is_none()
        {
            return Compacted {
                messages: stitched,
                status: CompactStatus::Unchanged,
                summary: None,
            };
        }
        // Summary + uncovered tail is still over the window. When the raw
        // ledger itself splits, fall through and persist a new checkpoint
        // against that ledger. When only the stitched copy is over, shrink
        // it in memory: its indexes are not ledger indexes.
        if compaction_split(
            &history,
            threshold,
            TAIL_TOKEN_BUDGET,
            scope.overhead_tokens,
        )
        .is_none()
        {
            history = stitched;
        }
    }
    // A manual `/compact` on a session that still fits the tail budget
    // summarizes everything instead of reporting nothing to compact: the
    // user asked for the shrink now, not at the auto threshold. The covered
    // range is the whole ledger, so the next request replays the summary
    // plus whatever arrives after it.
    let head_end = match compaction_split(
        &history,
        threshold,
        TAIL_TOKEN_BUDGET,
        scope.overhead_tokens,
    ) {
        Some(head_end) => head_end,
        None if force && history.len() >= 2 => history.len(),
        None => {
            return Compacted {
                messages: history,
                status: CompactStatus::Unchanged,
                summary: None,
            };
        }
    };
    // `/compact` on a head this checkpoint already covers would summarize the
    // same prefix again (same replaced count, same tail). Say so instead.
    // A session with fewer than two messages has nothing to summarize and
    // already returned `Unchanged` above; a longer session that fits the
    // tail is summarized in full below.
    if force
        && let Some(checkpoint) = prior.as_ref()
        && checkpoint.covered_head == scope.head
        && checkpoint.covered_messages > 0
    {
        return Compacted {
            messages: history,
            status: CompactStatus::Covered,
            summary: None,
        };
    }
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
    let transcript =
        compaction_transcript(prior_summary, transcript_head(&history, head_end, covered));
    // The summary can run for minutes. A cancel drops the attempt (and its
    // stream, which cancels the request) instead of holding the turn hostage.
    let summarized = tokio::select! {
        biased;
        () = scope.cancel.cancelled() => Ok(Err("compaction cancelled".to_owned())),
        summarized = tokio::time::timeout(
            SUMMARY_TIMEOUT,
            summarize_transcript(scope.wire, scope.model, &transcript, scope.cancel),
        ) => summarized,
    };
    let summary = match summarized {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => {
            return Compacted {
                messages: history,
                status: CompactStatus::Failed(message),
                summary: None,
            };
        }
        Err(_) => {
            return Compacted {
                messages: history,
                status: CompactStatus::Failed("summary timed out".to_owned()),
                summary: None,
            };
        }
    };
    let compacted = with_summary(&summary, &history[head_end..]);
    // A history that already starts with a summary is not the ledger
    // replay. Persisting `head_end` would store an index into that shorter
    // vector, and the next open of this head would slice the ledger with it.
    if already_summarized {
        return Compacted {
            messages: compacted,
            status: CompactStatus::Unchanged,
            summary: None,
        };
    }
    if scope.cancel.is_cancelled() {
        return Compacted {
            messages: history,
            status: CompactStatus::Failed("compaction cancelled".to_owned()),
            summary: None,
        };
    }
    let shown = summary.clone();
    let checkpoint = mycode_config::CompactionCheckpoint {
        format_version: mycode_config::COMPACTION_FORMAT_VERSION,
        kind: mycode_config::COMPACTION_KIND.to_owned(),
        session_id: scope.session_id.to_owned(),
        branch_id: scope.branch_id.to_owned(),
        covered_head: scope.head.to_owned(),
        covered_messages: head_end,
        summary,
        model: scope.model.to_owned(),
        created_at_unix: crate::unix_now().as_secs(),
    };
    let write_home = scope.home.clone();
    let write_session = scope.session_id.to_owned();
    let written = tokio::task::spawn_blocking(move || {
        mycode_config::write_compaction(&write_home, &write_session, &checkpoint)
    })
    .await
    .map_err(|error| format!("checkpoint write task failed: {error}"));
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Compacted {
                messages: history,
                status: CompactStatus::Failed(format!("checkpoint write failed: {error}")),
                summary: None,
            };
        }
        Err(message) => {
            return Compacted {
                messages: history,
                status: CompactStatus::Failed(message),
                summary: None,
            };
        }
    }
    Compacted {
        messages: compacted,
        status: CompactStatus::Wrote,
        summary: Some(shown),
    }
}

fn with_summary(summary: &str, tail: &[Arc<Message>]) -> Vec<Arc<Message>> {
    let mut compacted = Vec::with_capacity(tail.len() + 1);
    compacted.push(Arc::new(Message::User(mycode_core::UserMessage::text(
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

/// The summary a history opens with, without its wire frame, or `None`
/// when the history is a plain ledger replay.
fn leading_summary(history: &[Arc<Message>]) -> Option<String> {
    let first = history
        .first()
        .filter(|message| is_summary_message(message))?;
    let text = message_text(first);
    let frame = request_summary_text("");
    Some(match text.strip_prefix(frame.as_str()) {
        Some(body) => body.to_owned(),
        None => summary_body(&text).to_owned(),
    })
}

fn message_tokens(message: &Message) -> usize {
    mycode_config::estimate_token_count(message_chars(message))
}

fn message_chars(message: &Message) -> usize {
    match message {
        Message::User(user) => blocks_chars(&user.content),
        Message::Assistant(assistant) => blocks_chars(&assistant.blocks),
        Message::ToolResult(result) => {
            // `tool_result {id} {body}`
            "tool_result ".chars().count()
                + result.tool_call_id.chars().count()
                + 1
                + blocks_chars(&result.content)
        }
    }
}

fn blocks_chars(blocks: &[mycode_core::ContentBlock]) -> usize {
    let mut chars = 0usize;
    let mut parts = 0usize;
    for block in blocks {
        match block {
            mycode_core::ContentBlock::Text(text) => {
                chars += text.text.chars().count();
                parts += 1;
            }
            mycode_core::ContentBlock::ToolCall(call) => {
                // `tool_call {name} {arguments}`
                chars += "tool_call ".chars().count()
                    + call.name.chars().count()
                    + 1
                    + json_chars(&call.arguments);
                parts += 1;
            }
            mycode_core::ContentBlock::Thinking(_) | mycode_core::ContentBlock::Image(_) => {}
        }
    }
    chars + parts.saturating_sub(1)
}

fn json_chars(value: &serde_json::Value) -> usize {
    // Same length as `Display` for JSON, without allocating when the value
    // is a string.
    match value {
        serde_json::Value::String(text) => json_string_chars(text),
        other => other.to_string().chars().count(),
    }
}

fn json_string_chars(text: &str) -> usize {
    let mut count = 2;
    for ch in text.chars() {
        count += match ch {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            control if (control as u32) < 0x20 => 6,
            _ => 1,
        };
    }
    count
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
fn assistant_transcript_text(blocks: &[mycode_core::ContentBlock]) -> String {
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

fn blocks_text(blocks: &[mycode_core::ContentBlock]) -> String {
    let mut out = String::new();
    let mut first = true;
    for block in blocks {
        let separator = if first { "" } else { "\n" };
        match block {
            mycode_core::ContentBlock::Text(text) => {
                out.push_str(separator);
                out.push_str(&text.text);
            }
            mycode_core::ContentBlock::ToolCall(call) => {
                let _ = write!(out, "{separator}tool_call {} {}", call.name, call.arguments);
            }
            mycode_core::ContentBlock::Thinking(_) | mycode_core::ContentBlock::Image(_) => {
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
    checkpoint: &mycode_config::CompactionCheckpoint,
    head: &str,
    auto_threshold: usize,
    overhead: usize,
) -> bool {
    if checkpoint.covered_messages == 0 || checkpoint.covered_messages >= history.len() {
        return false;
    }
    if is_tool_result(&history[checkpoint.covered_messages]) {
        return false;
    }
    let tail_tokens: usize = history[checkpoint.covered_messages..]
        .iter()
        .map(|message| message_tokens(message))
        .sum();
    checkpoint.covered_head == head || tail_tokens.saturating_add(overhead) <= auto_threshold
}

fn is_summary_message(message: &Message) -> bool {
    // The marker opens the first text block; joining the whole message just
    // to test its prefix copied the summary on every request.
    let Message::User(user) = message else {
        return false;
    };
    user.content
        .iter()
        .find_map(|block| match block {
            mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .is_some_and(is_display_only_summary)
}

/// Returns the split index when compaction is due: everything before it is
/// summarized, everything from it on stays verbatim.
fn compaction_split(
    history: &[Arc<Message>],
    threshold: usize,
    tail_budget: usize,
    overhead: usize,
) -> Option<usize> {
    if history.len() < 2 {
        return None;
    }
    let estimate: usize = history
        .iter()
        .map(|message| message_tokens(message))
        .sum::<usize>()
        .saturating_add(overhead);
    if estimate <= threshold {
        return None;
    }
    let mut used = 0usize;
    let mut tail_start = history.len();
    while tail_start > 0 {
        let tokens = message_tokens(&history[tail_start - 1]);
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
/// kept whole; only the message log is cut to [`TRANSCRIPT_CAP_CHARS`],
/// dropping its oldest messages first.
fn compaction_transcript(prior_summary: Option<&str>, head: &[Arc<Message>]) -> String {
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
    let count = transcript.chars().count();
    if count > TRANSCRIPT_CAP_CHARS {
        let skip = transcript
            .char_indices()
            .nth(count - TRANSCRIPT_CAP_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(0);
        format!(
            "{prefix}...earlier content elided...\n{}",
            &transcript[skip..]
        )
    } else {
        prefix + &transcript
    }
}

/// Keeps the first and last half of the excerpt budget.
///
/// The kept text is still [`EXCERPT_CHARS`] characters. A longer message
/// records how many characters were dropped between the two halves.
fn excerpt(text: &str) -> String {
    let count = text.chars().count();
    if count <= EXCERPT_CHARS {
        return text.to_owned();
    }
    let head_chars = EXCERPT_CHARS / 2;
    let tail_chars = EXCERPT_CHARS - head_chars;
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
fn summary_max_output(model: &str) -> u64 {
    if mycode_providers::keeps_thinking_when_off(model) {
        SUMMARY_OUTPUT_WITH_THINKING
    } else {
        SUMMARY_OUTPUT_TOKENS
    }
}

enum SummaryAttempt {
    Complete(String),
    Truncated,
    TooShort,
}

async fn summarize_transcript(
    wire: &WireProvider,
    model: &str,
    transcript: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let first = summary_max_output(model);
    match request_summary(wire, transcript, first, cancel).await? {
        SummaryAttempt::Complete(summary) => Ok(summary),
        SummaryAttempt::Truncated => finish_summary_retry(
            request_summary(wire, transcript, SUMMARY_OUTPUT_RETRY_TOKENS, cancel).await?,
        ),
        SummaryAttempt::TooShort => {
            finish_summary_retry(request_summary(wire, transcript, first, cancel).await?)
        }
    }
}

fn finish_summary_retry(attempt: SummaryAttempt) -> Result<String, String> {
    match attempt {
        SummaryAttempt::Complete(summary) => Ok(summary),
        SummaryAttempt::Truncated => Err("summary was truncated".to_owned()),
        SummaryAttempt::TooShort => Err("summary was too short".to_owned()),
    }
}

/// User message for the summary request.
///
/// The transcript is fenced so a conversation that ends on a tool result is
/// data, not the next task. The reminder after the fence repeats that.
fn summary_user_text(transcript: &str) -> String {
    let body = transcript.trim_end_matches(['\r', '\n']);
    format!(
        "Summarize the following conversation for continuation:\n\n\
         <conversation>\n\
         {body}\n\
         </conversation>\n\n\
         {SUMMARY_REMINDER}"
    )
}

fn summary_request(transcript: &str, max_output_tokens: u64) -> Request {
    let mut request = Request::new()
        .with_system_prompt(
            "You are performing a CONTEXT CHECKPOINT COMPACTION. Summarize \
your own work so far so you can resume it after a context reset.\n\
Write dense factual prose in first person, no preamble. Cover:\n\
- User goals and constraints, including corrections and preferences the user stated\n\
- Decisions made, and why\n\
- Files and paths touched (created, edited, read)\n\
- Commands run and their outcomes; which checks passed, failed, or were not run yet\n\
- Subagent answers the remaining work depends on\n\
- Current work, open tasks, and unresolved errors\n\
- The user's latest request, quoted when it is short\n\
- Clear next steps\n\
Preserve names, paths, commands, and error text verbatim. Do not invent work that did not \
happen. Never copy secrets (API keys, tokens, passwords) into the summary. Write in the \
language the user writes in. Stay under about 2,500 words; drop detail that the next steps \
do not need.",
        )
        .with_message(Message::User(mycode_core::UserMessage::text(
            summary_user_text(transcript),
        )));
    // `Off` is applied by the provider adapter: Anthropic and GLM/Z.AI send
    // `thinking.type = disabled`, Qwen sends `enable_thinking: false`, and
    // the other families use their own off or lowest-effort field.
    request.reasoning = Some(mycode_core::ReasoningLevel::Off);
    request.max_output_tokens = Some(max_output_tokens);
    request
}

async fn request_summary(
    wire: &WireProvider,
    transcript: &str,
    max_output_tokens: u64,
    cancel: &CancellationToken,
) -> Result<SummaryAttempt, String> {
    let request = summary_request(transcript, max_output_tokens);
    let mut stream = wire
        .stream(&request, cancel.child_token())
        .await
        .map_err(|error| format!("summary request failed: {error}"))?;
    let mut summary = String::new();
    let stop = loop {
        let Some(event) = stream.next().await else {
            return Err("summary stream ended without completion".to_owned());
        };
        match event {
            StreamEvent::TextDelta(delta) => summary.push_str(&delta),
            StreamEvent::Done { message } => {
                if summary.is_empty() {
                    summary = message.text();
                }
                break message.stop_reason;
            }
            StreamEvent::Error(error) => return Err(format!("summary stream failed: {error}")),
            _ => {}
        }
    };
    if stop == mycode_core::StopReason::Length {
        return Ok(SummaryAttempt::Truncated);
    }
    if stop == mycode_core::StopReason::Error {
        return Err("summary stream failed".to_owned());
    }
    if let Some((cut, _)) = summary.char_indices().nth(mycode_config::MAX_SUMMARY_CHARS) {
        summary.truncate(cut);
    }
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err("summary was empty".to_owned());
    }
    if trimmed.chars().count() < MIN_SUMMARY_CHARS {
        return Ok(SummaryAttempt::TooShort);
    }
    Ok(SummaryAttempt::Complete(summary))
}

#[cfg(test)]
mod tests;
