//! Auto-compaction: Codex-style checkpoint summaries before each provider request.
//!
//! Trigger is 90% of the usable context window (95% of the model window, same
//! safety margin as Codex). The tail keeps the last ~20k tokens of messages
//! without splitting a tool-call pair. The head is summarized with a handoff
//! prompt (goals, files, decisions, errors, next steps). The session ledger
//! is never rewritten; only the in-memory request history shrinks.

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
Output only the summary in the required format.";

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
        chars = chars.saturating_add(tool.params_schema.to_string().len());
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
}

/// What a compaction attempt did with the checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompactStatus {
    /// A new checkpoint was written.
    Wrote,
    /// There is nothing new to summarize.
    Unchanged,
    /// A checkpoint already covers this exact head.
    Covered,
    /// The summary request failed. The history is unchanged.
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
    let prior = mycode_config::read_compaction(scope.home, scope.session_id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint.branch_id == scope.branch_id);
    // `/compact` writes a checkpoint the next request must use even while the
    // raw ledger is still under the auto threshold. Apply that checkpoint
    // before the threshold check, then judge the threshold on the stitched
    // history. A forced compact still summarizes (or reports that this head
    // is already covered) instead of stopping at the previous stitch.
    let mut stitched_over_threshold = false;
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
            stitched_over_threshold = true;
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
    // A short session that fits in the tail returns above, before this, so
    // it still reports nothing to compact.
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
    // points into the tail and would drop messages that must stay.
    let already_summarized = history
        .first()
        .is_some_and(|message| is_summary_message(message));
    let covered = if stitched_over_threshold {
        None
    } else {
        prior.as_ref().map(|checkpoint| checkpoint.covered_messages)
    };
    let prior_summary = prior.as_ref().map(|checkpoint| checkpoint.summary.as_str());
    // The previous summary already stands in for `history[..covered]`.
    // Sending that prefix again makes a later `/compact` re-bill the same
    // leading messages on top of the old summary.
    let transcript =
        compaction_transcript(prior_summary, transcript_head(&history, head_end, covered));
    let summarized = tokio::time::timeout(
        SUMMARY_TIMEOUT,
        summarize_transcript(scope.wire, scope.model, &transcript),
    )
    .await;
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
        created_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
    };
    if let Err(error) = mycode_config::write_compaction(scope.home, scope.session_id, &checkpoint) {
        return Compacted {
            messages: history,
            status: CompactStatus::Failed(format!("checkpoint write failed: {error:?}")),
            summary: None,
        };
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
        display_summary_text(summary),
    ))));
    compacted.extend(tail.iter().cloned());
    compacted
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
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            mycode_core::ContentBlock::Text(text) => parts.push(text.text.clone()),
            mycode_core::ContentBlock::ToolCall(call) => {
                parts.push(format!("tool_call {} {}", call.name, call.arguments));
            }
            mycode_core::ContentBlock::Thinking(_) | mycode_core::ContentBlock::Image(_) => {}
        }
    }
    parts.join("\n")
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
    matches!(message, Message::User(user) if is_display_only_summary(&blocks_text(&user.content)))
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

