//! The summarizer request: output budget, prompt text, and the retry that
//! follows a summary cut off at the cap.

use super::*;
pub(super) fn summary_max_output(model: &str) -> u64 {
    if mycode_agent::keeps_thinking_when_off(model) {
        SUMMARY_OUTPUT_WITH_THINKING
    } else {
        SUMMARY_OUTPUT_TOKENS
    }
}

/// Sizes for one summary request. It carries the transcript and asks for
/// the summary within one window, so a small window gets a shorter
/// transcript and lower output caps instead of a rejected request.
pub(super) struct SummarySpec {
    /// Length the prompt asks for, in words.
    pub(super) words: usize,
    /// Transcript the request may carry, in provider tokens.
    pub(super) transcript_tokens: usize,
    /// Output cap of the first attempt.
    pub(super) first_output: u64,
    /// Output cap of the one retry after a truncated summary.
    pub(super) retry_output: u64,
}

impl SummarySpec {
    pub(super) fn new(model: &str, context_window: u64, words: usize) -> Self {
        let usable = sizing::usable_window(context_window);
        let cap = summary_max_output(model);
        // A model that keeps thinking on spends output on it, so it gets a
        // larger share of the window.
        let share = if cap > SUMMARY_OUTPUT_TOKENS { 2 } else { 4 };
        let first_output = cap.min((usable / share).max(MIN_SUMMARY_OUTPUT_TOKENS));
        let input = usable
            .saturating_sub(first_output)
            .saturating_sub(SUMMARY_PROMPT_TOKENS as u64);
        let transcript_tokens = usize::try_from(input)
            .unwrap_or(usize::MAX)
            .min(MAX_TRANSCRIPT_TOKENS);
        let retry_room = usable.saturating_sub((transcript_tokens + SUMMARY_PROMPT_TOKENS) as u64);
        Self {
            words,
            transcript_tokens,
            first_output,
            retry_output: SUMMARY_OUTPUT_RETRY_TOKENS.min(retry_room.max(first_output)),
        }
    }
}

pub(super) enum SummaryAttempt {
    Complete(String),
    Truncated,
    TooShort,
}

pub(super) async fn summarize_transcript(
    wire: &WireProvider,
    transcript: &str,
    spec: &SummarySpec,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let attempt = |output| request_summary(wire, transcript, spec.words, output, cancel);
    match attempt(spec.first_output).await? {
        SummaryAttempt::Complete(summary) => Ok(summary),
        SummaryAttempt::Truncated => finish_summary_retry(attempt(spec.retry_output).await?),
        SummaryAttempt::TooShort => finish_summary_retry(attempt(spec.first_output).await?),
    }
}

pub(super) fn finish_summary_retry(attempt: SummaryAttempt) -> Result<String, String> {
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
pub(super) fn summary_user_text(transcript: &str) -> String {
    let body = transcript.trim_end_matches(['\r', '\n']);
    format!(
        "Summarize the following conversation for continuation:\n\n\
         <conversation>\n\
         {body}\n\
         </conversation>\n\n\
         {SUMMARY_REMINDER}"
    )
}

pub(super) fn summary_request(transcript: &str, words: usize, max_output_tokens: u64) -> Request {
    let mut request = Request::new()
        .with_system_prompt(format!(
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
language the user writes in. Stay under about {words} words; drop detail that the next steps \
do not need."
        ))
        .with_message(Message::User(mycode_agent::UserMessage::text(
            summary_user_text(transcript),
        )));
    // `Off` is applied by the provider adapter: Anthropic and GLM/Z.AI send
    // `thinking.type = disabled`, Qwen sends `enable_thinking: false`, and
    // the other families use their own off or lowest-effort field.
    request.reasoning = Some(mycode_agent::ReasoningLevel::Off);
    request.max_output_tokens = Some(max_output_tokens);
    request
}

async fn request_summary(
    wire: &WireProvider,
    transcript: &str,
    words: usize,
    max_output_tokens: u64,
    cancel: &CancellationToken,
) -> Result<SummaryAttempt, String> {
    let request = summary_request(transcript, words, max_output_tokens);
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
    if stop == mycode_agent::StopReason::Length {
        return Ok(SummaryAttempt::Truncated);
    }
    if stop == mycode_agent::StopReason::Error {
        return Err("summary stream failed".to_owned());
    }
    let summary = elide(&summary, MAX_MODEL_SUMMARY_CHARS);
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err("summary was empty".to_owned());
    }
    if trimmed.chars().count() < MIN_SUMMARY_CHARS {
        return Ok(SummaryAttempt::TooShort);
    }
    Ok(SummaryAttempt::Complete(summary))
}
