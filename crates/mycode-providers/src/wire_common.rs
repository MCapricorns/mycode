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
    apply_thinking_request(body, model, endpoint, level, ThinkingWire::Completions);
}

/// Applies reasoning fields on an OpenAI Responses body.
pub(crate) fn apply_responses_thinking(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    apply_thinking_request(body, model, endpoint, level, ThinkingWire::Responses);
}

/// Applies Anthropic Messages thinking fields, including the output cap when
/// a budget is sent.
pub(crate) fn apply_anthropic_thinking(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    apply_thinking_request(body, model, endpoint, level, ThinkingWire::Anthropic);
}

fn apply_thinking_request(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
    wire: ThinkingWire,
) {
    if wire == ThinkingWire::Anthropic {
        apply_anthropic_body(body, model, endpoint, level);
        return;
    }
    apply_chat_body(body, model, endpoint, level, wire);
}

fn apply_chat_body(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
    wire: ThinkingWire,
) {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    if host.contains("openrouter.ai") {
        apply_openrouter(body, level);
        return;
    }
    if dashscope_host(&host) {
        apply_dashscope(body, &model_id, level);
        return;
    }
    if zai_host(&host) || model_id.contains("glm") {
        apply_zai(body, level);
        return;
    }
    if minimax_target(model, endpoint) {
        apply_minimax(body, level);
        return;
    }
    if kimi_family(&model_id, &host) {
        apply_kimi_chat(body, &model_id, level);
        return;
    }
    if model_id.contains("qwen") || model_id.contains("qwq") {
        apply_enable_thinking(body, level);
        if let Some(token) = effort_token(level) {
            body["reasoning_effort"] = json!(token);
        }
        return;
    }
    if model_id.contains("deepseek") || host.contains("deepseek") {
        apply_deepseek(body, &model_id, level);
        return;
    }
    apply_openai_effort(body, &model_id, level, wire);
}

fn apply_anthropic_body(body: &mut Value, model: &str, endpoint: &str, level: ReasoningLevel) {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    if minimax_target(model, endpoint) {
        apply_minimax(body, level);
        return;
    }
    if kimi_family(&model_id, &host) {
        if level == ReasoningLevel::Off {
            body["thinking"] = json!({ "type": "disabled" });
            return;
        }
        body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        body["output_config"] = json!({ "effort": adaptive_effort(level) });
        return;
    }
    if zai_host(&host) || model_id.contains("glm") {
        // Anthropic-compatible Z.AI rejects `budget_tokens` and `clear_thinking`.
        if level == ReasoningLevel::Off {
            body["thinking"] = json!({ "type": "disabled" });
        } else {
            body["thinking"] = json!({ "type": "enabled" });
        }
        return;
    }
    if claude_adaptive(&model_id) {
        if level == ReasoningLevel::Off {
            body["thinking"] = json!({ "type": "disabled" });
            return;
        }
        let mut thinking = json!({ "type": "adaptive" });
        if claude_summarized_display(&model_id) {
            thinking["display"] = json!("summarized");
        }
        body["thinking"] = thinking;
        body["output_config"] = json!({ "effort": adaptive_effort(level) });
        return;
    }
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
    let budget = match level {
        ReasoningLevel::Minimal | ReasoningLevel::Low => 1_024,
        ReasoningLevel::On | ReasoningLevel::Medium => 4_096,
        ReasoningLevel::High => 16_384,
        ReasoningLevel::Xhigh | ReasoningLevel::Max => 32_768,
        ReasoningLevel::Off => 0,
    };
    let max_tokens = body["max_tokens"].as_u64().unwrap_or(4_096);
    if max_tokens <= budget {
        body["max_tokens"] = json!(budget + 4_096);
    }
    body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
}

fn apply_openrouter(body: &mut Value, level: ReasoningLevel) {
    let effort = match level {
        ReasoningLevel::Off => "none",
        ReasoningLevel::On => "high",
        other => other.effort_token().unwrap_or("high"),
    };
    body["reasoning"] = json!({ "effort": effort });
}

