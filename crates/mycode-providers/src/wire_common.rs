//! Helpers shared by the wire-protocol adapters.
//!
//! Reasoning-effort body fields (including Anthropic's budgeted thinking),
//! text-block concatenation, and terminal block assembly live here.
//! Per-index block accumulators stay with their adapters.

use serde_json::{Value, json};

use mycode_core::{
    ContentBlock, ReasoningLevel, StopReason, TextBlock, ThinkingBlock, ToolCall, Usage,
    interrupted_response_text,
};

/// Separator between ordered system-prompt parts.
pub(crate) const SYSTEM_JOIN: &str = "\n\n";

/// Ceiling for one streamed assistant payload (text, thinking, and tool JSON).
///
/// Each SSE frame is already capped. This bounds the sum so a long or
/// hostile stream cannot grow without limit before the terminal event.
pub(crate) const MAX_STREAM_ACCUMULATED_BYTES: usize = 8 * 1024 * 1024;

/// Highest content-block or tool-call index accepted in one stream.
///
/// A frame that names an enormous index would otherwise allocate that many
/// accumulator slots in a single `feed` call.
pub(crate) const MAX_STREAM_INDEX: u64 = 64;

/// Records a provider finish token.
///
/// An error token wins over a later success token and keeps the first detail.
pub(crate) fn record_provider_stop(
    current: &mut Option<StopReason>,
    interrupt: &mut Option<String>,
    token: &str,
) {
    let reason = map_stop_reason(token);
    if reason == StopReason::Error {
        if interrupt.is_none() {
            *interrupt = Some(match token {
                "refusal" => "the model declined to continue (refusal)".to_owned(),
                other => other.to_owned(),
            });
        }
        *current = Some(StopReason::Error);
    } else if *current != Some(StopReason::Error) {
        *current = Some(reason);
    }
}

/// Maps a provider finish or stop token onto the agent stop reason.
///
/// `length` / `max_tokens` must stay [`StopReason::Length`]. The agent
/// refuses to execute tool calls that were cut off by the output limit;
/// folding those tokens into `Stop` makes it run the partial arguments.
/// Anthropic's `model_context_window_exceeded` is the same cut-off. Its
/// `refusal` (a safety decline with HTTP 200) is an error: the reply may be
/// empty, and the user needs to see why the turn stopped.
#[must_use]
pub(crate) fn map_stop_reason(token: &str) -> StopReason {
    match token {
        "tool_calls" | "function_call" | "tool_use" => StopReason::ToolUse,
        "length" | "max_tokens" | "model_context_window_exceeded" => StopReason::Length,
        "error" | "network_error" | "content_filter" | "sensitive" | "refusal" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

/// Appends the shared interruption line once.
pub(crate) fn append_interruption(text: &mut String, detail: &str) {
    let note = interrupted_response_text(detail);
    if text.contains(note.as_str()) {
        return;
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&note);
}

/// Reads a provider error message from a JSON object, when one is present.
pub(crate) fn provider_error_detail(value: &Value) -> Option<String> {
    let error = value.get("error").filter(|error| !error.is_null())?;
    let detail = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or("provider error");
    Some(detail.to_owned())
}

/// Accounts `extra` bytes toward [`MAX_STREAM_ACCUMULATED_BYTES`].
///
/// Returns false once the ceiling is crossed. The caller ends the stream
/// with a protocol error instead of dispatching a partial tool call.
pub(crate) fn charge_stream(used: &mut usize, extra: usize) -> bool {
    match used.checked_add(extra) {
        Some(next) if next <= MAX_STREAM_ACCUMULATED_BYTES => {
            *used = next;
            true
        }
        _ => false,
    }
}

/// Which chat API the body is for. Responses uses `reasoning.effort`; chat
/// completions uses `reasoning_effort` for the same OpenAI-shaped models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThinkingWire {
    /// OpenAI Chat Completions and compatible gateways.
    Completions,
    /// OpenAI Responses.
    Responses,
    /// Anthropic Messages and compatible gateways.
    Anthropic,
}

/// How an assistant turn's reasoning is echoed on the next request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningReplay {
    /// Leave thinking out of the wire history.
    ///
    /// Generic OpenAI gateways reject an assistant message that is only
    /// reasoning, so those turns stay omitted.
    Omit,
    /// OpenAI-style `reasoning_content` (GLM, DeepSeek, Kimi/Moonshot).
    Content,
    /// MiniMax Chat Completions. Replay the original `reasoning_details`
    /// array when the block stored one, `reasoning_content` when that is
    /// what arrived, and a synthesized `reasoning_details` array when the
    /// only source was a `<think>` wrapper.
    MiniMax,
}

