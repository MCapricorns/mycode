//! Helpers shared by the wire-protocol adapters.
//!
//! One place for the fragments every OpenAI-family adapter repeats: the
//! reasoning-effort body fields, text-block concatenation, and terminal
//! block assembly. Adapter-specific shapes (Anthropic's budgeted thinking,
//! per-index block accumulators) stay with their adapters.

use serde_json::{Value, json};

use mycode_core::{
    ContentBlock, ReasoningLevel, StopReason, TextBlock, ThinkingBlock, ToolCall, Usage,
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
        _ => StopReason::Stop,
    }
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
        let arguments = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
        blocks.push(ContentBlock::ToolCall(ToolCall::new(id, name, arguments)));
    }
    blocks
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
}