fn apply_dashscope(body: &mut Value, model_id: &str, level: ReasoningLevel) {
    // DashScope defaults `kimi-k2-thinking` on. Every other reasoning model
    // on this host, including Kimi, GLM, Qwen, and DeepSeek, needs the flag.
    if level == ReasoningLevel::Off {
        body["enable_thinking"] = json!(false);
    } else if !model_id.contains("kimi-k2-thinking") {
        body["enable_thinking"] = json!(true);
    }
    if let Some(token) = effort_token(level) {
        body["reasoning_effort"] = json!(token);
    }
}

fn apply_zai(body: &mut Value, level: ReasoningLevel) {
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
    body["thinking"] = json!({ "type": "enabled", "clear_thinking": false });
    if let Some(token) = effort_token(level) {
        body["reasoning_effort"] = json!(token);
    }
}

fn apply_minimax(body: &mut Value, level: ReasoningLevel) {
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
    body["thinking"] = json!({ "type": "adaptive" });
}

fn apply_kimi_chat(body: &mut Value, model_id: &str, level: ReasoningLevel) {
    // Kimi K3 publishes effort values. The rest of the family is a thinking
    // toggle and rejects `reasoning_effort`.
    if (model_id.contains("kimi-k3") || model_id.ends_with("/k3") || model_id == "k3")
        && let Some(token) = effort_token(level)
    {
        body["reasoning_effort"] = json!(token);
        return;
    }
    // K2.7 Code rejects `{type:"disabled"}`. Leaving the field off matches
    // that API and OpenCode, which has no off variant for it.
    if level == ReasoningLevel::Off && model_id.contains("kimi-k2.7-code") {
        return;
    }
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
    } else {
        body["thinking"] = json!({ "type": "enabled" });
    }
}

fn apply_enable_thinking(body: &mut Value, level: ReasoningLevel) {
    body["enable_thinking"] = json!(level != ReasoningLevel::Off);
}

fn apply_deepseek(body: &mut Value, model_id: &str, level: ReasoningLevel) {
    if model_id.contains("deepseek-v4") {
        let effort = match level {
            ReasoningLevel::Off => "none",
            ReasoningLevel::On => "high",
            other => other.effort_token().unwrap_or("high"),
        };
        body["reasoning_effort"] = json!(effort);
        return;
    }
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
    } else {
        body["thinking"] = json!({ "type": "enabled" });
    }
}

fn apply_openai_effort(
    body: &mut Value,
    model_id: &str,
    level: ReasoningLevel,
    wire: ThinkingWire,
) {
    let effort = match level {
        ReasoningLevel::Off => "none",
        ReasoningLevel::On if gpt5_defaults_medium(model_id) => "medium",
        ReasoningLevel::On => return,
        other => other.effort_token().unwrap_or("medium"),
    };
    if wire == ThinkingWire::Responses {
        body["reasoning"] = json!({ "effort": effort });
    } else {
        body["reasoning_effort"] = json!(effort);
    }
}

fn effort_token(level: ReasoningLevel) -> Option<&'static str> {
    match level {
        ReasoningLevel::Off | ReasoningLevel::On => None,
        other => other.effort_token(),
    }
}

fn adaptive_effort(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Off => "low",
        ReasoningLevel::On | ReasoningLevel::High => "high",
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::Xhigh => "xhigh",
        ReasoningLevel::Max => "max",
    }
}

fn dashscope_host(host: &str) -> bool {
    host.contains("dashscope") || host.contains("aliyuncs")
}

fn zai_host(host: &str) -> bool {
    host.contains("z.ai") || host.contains("bigmodel") || host.contains("zhipu")
}

/// Every Kimi and Moonshot model, not a short id list.
fn kimi_family(model_id: &str, host: &str) -> bool {
    model_id.contains("kimi")
        || model_id.contains("moonshot")
        || model_id.contains("k2p")
        || host.contains("api.kimi.com")
        || host.contains("moonshot.ai")
        || host.contains("moonshot.cn")
        || host.contains("moonshotai.cn")
}

fn gpt5_defaults_medium(model_id: &str) -> bool {
    model_id.contains("gpt-5")
        && !model_id.contains("gpt-5-chat")
        && !model_id.contains("gpt-5-pro")
}

fn claude_adaptive(model_id: &str) -> bool {
    const MARKERS: &[&str] = &[
        "opus-4-6",
        "opus-4.6",
        "4-6-opus",
        "4.6-opus",
        "sonnet-4-6",
        "sonnet-4.6",
        "4-6-sonnet",
        "4.6-sonnet",
    ];
    if MARKERS.iter().any(|marker| model_id.contains(marker)) {
        return true;
    }
    claude_summarized_display(model_id)
}

