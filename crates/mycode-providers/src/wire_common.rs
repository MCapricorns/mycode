//! Helpers shared by the wire-protocol adapters.
//!
//! One place for the fragments every OpenAI-family adapter repeats: the
//! reasoning-effort body fields, text-block concatenation, and terminal
//! block assembly. Adapter-specific shapes (Anthropic's budgeted thinking,
//! per-index block accumulators) stay with their adapters.

use serde_json::{Value, json};

use mycode_core::{
    ContentBlock, ReasoningLevel, StopReason, TextBlock, ThinkingBlock, ToolCall, Usage,
    interrupted_response_text,
};

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

/// Maps a provider finish or stop token onto the agent stop reason.
///
/// `length` / `max_tokens` must stay [`StopReason::Length`]. The agent
/// refuses to execute tool calls that were cut off by the output limit;
/// folding those tokens into `Stop` makes it run the partial arguments.
#[must_use]
pub(crate) fn map_stop_reason(token: &str) -> StopReason {
    match token {
        "tool_calls" | "function_call" | "tool_use" => StopReason::ToolUse,
        "length" | "max_tokens" => StopReason::Length,
        "error" | "network_error" | "content_filter" | "sensitive" => StopReason::Error,
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

/// MiniMax (including minimaxi.com) rejects `thinking.type = "enabled"`.
///
/// Model ids such as `MiniMax-M2.5` and hosts such as `api.minimax.cn` /
/// `api.minimaxi.com` select this vendor. Other providers keep `enabled`.
pub(crate) fn minimax_target(model: &str, endpoint: &str) -> bool {
    model.to_ascii_lowercase().contains("minimax")
        || endpoint.to_ascii_lowercase().contains("minimax")
}

/// Zhipu / Z.AI / BigModel, including a model id that contains `glm`.
pub(crate) fn glm_target(model: &str, endpoint: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let endpoint = endpoint.to_ascii_lowercase();
    model.contains("glm")
        || endpoint.contains("bigmodel")
        || endpoint.contains("z.ai")
        || endpoint.contains("zhipu")
}

/// GLM-5 keeps thinking on. `thinking.type = "disabled"` is rejected.
pub(crate) fn glm_cannot_disable(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        if bytes[index..].starts_with(b"glm") {
            let after = lower[index + 3..].trim_start_matches(['-', '_', '.']);
            if let Some(stripped) = after.strip_prefix('5') {
                let boundary = stripped.chars().next();
                if boundary.is_none_or(|ch| !ch.is_ascii_digit()) {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

/// How an assistant turn's reasoning is echoed on the next request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningReplay {
    /// Leave thinking out of the wire history.
    ///
    /// This is the MiniMax and generic OpenAI shape. MiniMax-M3 with
    /// thinking on already keeps multi-turn replies without an extra
    /// reasoning field, so that request is left as it was.
    Omit,
    /// OpenAI-style `reasoning_content` (GLM, DeepSeek, Kimi/Moonshot).
    Content,
}

/// Vendors that reject a follow-up unless prior reasoning is echoed.
pub(crate) fn reasoning_replay(model: &str, endpoint: &str) -> ReasoningReplay {
    // MiniMax stays on the omit path even though the name would not match
    // the GLM/DeepSeek checks. A successful MiniMax turn must not grow a
    // new reasoning field.
    if minimax_target(model, endpoint) {
        return ReasoningReplay::Omit;
    }
    let model_lower = model.to_ascii_lowercase();
    let endpoint_lower = endpoint.to_ascii_lowercase();
    if glm_target(model, endpoint)
        || model_lower.contains("deepseek")
        || endpoint_lower.contains("deepseek")
        || model_lower.contains("kimi")
        || model_lower.contains("moonshot")
        || endpoint_lower.contains("moonshot")
    {
        return ReasoningReplay::Content;
    }
    ReasoningReplay::Omit
}

/// Applies GLM thinking fields.
///
/// Effort levels enable thinking and, on chat-completions bodies, also set
/// `reasoning_effort`. Anthropic GLM bodies must not send `budget_tokens`.
/// `glm-5` cannot be switched off.
pub(crate) fn apply_glm_thinking(
    body: &mut Value,
    model: &str,
    level: ReasoningLevel,
    effort_field: bool,
) {
    match level {
        ReasoningLevel::Off if glm_cannot_disable(model) => {
            body["thinking"] = json!({ "type": "enabled" });
        }
        ReasoningLevel::Off => {
            body["thinking"] = json!({ "type": "disabled" });
        }
        ReasoningLevel::On => {
            body["thinking"] = json!({ "type": "enabled" });
        }
        other => {
            body["thinking"] = json!({ "type": "enabled" });
            if effort_field && let Some(token) = other.effort_token() {
                body["reasoning_effort"] = json!(token);
            }
        }
    }
}

/// Applies the requested reasoning effort to an OpenAI-style body.
///
/// Thinking On is `thinking.type = "enabled"` except on MiniMax, where the
/// accepted on-value is `"adaptive"`. Off stays `"disabled"` for every vendor.
pub(crate) fn apply_reasoning_effort(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    if glm_target(model, endpoint) {
        apply_glm_thinking(body, model, level, true);
        return;
    }
    match level {
        ReasoningLevel::Off => {
            body["reasoning_effort"] = json!("none");
            body["thinking"] = json!({ "type": "disabled" });
        }
        ReasoningLevel::On => {
            let thinking_type = if minimax_target(model, endpoint) {
                "adaptive"
            } else {
                "enabled"
            };
            body["thinking"] = json!({ "type": thinking_type });
        }
        other => {
            if let Some(token) = other.effort_token() {
                body["reasoning_effort"] = json!(token);
            }
        }
    }
}

/// Reads a token count from the first present alias.
///
/// Gateways disagree on names (`prompt_tokens` vs `input_tokens`) and on
/// whether the number is a JSON number or a string. A missing field is 0.
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

/// Builds usage from either OpenAI or Responses field names.
pub(crate) fn usage_from_value(usage: &Value) -> Usage {
    let cache = token_count(
        usage,
        &[
            "cache_read_tokens",
            "cache_read_input_tokens",
            "cached_tokens",
        ],
    )
    .max(
        usage
            .get("prompt_tokens_details")
            .or_else(|| usage.get("input_tokens_details"))
            .map(|details| token_count(details, &["cached_tokens", "cache_read_tokens"]))
            .unwrap_or(0),
    );
    Usage {
        input_tokens: token_count(usage, &["input_tokens", "prompt_tokens", "input"]),
        output_tokens: token_count(usage, &["output_tokens", "completion_tokens", "output"]),
        cache_read_tokens: (cache > 0).then_some(cache),
    }
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
    }
}

/// Concatenates the text of every text block, in order.
pub(crate) fn join_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
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
    let mut blocks = Vec::new();
    if !thinking.is_empty() {
        blocks.push(ContentBlock::Thinking(ThinkingBlock::new(thinking)));
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
mod tests {
    use mycode_core::{ReasoningLevel, Request};

    use super::{apply_reasoning_effort, minimax_target};

    #[test]
    fn minimax_thinking_on_is_adaptive_not_enabled() {
        let mut by_model = serde_json::json!({});
        apply_reasoning_effort(
            &mut by_model,
            "MiniMax-M2.5",
            "https://api.example.com/v1/chat/completions",
            ReasoningLevel::On,
        );
        assert_eq!(by_model["thinking"]["type"], "adaptive");
        assert_ne!(by_model["thinking"]["type"], "enabled");

        let mut by_host = serde_json::json!({});
        apply_reasoning_effort(
            &mut by_host,
            "M2.5",
            "https://api.minimaxi.com/v1/chat/completions",
            ReasoningLevel::On,
        );
        assert_eq!(by_host["thinking"]["type"], "adaptive");
        assert!(minimax_target(
            "M2.5",
            "https://api.minimax.cn/anthropic/v1/messages"
        ));
    }

    #[test]
    fn other_providers_keep_thinking_on_enabled() {
        let mut body = serde_json::json!({});
        apply_reasoning_effort(
            &mut body,
            "gpt-5",
            "https://api.openai.com/v1/chat/completions",
            ReasoningLevel::On,
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(!minimax_target(
            "deepseek-reasoner",
            "https://api.deepseek.com/v1/chat/completions"
        ));
    }

    #[test]
    fn completions_and_messages_bodies_use_the_minimax_on_mapping() {
        let on = Request::new().with_reasoning(ReasoningLevel::On);
        let completions = crate::openai_completions::build_body(
            "MiniMax-M2.5",
            "https://api.minimaxi.com/v1/chat/completions",
            &on,
        );
        assert_eq!(completions["thinking"]["type"], "adaptive");
        assert_ne!(completions["thinking"]["type"], "enabled");

        let responses = crate::openai_responses::build_body(
            "minimax/minimax-m2",
            "https://api.example.com/responses",
            &on,
        );
        assert_eq!(responses["thinking"]["type"], "adaptive");

        let minimax_messages = crate::anthropic_messages::build_body(
            "MiniMax-M2.5",
            "https://api.minimax.cn/anthropic/v1/messages",
            &on,
        );
        assert_eq!(minimax_messages["thinking"]["type"], "adaptive");
        assert!(minimax_messages["thinking"].get("budget_tokens").is_none());

        let claude = crate::anthropic_messages::build_body(
            "claude-sonnet-4-6",
            "https://api.anthropic.com/v1/messages",
            &on,
        );
        assert_eq!(claude["thinking"]["type"], "enabled");
        assert!(claude["thinking"]["budget_tokens"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn glm_effort_enables_thinking_and_glm5_cannot_disable() {
        let mut high = serde_json::json!({});
        apply_reasoning_effort(
            &mut high,
            "glm-4.7",
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            ReasoningLevel::High,
        );
        assert_eq!(high["thinking"]["type"], "enabled");
        assert_eq!(high["reasoning_effort"], "high");

        let mut off = serde_json::json!({});
        apply_reasoning_effort(
            &mut off,
            "glm-4.7",
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            ReasoningLevel::Off,
        );
        assert_eq!(off["thinking"]["type"], "disabled");
        assert!(off.get("reasoning_effort").is_none());

        let mut locked = serde_json::json!({});
        apply_reasoning_effort(
            &mut locked,
            "glm-5.3",
            "https://api.z.ai/api/paas/v4/chat/completions",
            ReasoningLevel::Off,
        );
        assert_eq!(locked["thinking"]["type"], "enabled");
        assert!(locked.get("reasoning_effort").is_none());
        assert!(super::glm_cannot_disable("GLM-5"));
        assert!(!super::glm_cannot_disable("glm-4.7"));
        assert!(!super::glm_cannot_disable("glm-50"));
    }

    #[test]
    fn glm_messages_body_has_no_budget_tokens() {
        let high = Request::new().with_reasoning(ReasoningLevel::High);
        let body = crate::anthropic_messages::build_body(
            "glm-4.7",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            &high,
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"].get("budget_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }
}