fn compaction_transcript(prior_summary: Option<&str>, head: &[Arc<Message>]) -> String {
    let mut transcript = String::new();
    if let Some(summary) = prior_summary {
        transcript.push_str("Previous checkpoint:\n");
        transcript.push_str(summary);
        transcript.push_str("\n\n");
    }
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
        transcript.push_str(&format!("[{role}] {text}\n\n"));
    }
    let count = transcript.chars().count();
    if count > TRANSCRIPT_CAP_CHARS {
        let skip = transcript
            .char_indices()
            .nth(count - TRANSCRIPT_CAP_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(0);
        format!("...earlier content elided...\n{}", &transcript[skip..])
    } else {
        transcript
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

/// Models that reject an explicit thinking disable keep the provider
/// default. The wire `Off` mapping already omits the rejected field; the
/// summary needs a larger output cap so thinking cannot consume it all.
fn summary_max_output(model: &str) -> u64 {
    if model.to_ascii_lowercase().contains("kimi-k2.7-code") {
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
) -> Result<String, String> {
    let first = summary_max_output(model);
    match request_summary(wire, transcript, first).await? {
        SummaryAttempt::Complete(summary) => Ok(summary),
        SummaryAttempt::Truncated => finish_summary_retry(
            request_summary(wire, transcript, SUMMARY_OUTPUT_RETRY_TOKENS).await?,
        ),
        SummaryAttempt::TooShort => {
            finish_summary_retry(request_summary(wire, transcript, first).await?)
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
            "You are performing a CONTEXT CHECKPOINT COMPACTION. Create a \
handoff summary for another coding-agent model that will resume the task.\n\
Write dense factual prose, no preamble. Cover:\n\
- User goals and constraints\n\
- Decisions made, and why\n\
- Files and paths touched (created, edited, read)\n\
- Commands run and their outcomes\n\
- Current work, open tasks, and unresolved errors\n\
- Clear next steps\n\
Preserve names, paths, and error text. Do not invent work that did not happen.",
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
) -> Result<SummaryAttempt, String> {
    let request = summary_request(transcript, max_output_tokens);
    let cancel = CancellationToken::new();
    let mut stream = wire
        .stream(&request, cancel)
        .await
        .map_err(|error| format!("summary request failed: {error:?}"))?;
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
            StreamEvent::Error(error) => return Err(format!("summary stream failed: {error:?}")),
            _ => {}
        }
    };
    if stop == mycode_core::StopReason::Length {
        return Ok(SummaryAttempt::Truncated);
    }
    if stop == mycode_core::StopReason::Error {
        return Err("summary stream failed".to_owned());
    }
    let chars: Vec<char> = summary.chars().collect();
    if chars.len() > mycode_config::MAX_SUMMARY_CHARS {
        summary = chars[..mycode_config::MAX_SUMMARY_CHARS].iter().collect();
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
mod tests {
    use std::sync::Arc;

    use mycode_core::{
        AssistantMessage, ContentBlock, Message, StopReason, TextBlock, ThinkingBlock, ToolCall,
        ToolResultMessage, UserMessage,
    };

    use super::{
        CompactStatus, Compacted, compaction_transcript, display_summary_text,
        is_display_only_summary, transcript_head,
    };

    /// Long enough for the short-summary gate. The opening phrase stays so
    /// older assertions can still find it.
    const PLAUSIBLE_SUMMARY: &str = concat!(
        "handoff notes. User goals and constraints: continue the coding task from the kept tail. ",
        "Decisions made, and why: the covered prefix is replaced by this checkpoint. ",
        "Files and paths touched: the files named in the conversation were read. ",
        "Commands run and their outcomes: none failed in the covered prefix. ",
        "Current work, open tasks, and unresolved errors: the tail is still unanswered. ",
        "Clear next steps: reply to the latest user message."
    );
    const FULL_HANDOFF: &str = concat!(
        "full handoff. User goals and constraints: continue the coding task from the kept tail. ",
        "Decisions made, and why: the covered prefix is replaced by this checkpoint. ",
        "Files and paths touched: the files named in the conversation were read. ",
        "Commands run and their outcomes: none failed in the covered prefix. ",
        "Current work, open tasks, and unresolved errors: the tail is still unanswered. ",
        "Clear next steps: reply to the latest user message."
    );

    fn user(text: &str) -> Arc<Message> {
        Arc::new(Message::User(UserMessage::text(text)))
    }

    #[test]
    fn a_later_compact_summarizes_only_the_uncovered_head() {
        let history = vec![
            user("old-a"),
            user("old-b"),
            user("old-c"),
            user("new-d"),
            user("new-e"),
            user("kept-tail"),
        ];
        let head = transcript_head(&history, 5, Some(3));
        let transcript = compaction_transcript(Some("prior summary"), head);
        assert!(transcript.contains("prior summary"), "{transcript}");
        assert!(transcript.contains("new-d"), "{transcript}");
        assert!(transcript.contains("new-e"), "{transcript}");
        assert!(!transcript.contains("old-a"), "{transcript}");
        assert!(!transcript.contains("old-b"), "{transcript}");
        assert!(!transcript.contains("old-c"), "{transcript}");
        assert!(!transcript.contains("kept-tail"), "{transcript}");
    }

    #[test]
    fn the_first_compact_still_includes_the_whole_head() {
        let history = vec![user("one"), user("two"), user("tail")];
        let head = transcript_head(&history, 2, None);
        assert_eq!(head.len(), 2);
        let transcript = compaction_transcript(None, head);
        assert!(transcript.contains("one"), "{transcript}");
        assert!(transcript.contains("two"), "{transcript}");
        assert!(!transcript.contains("tail"), "{transcript}");
    }

    #[test]
    fn manual_and_auto_compaction_post_the_same_summary_message() {
        let body = "files touched: src/main.rs\nnext: run the tests";
        let manual = Compacted {
            messages: Vec::new(),
            status: CompactStatus::Wrote,
            summary: Some(body.to_owned()),
        };
        let auto = Compacted {
            messages: Vec::new(),
            status: CompactStatus::Wrote,
            summary: Some(body.to_owned()),
        };
        let manual_text = display_summary_text(manual.summary.as_deref().expect("manual"));
        let auto_text = display_summary_text(auto.summary.as_deref().expect("auto"));
        assert_eq!(manual_text, auto_text);
        assert!(is_display_only_summary(&manual_text));
        assert!(manual_text.contains(body));
        assert!(super::summary_body(&manual_text).contains("src/main.rs"));
    }

    fn user_text(message: &Message) -> String {
        match message {
            Message::User(user) => user
                .content
                .iter()
                .filter_map(|block| match block {
                    mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect(),
            _ => String::new(),
        }
    }

    fn padded(marker: &str, chars: usize) -> String {
        let mut text = marker.to_owned();
        let extra = chars.saturating_sub(marker.chars().count());
        text.push_str(&"x".repeat(extra));
        text
    }

    struct SummaryTransport {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl mycode_providers::SseTransport for SummaryTransport {
        async fn post(
            &self,
            call: mycode_providers::TransportCall,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures_util::Stream<
                            Item = Result<bytes::Bytes, mycode_core::ProviderError>,
                        > + Send,
                >,
            >,
            mycode_core::ProviderError,
        > {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = String::from_utf8_lossy(&call.body);
            let text = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                PLAUSIBLE_SUMMARY
            } else {
                "unexpected summary request"
            };
            let chunk = serde_json::json!({
                "choices": [{
                    "delta": {"content": text},
                    "finish_reason": "stop"
                }]
            });
            let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            Ok(Box::pin(futures_util::stream::once(async move {
                Ok(bytes::Bytes::from(sse))
            })))
        }
    }

    fn scratch_home() -> (std::path::PathBuf, mycode_config::HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-compact-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let home = mycode_config::HomeLayout::from_root(&root).expect("home");
        (root, home)
    }

    fn summary_wire(
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> mycode_providers::WireProvider {
        let settings = mycode_config::ProviderSettings {
            id: "local".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["model-a".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        };
        let resolved = mycode_providers::ResolvedProvider::resolve(
            &settings,
            "model-a",
            "test-key",
            "test-agent",
        )
        .expect("provider");
        mycode_providers::WireProvider::new(
            resolved,
            std::sync::Arc::new(SummaryTransport { calls }),
        )
    }

    #[tokio::test]
    async fn manual_compact_is_the_next_request_while_under_the_auto_threshold() {
        let (root, home) = scratch_home();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire = summary_wire(calls.clone());
        let head = padded("HEAD-MARKER-", 100_000);
        let history = vec![user(&head), user("TAIL-ONE"), user("TAIL-TWO")];
        let scope = super::CompactScope {
            home: &home,
            wire: &wire,
            model: "model-a",
            session_id: "session-a",
            branch_id: "branch-a",
            head: "head-1",
            // Large window: this history is over the ~20k tail, under the
            // auto threshold, which is the case that used to skip the checkpoint.
            context_window: 200_000,
            overhead_tokens: 0,
        };
        let manual = super::compact_history(&scope, history.clone(), true).await;
        assert_eq!(manual.status, CompactStatus::Wrote);
        assert_eq!(manual.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(user_text(&manual.messages[0]).contains("handoff notes"));
        assert!(user_text(&manual.messages[0]).contains("COMPACTION SUMMARY"));
        assert_eq!(user_text(&manual.messages[1]), "TAIL-ONE");
        assert_eq!(user_text(&manual.messages[2]), "TAIL-TWO");
        assert!(
            manual
                .messages
                .iter()
                .all(|message| !user_text(message).contains("HEAD-MARKER-"))
        );

        let mut later = history;
        later.push(user("NEW-TURN"));
        let next_scope = super::CompactScope {
            head: "head-2",
            ..scope
        };
        let next = super::compact_history(&next_scope, later, false).await;
        assert_eq!(next.status, CompactStatus::Unchanged);
        assert!(next.summary.is_none());
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an under-threshold replay must reuse the checkpoint"
        );
        assert_eq!(
            next.messages
                .iter()
                .filter(|message| super::is_summary_message(message))
                .count(),
            1
        );
        assert!(user_text(&next.messages[0]).contains("handoff notes"));
        assert_eq!(user_text(&next.messages[1]), "TAIL-ONE");
        assert_eq!(user_text(&next.messages[2]), "TAIL-TWO");
        assert_eq!(user_text(&next.messages[3]), "NEW-TURN");
        assert!(
            next.messages
                .iter()
                .all(|message| !user_text(message).contains("HEAD-MARKER-"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn manual_compact_summarizes_a_session_that_still_fits() {
        let (root, home) = scratch_home();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wire = summary_wire(calls.clone());
        let history = vec![user("hi"), user("there")];
        let scope = super::CompactScope {
            home: &home,
            wire: &wire,
            model: "model-a",
            session_id: "session-b",
            branch_id: "branch-b",
            head: "head-1",
            context_window: 200_000,
            overhead_tokens: 0,
        };
        // `/compact` no longer answers "nothing to compact" for a short
        // session: the whole ledger is summarized and the tail is empty.
        let manual = super::compact_history(&scope, history, true).await;
        assert_eq!(manual.status, CompactStatus::Wrote);
        assert_eq!(manual.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(manual.messages.len(), 1);
        assert!(user_text(&manual.messages[0]).contains("COMPACTION SUMMARY"));
        let checkpoint = mycode_config::read_compaction(&home, "session-b")
            .expect("read")
            .expect("written");
        assert_eq!(checkpoint.covered_messages, 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_model_that_cannot_disable_thinking_gets_a_larger_summary_cap() {
        assert_eq!(super::summary_max_output("glm-5.3"), 8_192);
        assert_eq!(super::summary_max_output("kimi-k2.7-code"), 32_768);
    }

    #[test]
    fn a_long_message_keeps_its_head_and_tail() {
        let mut text = "HEAD-".to_owned();
        text.push_str(&"m".repeat(5_000));
        text.push_str("-TAIL");
        let kept = super::excerpt(&text);
        assert!(kept.starts_with("HEAD-"), "{kept}");
        assert!(kept.ends_with("-TAIL"), "{kept}");
        let omitted = text.chars().count() - super::EXCERPT_CHARS;
        assert!(
            kept.contains(&format!("{omitted} characters omitted")),
            "{kept}"
        );
        let marker = format!("\n...{omitted} characters omitted...\n");
        let (head, tail) = kept.split_once(&marker).expect("marker");
        assert_eq!(
            head.chars().count() + tail.chars().count(),
            super::EXCERPT_CHARS
        );
    }

    struct ScriptedSummary {
        bodies: std::sync::Mutex<Vec<String>>,
        /// `(finish_reason, content)` for each summary call, in order.
        script: Vec<(&'static str, &'static str)>,
        index: std::sync::atomic::AtomicUsize,
        /// `openai` or `anthropic`. The request body is what the test checks;
        /// the scripted stream only has to complete in that protocol.
        protocol: &'static str,
    }

    #[async_trait::async_trait]
    impl mycode_providers::SseTransport for ScriptedSummary {
        async fn post(
            &self,
            call: mycode_providers::TransportCall,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures_util::Stream<
                            Item = Result<bytes::Bytes, mycode_core::ProviderError>,
                        > + Send,
                >,
            >,
            mycode_core::ProviderError,
        > {
            self.bodies
                .lock()
                .expect("bodies")
                .push(String::from_utf8_lossy(&call.body).into_owned());
            let index = self.index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (finish, content) = self
                .script
                .get(index)
                .copied()
                .unwrap_or(("stop", PLAUSIBLE_SUMMARY));
            let sse = if self.protocol == "anthropic" {
                let stop = if finish == "length" {
                    "max_tokens"
                } else {
                    "end_turn"
                };
                let text = serde_json::to_string(content).unwrap_or_else(|_| "\"\"".to_owned());
                format!(
                    "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
                     data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{text}}}}}\n\n\
                     data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
                     data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop}\"}}}}\n\n\
                     data: {{\"type\":\"message_stop\"}}\n\n"
                )
            } else {
                let chunk = serde_json::json!({
                    "choices": [{
                        "delta": {"content": content},
                        "finish_reason": finish
                    }]
                });
                format!("data: {chunk}\n\ndata: [DONE]\n\n")
            };
            Ok(Box::pin(futures_util::stream::once(async move {
                Ok(bytes::Bytes::from(sse))
            })))
        }
    }

    fn scripted_wire(
        kind: &str,
        base_url: &str,
        model: &str,
        script: Vec<(&'static str, &'static str)>,
    ) -> (
        mycode_providers::WireProvider,
        std::sync::Arc<ScriptedSummary>,
    ) {
        let protocol = if kind == "anthropic-messages" {
            "anthropic"
        } else {
            "openai"
        };
        let transport = std::sync::Arc::new(ScriptedSummary {
            bodies: std::sync::Mutex::new(Vec::new()),
            script,
            index: std::sync::atomic::AtomicUsize::new(0),
            protocol,
        });
        let settings = mycode_config::ProviderSettings {
            id: "local".to_owned(),
            kind: kind.to_owned(),
            base_url: base_url.to_owned(),
            models: vec![model.to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        };
        let resolved =
            mycode_providers::ResolvedProvider::resolve(&settings, model, "test-key", "test-agent")
                .expect("provider");
        let wire = mycode_providers::WireProvider::new(resolved, transport.clone());
        (wire, transport)
    }

    fn large_history() -> Vec<Arc<Message>> {
        vec![
            user(&padded("HEAD-MARKER-", 100_000)),
            user("TAIL-ONE"),
            user("TAIL-TWO"),
        ]
    }

    #[tokio::test]
    async fn summary_request_disables_glm_thinking_and_retries_a_truncated_summary() {
        let (root, home) = scratch_home();
        let (wire, transport) = scripted_wire(
            "openai-completions",
            "https://api.z.ai/api/paas/v4",
            "glm-5.3",
            vec![("length", "partial"), ("stop", FULL_HANDOFF)],
        );
        let scope = super::CompactScope {
            home: &home,
            wire: &wire,
            model: "glm-5.3",
            session_id: "session-glm",
            branch_id: "branch-glm",
            head: "head-1",
            context_window: 200_000,
            overhead_tokens: 0,
        };
        let compacted = super::compact_history(&scope, large_history(), true).await;
        assert_eq!(compacted.status, CompactStatus::Wrote);
        assert_eq!(compacted.summary.as_deref(), Some(FULL_HANDOFF));
        let bodies = transport.bodies.lock().expect("bodies");
        assert_eq!(bodies.len(), 2, "a length finish must retry once");
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).expect("json");
        let second: serde_json::Value = serde_json::from_str(&bodies[1]).expect("json");
        assert_eq!(first["thinking"]["type"], "disabled");
        assert!(first.get("reasoning_effort").is_none());
        assert_eq!(first["max_tokens"], 8_192);
        assert_eq!(second["max_tokens"], 65_536);
        assert_eq!(second["thinking"]["type"], "disabled");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_still_truncated_summary_is_not_stored() {
        let (root, home) = scratch_home();
        let (wire, transport) = scripted_wire(
            "openai-completions",
            "https://api.z.ai/api/paas/v4",
            "glm-5.3",
            vec![("length", "partial"), ("length", "still partial")],
        );
        let scope = super::CompactScope {
            home: &home,
            wire: &wire,
            model: "glm-5.3",
            session_id: "session-cut",
            branch_id: "branch-cut",
            head: "head-1",
            context_window: 200_000,
            overhead_tokens: 0,
        };
        let compacted = super::compact_history(&scope, large_history(), true).await;
        match compacted.status {
            CompactStatus::Failed(message) => assert!(message.contains("truncated"), "{message}"),
            other => panic!("truncated summary was accepted: {other:?}"),
        }
        assert!(compacted.summary.is_none());
        assert_eq!(transport.index.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            mycode_config::read_compaction(&home, "session-cut")
                .expect("read")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_empty_summary_is_an_error() {
        let (root, home) = scratch_home();
        let (wire, transport) = scripted_wire(
            "anthropic-messages",
            "https://api.z.ai",
            "glm-5.3",
            vec![("end_turn", "")],
        );
        let scope = super::CompactScope {
            home: &home,
            wire: &wire,
            model: "glm-5.3",
            session_id: "session-empty",
            branch_id: "branch-empty",
            head: "head-1",
            context_window: 200_000,
            overhead_tokens: 0,
        };
        let compacted = super::compact_history(&scope, large_history(), true).await;
        match compacted.status {
            CompactStatus::Failed(message) => assert!(message.contains("empty"), "{message}"),
            other => panic!("empty summary was accepted: {other:?}"),
        }
        let bodies = transport.bodies.lock().expect("bodies");
        let body: serde_json::Value = serde_json::from_str(&bodies[0]).expect("json");
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["max_tokens"], 8_192);
        assert!(
            mycode_config::read_compaction(&home, "session-empty")
                .expect("read")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn assistant_message(blocks: Vec<ContentBlock>) -> Arc<Message> {
        Arc::new(Message::Assistant(AssistantMessage {
            blocks,
            usage: None,
            stop_reason: StopReason::Stop,
        }))
    }

    fn tool_result_message(id: &str, body: &str) -> Arc<Message> {
        Arc::new(Message::ToolResult(ToolResultMessage {
            tool_call_id: id.to_owned(),
            content: vec![ContentBlock::Text(TextBlock::new(body))],
            is_error: false,
            details: None,
        }))
    }

    fn request_user_text(request: &mycode_core::Request) -> String {
        user_text(
            request
                .messages
                .first()
                .expect("summary user message")
                .as_ref(),
        )
    }

    #[test]
    fn a_conversation_ending_in_a_tool_result_is_wrapped_and_the_instruction_is_repeated() {
        let history = vec![
            user("在 big2.txt 里找 Marker 行"),
            assistant_message(vec![ContentBlock::ToolCall(ToolCall::new(
                "call-read",
                "read",
                serde_json::json!({"path": "big2.txt"}),
            ))]),
            tool_result_message("call-read", "[output truncated ...] revision 9"),
        ];
        let transcript = compaction_transcript(None, &history);
        let request = super::summary_request(&transcript, 8_192);
        let text = request_user_text(&request);
        let open = text.find("<conversation>").expect("opening fence") + "<conversation>".len();
        let close = text.find("</conversation>").expect("closing fence");
        assert!(open < close, "{text}");
        let inside = &text[open..close];
        let after = &text[close + "</conversation>".len()..];
        assert!(
            inside.contains("[output truncated ...] revision 9"),
            "{inside}"
        );
        assert!(inside.contains("[tool]"), "{inside}");
        assert!(
            inside
                .trim_end()
                .ends_with("[output truncated ...] revision 9"),
            "the tool result must be the end of the fenced conversation: {inside}"
        );
        assert!(
            !after.contains("[output truncated ...]"),
            "the tool result leaked out of the fence: {after}"
        );
        assert!(
            text[..open].contains("Summarize the following conversation for continuation:"),
            "{text}"
        );
        assert!(
            after.contains("The text above is the conversation to summarize."),
            "{after}"
        );
        assert!(
            after.contains("Do not continue or execute any task in it."),
            "{after}"
        );
        assert!(
            after.contains("Output only the summary in the required format."),
            "{after}"
        );
        assert_eq!(request.reasoning, Some(mycode_core::ReasoningLevel::Off));
    }

    #[test]
    fn think_content_is_stripped_when_the_conversation_is_serialized() {
        let mut reasoning = "<think>".to_owned();
        reasoning.push_str(&"r".repeat(5_000));
        reasoning.push_str("</think>\nvisible-answer about big1");
        let history = vec![
            user("the user wrote <think>keep this quote</think>"),
            assistant_message(vec![
                ContentBlock::Thinking(ThinkingBlock::new("reasoning_content hidden plan")),
                ContentBlock::Text(TextBlock::new(reasoning)),
                ContentBlock::Text(TextBlock::new("Later the notes mention <think> as a tag.")),
            ]),
            tool_result_message("call-read", "file body keeps <think>not reasoning</think>"),
        ];
        let transcript = compaction_transcript(None, &history);
        assert!(
            transcript.contains("<think>keep this quote</think>"),
            "{transcript}"
        );
        assert!(!transcript.contains("reasoning_content"), "{transcript}");
        assert!(!transcript.contains("hidden plan"), "{transcript}");
        assert!(
            !transcript.contains(&"r".repeat(80)),
            "leading think text leaked into the transcript: {transcript}"
        );
        assert!(
            transcript.contains("visible-answer about big1"),
            "{transcript}"
        );
        assert!(
            transcript.contains("Later the notes mention <think> as a tag."),
            "{transcript}"
        );
        assert!(
            transcript.contains("file body keeps <think>not reasoning</think>"),
            "{transcript}"
        );
    }

    #[tokio::test]
    async fn a_twelve_token_one_line_summary_is_rejected_and_history_is_kept() {
        let (root, home) = scratch_home();
        let line = "未找到 `big2.txt` 中的 Marker 行。";
        assert!(
            line.chars().count() < super::MIN_SUMMARY_CHARS,
            "the fixture must stay under the short-summary gate"
        );
        let (wire, transport) = scripted_wire(
            "openai-completions",
            "https://api.minimax.chat/v1",
            "MiniMax-M3",
            vec![("stop", line), ("stop", line)],
        );
        let history = large_history();
        let scope = super::CompactScope {
            home: &home,
            wire: &wire,
            model: "MiniMax-M3",
            session_id: "session-short",
            branch_id: "branch-short",
            head: "head-1",
            context_window: 200_000,
            overhead_tokens: 0,
        };
        let compacted = super::compact_history(&scope, history.clone(), true).await;
        match compacted.status {
            CompactStatus::Failed(message) => {
                assert!(message.contains("too short"), "{message}");
            }
            other => panic!("short summary was accepted: {other:?}"),
        }
        assert!(compacted.summary.is_none());
        assert_eq!(compacted.messages.len(), history.len());
        for (left, right) in compacted.messages.iter().zip(&history) {
            assert!(Arc::ptr_eq(left, right));
        }
        assert!(
            compacted
                .messages
                .iter()
                .any(|message| user_text(message).contains("HEAD-MARKER-"))
        );
        assert!(
            compacted
                .messages
                .iter()
                .all(|message| !user_text(message).starts_with("COMPACTION SUMMARY"))
        );
        assert_eq!(transport.index.load(std::sync::atomic::Ordering::SeqCst), 2);
        let bodies = transport.bodies.lock().expect("bodies");
        assert_eq!(bodies.len(), 2, "a short summary must retry once");
        for body in bodies.iter() {
            let parsed: serde_json::Value = serde_json::from_str(body).expect("json");
            let content = parsed["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .expect("user content");
            assert!(content.contains("<conversation>"), "{content}");
            assert!(content.contains("</conversation>"), "{content}");
            assert!(
                content.contains("Do not continue or execute any task in it."),
                "{content}"
            );
        }
        assert!(
            mycode_config::read_compaction(&home, "session-short")
                .expect("read")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn system_prompt_and_tools_count_toward_the_auto_trigger() {
        let history = vec![
            user(&"a".repeat(8_000)),
            user(&"b".repeat(8_000)),
            user(&"c".repeat(8_000)),
            user(&"d".repeat(8_000)),
        ];
        let threshold = 9_000;
        assert!(
            super::compaction_split(&history, threshold, 4_000, 0).is_none(),
            "history alone is under the trigger"
        );
        assert!(
            super::compaction_split(&history, threshold, 4_000, 2_000).is_some(),
            "the same history plus the prompt and tools is over the trigger"
        );
        let tools = [mycode_core::ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            params_schema: serde_json::json!({"type": "object"}),
        }];
        let tokens = super::estimate_prefix_tokens(&["You are MYCode".to_owned()], &tools);
        assert!(tokens > 0, "{tokens}");
    }
}