/// Applies the requested reasoning effort to an OpenAI-style body.
///
/// Field choice follows OpenCode's models.dev transform: the gateway decides
/// the shape, then the model family. Off is an explicit disable for formats
/// that otherwise stay on when the field is omitted.
pub(crate) fn apply_reasoning_effort(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    crate::family::apply_chat_thinking(body, model, endpoint, level, ThinkingWire::Completions);
}

/// Applies reasoning fields on an OpenAI Responses body.
pub(crate) fn apply_responses_thinking(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    crate::family::apply_chat_thinking(body, model, endpoint, level, ThinkingWire::Responses);
}

/// Applies Anthropic Messages thinking fields, including the output cap when
/// a budget is sent.
pub(crate) fn apply_anthropic_thinking(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    crate::family::apply_anthropic_thinking_policy(body, model, endpoint, level);
}

/// Parameters object placed on a tool definition.
///
/// Every provider gets the same expansion. OpenCode `schema()` in
/// `packages/opencode/src/provider/transform.ts` does not: `sanitizeMoonshot`
/// keeps a bare `$ref` (Moonshot expands it and rejects sibling keywords),
/// and `sanitizeGemini` copies `$ref` through. Moonshot's coding-plan
/// validator still reports infinite recursion on schemars `$ref` + `$defs`
/// at `operations.items`, so the refs are inlined and the def buckets are
/// dropped before the body is sent.
pub(crate) fn tool_parameters(tool: &mycode_core::ToolSpec) -> Value {
    let mut schema = tool.params_schema.clone();
    mycode_core::inline_schema_refs(&mut schema);
    schema
}

pub(crate) fn token_count(value: &Value, keys: &[&str]) -> u64 {
    for key in keys {
        let Some(field) = value.get(*key) else {
            continue;
        };
        if let Some(count) = field.as_u64() {
            return count;
        }
        if let Some(count) = field.as_i64().filter(|count| *count >= 0) {
            return count as u64;
        }
        if let Some(count) = field
            .as_f64()
            .filter(|count| count.is_finite() && *count >= 0.0)
        {
            return count as u64;
        }
        if let Some(text) = field.as_str()
            && let Ok(count) = text.trim().parse::<u64>()
        {
            return count;
        }
    }
    0
}

/// Builds usage from OpenAI, Anthropic, DeepSeek, or Gemini field names.
///
/// `prompt_tokens` is the context-meter size. OpenAI `prompt_tokens` and
/// Responses `input_tokens` already include cache reads. Anthropic
/// `input_tokens` does not, so a payload that carries Anthropic cache fields
/// sums the uncached input with cache reads and cache writes.
pub(crate) fn usage_from_value(usage: &Value) -> Usage {
    let input_tokens = token_count(usage, crate::family::USAGE_INPUT_KEYS);
    let output_tokens = token_count(usage, crate::family::USAGE_OUTPUT_KEYS);
    let cache_read = cache_read_count(usage);
    let cache_write = cache_write_count(usage);
    let anthropic_split = usage.get("cache_read_input_tokens").is_some()
        || usage.get("cache_creation_input_tokens").is_some()
        || usage.get("cache_creation").is_some()
        || usage.get("cache_write_input_tokens").is_some();
    let prompt_tokens = if anthropic_split {
        input_tokens
            .saturating_add(cache_read)
            .saturating_add(cache_write)
    } else {
        input_tokens
    };
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: (cache_read > 0).then_some(cache_read),
        cache_write_tokens: (cache_write > 0).then_some(cache_write),
        prompt_tokens,
    }
}

fn cache_read_count(usage: &Value) -> u64 {
    let direct = token_count(usage, crate::family::USAGE_CACHE_READ_KEYS);
    let nested = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .map(|details| token_count(details, crate::family::USAGE_CACHE_READ_NESTED_KEYS))
        .unwrap_or(0);
    direct.max(nested)
}