fn claude_summarized_display(model_id: &str) -> bool {
    let Some((major, minor)) = claude_version(model_id) else {
        return false;
    };
    major > 4 || (major == 4 && minor >= 7)
}

/// `claude-opus-4.7` and `claude-4.7-opus` both parse. An 8-digit release
/// date after the major is not a minor version.
fn claude_version(model_id: &str) -> Option<(u32, u32)> {
    let rest = model_id.split("claude-").nth(1)?;
    let rest = if rest.starts_with(|ch: char| ch.is_ascii_digit()) {
        rest
    } else {
        rest.split_once('-').map(|(_, after)| after)?
    };
    let major_len = rest
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(rest.len());
    if major_len == 0 {
        return None;
    }
    let major = rest[..major_len].parse().ok()?;
    let rest = &rest[major_len..];
    let minor = if let Some(digits) = rest.strip_prefix(['.', '-']) {
        let len = digits
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(digits.len());
        if (1..=2).contains(&len) {
            digits[..len].parse().unwrap_or(0)
        } else {
            0
        }
    } else {
        0
    };
    Some((major, minor))
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

/// Builds usage from OpenAI, Anthropic, DeepSeek, or Gemini field names.
///
/// `prompt_tokens` is the context-meter size. OpenAI `prompt_tokens` and
/// Responses `input_tokens` already include cache reads. Anthropic
/// `input_tokens` does not, so a payload that carries Anthropic cache fields
/// sums the uncached input with cache reads and cache writes.
pub(crate) fn usage_from_value(usage: &Value) -> Usage {
    let input_tokens = token_count(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "input",
            "promptTokenCount",
            "prompt_token_count",
        ],
    );
    let output_tokens = token_count(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "output",
            "candidatesTokenCount",
            "candidates_token_count",
        ],
    );
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
    let direct = token_count(
        usage,
        &[
            "cache_read_tokens",
            "cache_read_input_tokens",
            "cached_tokens",
            "prompt_cache_hit_tokens",
            "cachedContentTokenCount",
            "cached_content_token_count",
        ],
    );
    let nested = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .map(|details| token_count(details, &["cached_tokens", "cache_read_tokens"]))
        .unwrap_or(0);
    direct.max(nested)
}

