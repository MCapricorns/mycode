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
const SUMMARY_PREFIX: &str = "COMPACTION SUMMARY";

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
    history: Vec<Arc<Message>>,
    force: bool,
) -> Compacted {
    let threshold = if force {
        0
    } else {
        compaction_threshold(scope.context_window)
    };
    let Some(head_end) = compaction_split(&history, threshold, TAIL_TOKEN_BUDGET) else {
        return Compacted {
            messages: history,
            status: CompactStatus::Unchanged,
            summary: None,
        };
    };
    let prior = mycode_config::read_compaction(scope.home, scope.session_id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint.branch_id == scope.branch_id);
    // `/compact` on a head this checkpoint already covers would summarize the
    // same prefix again (same replaced count, same tail). Say so instead.
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
    // A checkpoint still applies after the head moves by appends, while the
    // uncovered tail fits. `/compact` writes that checkpoint; the next turn
    // uses it instead of waiting for the automatic threshold. A forced
    // compact summarizes again only when this head is not already covered.
    if !force
        && !already_summarized
        && let Some(checkpoint) = prior.as_ref()
        && checkpoint.covered_messages > 0
        && checkpoint.covered_messages < history.len()
        && !is_tool_result(&history[checkpoint.covered_messages])
    {
        let tail_tokens: usize = history[checkpoint.covered_messages..]
            .iter()
            .map(|message| message_tokens(message))
            .sum();
        let same_head = checkpoint.covered_head == scope.head;
        if same_head || tail_tokens <= compaction_threshold(scope.context_window) {
            return Compacted {
                messages: with_summary(
                    &checkpoint.summary,
                    &history[checkpoint.covered_messages..],
                ),
                status: CompactStatus::Unchanged,
                summary: None,
            };
        }
    }
    let covered = prior.as_ref().map(|checkpoint| checkpoint.covered_messages);
    let prior_summary = prior.as_ref().map(|checkpoint| checkpoint.summary.as_str());
    // The previous summary already stands in for `history[..covered]`.
    // Sending that prefix again makes a later `/compact` re-bill the same
    // leading messages on top of the old summary.
    let transcript =
        compaction_transcript(prior_summary, transcript_head(&history, head_end, covered));
    let summarized = tokio::time::timeout(
        SUMMARY_TIMEOUT,
        summarize_transcript(scope.wire, &transcript),
    )
    .await;
    let summary = match summarized {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => {
            eprintln!("[mycode-compaction] skipped: {message}");
            return Compacted {
                messages: history,
                status: CompactStatus::Failed(message),
                summary: None,
            };
        }
        Err(_) => {
            eprintln!("[mycode-compaction] skipped: summary timed out");
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
        eprintln!(
            "[mycode-compaction] shrunk an already summarized history in memory ({} remain)",
            compacted.len()
        );
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
        eprintln!("[mycode-compaction] checkpoint write failed: {error:?}");
        return Compacted {
            messages: history,
            status: CompactStatus::Failed(format!("checkpoint write failed: {error:?}")),
            summary: None,
        };
    }
    eprintln!(
        "[mycode-compaction] replaced {head_end} messages with a checkpoint ({} remain)",
        compacted.len()
    );
    Compacted {
        messages: compacted,
        status: CompactStatus::Wrote,
        summary: Some(shown),
    }
}

fn with_summary(summary: &str, tail: &[Arc<Message>]) -> Vec<Arc<Message>> {
    let mut compacted = Vec::with_capacity(tail.len() + 1);
    compacted.push(Arc::new(Message::User(mycode_core::UserMessage::text(
        format!("{SUMMARY_PREFIX}\n\n{summary}"),
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
        Message::Custom(custom) => "custom ".chars().count() + custom.kind.chars().count(),
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
        Message::Assistant(assistant) => blocks_text(&assistant.blocks),
        Message::ToolResult(result) => {
            let body = blocks_text(&result.content);
            format!("tool_result {} {body}", result.tool_call_id)
        }
        Message::Custom(custom) => format!("custom {}", custom.kind),
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

fn is_summary_message(message: &Message) -> bool {
    matches!(message, Message::User(user) if blocks_text(&user.content).starts_with(SUMMARY_PREFIX))
}

/// Returns the split index when compaction is due: everything before it is
/// summarized, everything from it on stays verbatim.
fn compaction_split(
    history: &[Arc<Message>],
    threshold: usize,
    tail_budget: usize,
) -> Option<usize> {
    if history.len() < 2 {
        return None;
    }
    let estimate: usize = history.iter().map(|message| message_tokens(message)).sum();
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
            Message::Custom(_) => "custom",
        };
        let text = message_text(message);
        let cut = text
            .char_indices()
            .nth(EXCERPT_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        transcript.push_str(&format!("[{role}] {}\n\n", &text[..cut]));
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

async fn summarize_transcript(wire: &WireProvider, transcript: &str) -> Result<String, String> {
    let request = Request::new()
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
        .with_message(Message::User(mycode_core::UserMessage::text(format!(
            "Summarize the following conversation for continuation:\n\n{transcript}"
        ))));
    let cancel = CancellationToken::new();
    let mut stream = wire
        .stream(&request, cancel)
        .await
        .map_err(|error| format!("summary request failed: {error:?}"))?;
    let mut summary = String::new();
    loop {
        let Some(event) = stream.next().await else {
            return Err("summary stream ended without completion".to_owned());
        };
        match event {
            StreamEvent::TextDelta(delta) => summary.push_str(&delta),
            StreamEvent::Done { .. } => break,
            StreamEvent::Error(error) => return Err(format!("summary stream failed: {error:?}")),
            _ => {}
        }
    }
    let chars: Vec<char> = summary.chars().collect();
    if chars.len() > mycode_config::MAX_SUMMARY_CHARS {
        summary = chars[..mycode_config::MAX_SUMMARY_CHARS].iter().collect();
    }
    if summary.trim().is_empty() {
        return Err("summary was empty".to_owned());
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mycode_core::{Message, UserMessage};

    use super::{
        CompactStatus, Compacted, compaction_transcript, display_summary_text,
        is_display_only_summary, transcript_head,
    };

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
}