fn cache_write_count(usage: &Value) -> u64 {
    let direct = token_count(usage, crate::family::USAGE_CACHE_WRITE_KEYS);
    let nested_details = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .map(|details| token_count(details, crate::family::USAGE_CACHE_WRITE_NESTED_KEYS))
        .unwrap_or(0);
    let nested_creation = usage
        .get("cache_creation")
        .map(|creation| {
            crate::family::USAGE_CACHE_WRITE_EPHEMERAL_KEYS
                .iter()
                .fold(0u64, |sum, key| {
                    sum.saturating_add(token_count(creation, &[key]))
                })
        })
        .unwrap_or(0);
    direct.max(nested_details).max(nested_creation)
}

/// Keeps a non-zero count when a later partial usage object reports 0.
pub(crate) fn merge_usage(previous: Option<Usage>, next: Usage) -> Usage {
    let Some(previous) = previous else {
        return next;
    };
    Usage {
        input_tokens: if next.input_tokens > 0 {
            next.input_tokens
        } else {
            previous.input_tokens
        },
        output_tokens: if next.output_tokens > 0 {
            next.output_tokens
        } else {
            previous.output_tokens
        },
        cache_read_tokens: next.cache_read_tokens.or(previous.cache_read_tokens),
        cache_write_tokens: next.cache_write_tokens.or(previous.cache_write_tokens),
        prompt_tokens: if next.prompt_tokens > 0 {
            next.prompt_tokens
        } else {
            previous.prompt_tokens
        },
    }
}

/// Concatenates the text of every text block, in order.
pub(crate) fn join_text(blocks: &[ContentBlock]) -> String {
    let mut joined = String::new();
    for block in blocks {
        if let ContentBlock::Text(text) = block {
            joined.push_str(&text.text);
        }
    }
    joined
}

/// Concatenates non-empty thinking blocks, separated by newlines.
pub(crate) fn join_thinking(blocks: &[ContentBlock]) -> String {
    let mut joined = String::new();
    for block in blocks {
        let ContentBlock::Thinking(thinking) = block else {
            continue;
        };
        if thinking.text.is_empty() {
            continue;
        }
        if !joined.is_empty() {
            joined.push('\n');
        }
        joined.push_str(&thinking.text);
    }
    joined
}

/// Assembles terminal content blocks: optional thinking, optional text, then
/// one tool-call block per stitched call. Arguments parse as JSON and default
/// to an empty object when a vendor streams an invalid fragment.
pub(crate) fn assemble_blocks<'a>(
    thinking: &str,
    text: &str,
    calls: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<ContentBlock> {
    assemble_blocks_with_replay(thinking, text, calls, None)
}

/// Same as [`assemble_blocks`], plus a MiniMax replay blob on the thinking
/// block. `replay` is the JSON object stored on [`ThinkingBlock::replay`].
pub(crate) fn assemble_blocks_with_replay<'a>(
    thinking: &str,
    text: &str,
    calls: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    replay: Option<&str>,
) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if !thinking.is_empty() || replay.is_some_and(|value| !value.is_empty()) {
        let mut block = ThinkingBlock::new(thinking);
        if let Some(replay) = replay.filter(|value| !value.is_empty()) {
            block = block.with_replay(replay);
        }
        blocks.push(ContentBlock::Thinking(block));
    }
    if !text.is_empty() {
        blocks.push(ContentBlock::Text(TextBlock::new(text)));
    }
    for (id, name, arguments) in calls {
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let arguments = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
        blocks.push(ContentBlock::ToolCall(ToolCall::new(id, name, arguments)));
    }
    blocks
}

/// Stop reason for an assembled message. An interruption wins over tool use
/// so a partial call is not executed.
pub(crate) fn assembled_stop_reason(
    interrupted: bool,
    has_calls: bool,
    stop_reason: Option<StopReason>,
) -> StopReason {
    if interrupted || stop_reason == Some(StopReason::Error) {
        StopReason::Error
    } else if has_calls && stop_reason != Some(StopReason::Length) {
        StopReason::ToolUse
    } else {
        stop_reason.unwrap_or(StopReason::Stop)
    }
}

#[cfg(test)]
mod tests;