fn cache_write_count(usage: &Value) -> u64 {
    let direct = token_count(
        usage,
        &[
            "cache_creation_input_tokens",
            "cache_write_input_tokens",
            "cache_write_tokens",
        ],
    );
    let nested_details = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .map(|details| {
            token_count(
                details,
                &["cache_write_tokens", "cache_creation_input_tokens"],
            )
        })
        .unwrap_or(0);
    let nested_creation = usage
        .get("cache_creation")
        .map(|creation| {
            token_count(creation, &["ephemeral_5m_input_tokens"])
                .saturating_add(token_count(creation, &["ephemeral_1h_input_tokens"]))
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
    fn openai_on_is_reasoning_effort_without_thinking_type() {
        let mut body = serde_json::json!({});
        apply_reasoning_effort(
            &mut body,
            "gpt-5",
            "https://api.openai.com/v1/chat/completions",
            ReasoningLevel::On,
        );
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("thinking").is_none());

        let mut off = serde_json::json!({});
        apply_reasoning_effort(
            &mut off,
            "gpt-5",
            "https://api.openai.com/v1/chat/completions",
            ReasoningLevel::Off,
        );
        assert_eq!(off["reasoning_effort"], "none");
        assert!(off.get("thinking").is_none());
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
        assert_eq!(claude["thinking"]["type"], "adaptive");
        assert!(claude["thinking"].get("budget_tokens").is_none());
        assert_eq!(claude["output_config"]["effort"], "high");

        let older = crate::anthropic_messages::build_body(
            "claude-sonnet-4-5",
            "https://api.anthropic.com/v1/messages",
            &on,
        );
        assert_eq!(older["thinking"]["type"], "enabled");
        assert!(older["thinking"]["budget_tokens"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn glm_effort_enables_thinking_and_off_is_explicit() {
        let mut high = serde_json::json!({});
        apply_reasoning_effort(
            &mut high,
            "glm-4.7",
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            ReasoningLevel::High,
        );
        assert_eq!(high["thinking"]["type"], "enabled");
        assert_eq!(high["thinking"]["clear_thinking"], false);
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

        let mut glm5 = serde_json::json!({});
        apply_reasoning_effort(
            &mut glm5,
            "glm-5.3",
            "https://api.z.ai/api/paas/v4/chat/completions",
            ReasoningLevel::Off,
        );
        assert_eq!(glm5["thinking"]["type"], "disabled");
        assert!(glm5.get("reasoning_effort").is_none());
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
        assert!(body["thinking"].get("clear_thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn dashscope_enables_thinking_for_every_model_family() {
        for model in [
            "qwen3.7-max",
            "kimi-k2.5",
            "kimi-k2.6",
            "glm-5",
            "deepseek-v4-pro",
            "MiniMax-M2.5",
        ] {
            let mut body = serde_json::json!({});
            apply_reasoning_effort(
                &mut body,
                model,
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                ReasoningLevel::On,
            );
            assert_eq!(body["enable_thinking"], true, "{model}");
            assert!(body.get("thinking").is_none(), "{model}");
        }

        let mut always_on = serde_json::json!({});
        apply_reasoning_effort(
            &mut always_on,
            "kimi-k2-thinking",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            ReasoningLevel::On,
        );
        assert!(always_on.get("enable_thinking").is_none());

        let mut off = serde_json::json!({});
        apply_reasoning_effort(
            &mut off,
            "kimi-k2.6",
            "https://coding.dashscope.aliyuncs.com/v1",
            ReasoningLevel::Off,
        );
        assert_eq!(off["enable_thinking"], false);
    }

    #[test]
    fn openrouter_uses_reasoning_effort_object() {
        let mut off = serde_json::json!({});
        apply_reasoning_effort(
            &mut off,
            "z-ai/glm-5",
            "https://openrouter.ai/api/v1/chat/completions",
            ReasoningLevel::Off,
        );
        assert_eq!(off["reasoning"]["effort"], "none");
        assert!(off.get("thinking").is_none());

        let mut high = serde_json::json!({});
        apply_reasoning_effort(
            &mut high,
            "moonshotai/kimi-k2.6",
            "https://openrouter.ai/api/v1/chat/completions",
            ReasoningLevel::High,
        );
        assert_eq!(high["reasoning"]["effort"], "high");
        assert!(high.get("thinking").is_none());
    }

    #[test]
    fn every_kimi_chat_model_uses_the_family_shape() {
        for model in [
            "kimi-k2.5",
            "kimi-k2.6",
            "moonshot-v1-128k",
            "kimi-k2-thinking",
        ] {
            let mut on = serde_json::json!({});
            apply_reasoning_effort(
                &mut on,
                model,
                "https://api.moonshot.cn/v1/chat/completions",
                ReasoningLevel::On,
            );
            assert_eq!(on["thinking"]["type"], "enabled", "{model}");
            assert!(on.get("reasoning_effort").is_none(), "{model}");

            let mut off = serde_json::json!({});
            apply_reasoning_effort(
                &mut off,
                model,
                "https://api.moonshot.ai/v1/chat/completions",
                ReasoningLevel::Off,
            );
            assert_eq!(off["thinking"]["type"], "disabled", "{model}");
        }

        let mut code_off = serde_json::json!({});
        apply_reasoning_effort(
            &mut code_off,
            "kimi-k2.7-code-highspeed",
            "https://api.moonshot.cn/v1/chat/completions",
            ReasoningLevel::Off,
        );
        assert!(code_off.get("thinking").is_none());
        assert!(code_off.get("reasoning_effort").is_none());

        let mut k3 = serde_json::json!({});
        apply_reasoning_effort(
            &mut k3,
            "kimi-k3",
            "https://api.moonshot.cn/v1/chat/completions",
            ReasoningLevel::High,
        );
        assert_eq!(k3["reasoning_effort"], "high");
        assert!(k3.get("thinking").is_none());

        let kimi = crate::anthropic_messages::build_body(
            "kimi-for-coding",
            "https://api.kimi.com/coding/v1/messages",
            &Request::new().with_reasoning(ReasoningLevel::Medium),
        );
        assert_eq!(kimi["thinking"]["type"], "adaptive");
        assert_eq!(kimi["thinking"]["display"], "summarized");
        assert!(kimi["thinking"].get("budget_tokens").is_none());
        assert_eq!(kimi["output_config"]["effort"], "medium");

        let kimi_off = crate::anthropic_messages::build_body(
            "k2p5",
            "https://api.kimi.com/coding/v1/messages",
            &Request::new().with_reasoning(ReasoningLevel::Off),
        );
        assert_eq!(kimi_off["thinking"]["type"], "disabled");
    }

    #[test]
    fn responses_openai_uses_reasoning_object() {
        let body = crate::openai_responses::build_body(
            "gpt-5",
            "https://api.openai.com/v1/responses",
            &Request::new().with_reasoning(ReasoningLevel::Low),
        );
        assert_eq!(body["reasoning"]["effort"], "low");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn usage_parses_cache_read_and_write_across_providers() {
        let anthropic = super::usage_from_value(&serde_json::json!({
            "input_tokens": 100,
            "cache_read_input_tokens": 11800,
            "cache_creation_input_tokens": 20,
            "output_tokens": 420,
        }));
        assert_eq!(anthropic.input_tokens, 100);
        assert_eq!(anthropic.cache_read_tokens, Some(11800));
        assert_eq!(anthropic.cache_write_tokens, Some(20));
        assert_eq!(anthropic.output_tokens, 420);
        assert_eq!(anthropic.prompt_tokens, 11920);

        let nested = super::usage_from_value(&serde_json::json!({
            "input_tokens": 10,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 4,
                "ephemeral_1h_input_tokens": 6,
            },
            "output_tokens": 1,
        }));
        assert_eq!(nested.cache_write_tokens, Some(10));
        assert_eq!(nested.prompt_tokens, 20);

        let both = super::usage_from_value(&serde_json::json!({
            "input_tokens": 10,
            "cache_creation_input_tokens": 20,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 20,
            },
        }));
        assert_eq!(both.cache_write_tokens, Some(20));
        assert_eq!(both.prompt_tokens, 30);

        let openai = super::usage_from_value(&serde_json::json!({
            "prompt_tokens": 12345,
            "completion_tokens": 420,
            "prompt_tokens_details": {
                "cached_tokens": 11800,
                "cache_write_tokens": 15,
            },
        }));
        assert_eq!(openai.input_tokens, 12345);
        assert_eq!(openai.cache_read_tokens, Some(11800));
        assert_eq!(openai.cache_write_tokens, Some(15));
        assert_eq!(openai.prompt_tokens, 12345);

        let deepseek = super::usage_from_value(&serde_json::json!({
            "prompt_tokens": 12345,
            "completion_tokens": 420,
            "prompt_cache_hit_tokens": 11800,
            "prompt_cache_miss_tokens": 545,
        }));
        assert_eq!(deepseek.input_tokens, 12345);
        assert_eq!(deepseek.cache_read_tokens, Some(11800));
        assert_eq!(deepseek.cache_write_tokens, None);
        assert_eq!(deepseek.prompt_tokens, 12345);

        let gemini = super::usage_from_value(&serde_json::json!({
            "promptTokenCount": 1000,
            "candidatesTokenCount": 20,
            "cachedContentTokenCount": 800,
        }));
        assert_eq!(gemini.input_tokens, 1000);
        assert_eq!(gemini.output_tokens, 20);
        assert_eq!(gemini.cache_read_tokens, Some(800));
        assert_eq!(gemini.prompt_tokens, 1000);
    }

    #[test]
    fn merge_usage_keeps_prompt_tokens_when_a_delta_reports_output_only() {
        let previous = super::usage_from_value(&serde_json::json!({
            "input_tokens": 100,
            "cache_read_input_tokens": 50,
            "cache_creation_input_tokens": 5,
            "output_tokens": 1,
        }));
        let next = super::usage_from_value(&serde_json::json!({"output_tokens": 9}));
        let merged = super::merge_usage(Some(previous), next);
        assert_eq!(merged.input_tokens, 100);
        assert_eq!(merged.output_tokens, 9);
        assert_eq!(merged.cache_read_tokens, Some(50));
        assert_eq!(merged.cache_write_tokens, Some(5));
        assert_eq!(merged.prompt_tokens, 155);
    }
}
