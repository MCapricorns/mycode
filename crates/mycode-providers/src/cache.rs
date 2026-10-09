//! Prompt-cache markers.
//!
//! Every `anthropic-messages` body gets at most four
//! `cache_control: {type: "ephemeral"}` breakpoints, including MiniMax, Kimi,
//! DeepSeek, Bedrock, and Vertex when the caller uses that protocol. Chat
//! completions add the same marker only where the host accepts it: OpenRouter
//! Anthropic and Gemini, DashScope Qwen, and Z.AI / Zhipu.
//!
//! `prompt_cache_key` is the session id, clamped to 64 scalars. Hosts that
//! publish the field always get it. Hosts that reject unknown fields never
//! get it. Every other OpenAI-compatible host gets it once; a 400 that names
//! `prompt_cache_key` is retried without the field, and that host is skipped
//! for the rest of the process.
//!
//! Z.AI's implicit cache is per API key and per backend. A compaction summary
//! replaces the message prefix (one expected miss) and the summary request
//! itself is a large unrelated prompt, which drops that key's implicit entry.
//! The next two turns can share a byte-identical prefix and still report
//! `cached_tokens: 0`. `cache_control` breakpoints are the marker those
//! endpoints accept and that pins the new prefix; `prompt_cache_key` is not
//! part of Z.AI's published schema and is not sent.

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::{Value, json};

use mycode_core::{ProviderError, ProviderErrorKind, StreamEvent, Usage};

/// Anthropic allows four `cache_control` breakpoints on one request.
pub(crate) const ANTHROPIC_BREAKPOINT_CAP: usize = 4;

/// OpenAI rejects a `prompt_cache_key` longer than 64 Unicode scalars.
const PROMPT_CACHE_KEY_MAX_CHARS: usize = 64;

const CACHEABLE_BLOCKS: &[&str] = &[
    "text",
    "image",
    "image_url",
    "tool_use",
    "tool_result",
    "document",
];

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

/// Marks the last tool, the last system text block, and up to two trailing
/// message blocks. Existing breakpoints are not counted; callers start from
/// an unmarked body.
pub(crate) fn apply_anthropic_message_breakpoints(body: &mut Value) {
    let mut remaining = ANTHROPIC_BREAKPOINT_CAP;
    if mark_last_object(body.get_mut("tools")) {
        remaining = remaining.saturating_sub(1);
    }
    if mark_last_system_text(body.get_mut("system")) {
        remaining = remaining.saturating_sub(1);
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // MiniMax-M3 (and the rest of the MiniMax Messages models) keep a
    // passive prefix cache and require prior thinking blocks to be replayed
    // unchanged. A sliding breakpoint rewrites the message that holds the
    // thinking block, so the next turn misses. Pin the head and the tail
    // instead, and do not write cache_control into a thinking message.
    if crate::family::anthropic_pins_stable_ends(model) {
        mark_stable_ends(body.get_mut("messages"), remaining, &[], true);
    } else {
        mark_trailing_messages(body.get_mut("messages"), remaining.min(2), &[]);
    }
}

/// Chat-completions shape of the same four-breakpoint budget: last tool,
/// first system or developer message, then the first and last later messages.
///
/// The head message stays marked on every later turn, so a compaction
/// summary does not fall out of the cached prefix. Messages that carry
/// replayed reasoning are left untouched: GLM preserved thinking (glm-5.3
/// included) requires `reasoning_content` and the assistant content string
/// to stay byte-identical, and rewriting one of those messages on the next
/// turn changes the prefix.
pub(crate) fn apply_chat_cache_breakpoints(body: &mut Value) {
    let mut remaining = ANTHROPIC_BREAKPOINT_CAP;
    if mark_last_object(body.get_mut("tools")) {
        remaining = remaining.saturating_sub(1);
    }
    if mark_first_system_message(body.get_mut("messages")) {
        remaining = remaining.saturating_sub(1);
    }
    mark_stable_ends(
        body.get_mut("messages"),
        remaining,
        &["system", "developer"],
        true,
    );
}

/// OpenRouter Anthropic/Gemini, DashScope Qwen, and Z.AI / Zhipu GLM accept
/// explicit `cache_control`. DeepSeek, OpenRouter GLM, and DashScope GLM do not.
#[must_use]
pub(crate) fn explicit_chat_cache(model: &str, endpoint: &str) -> bool {
    // api.z.ai and open.bigmodel.cn (including coding-plan paths) accept
    // Anthropic-style breakpoints on the OpenAI-compatible body. That is what
    // keeps a post-compaction prefix cached when implicit cache was evicted.
    crate::family::explicit_chat_cache(model, endpoint)
}

/// Whether this endpoint should carry `prompt_cache_key` on the next request.
///
/// Known rejectors stay off. A host that already returned 400 naming the
/// field stays off for the rest of the process. Every other host is on.
#[must_use]
pub(crate) fn wants_prompt_cache_key(endpoint: &str) -> bool {
    if crate::family::omits_prompt_cache_key(endpoint) {
        return false;
    }
    !prompt_cache_key_rejected(&endpoint_host(endpoint))
}

/// Writes `prompt_cache_key` when this endpoint uses one and the session key
/// is non-empty. The key is clamped to 64 Unicode scalars.
pub(crate) fn apply_prompt_cache_key(body: &mut Value, endpoint: &str, key: Option<&str>) {
    if !wants_prompt_cache_key(endpoint) {
        return;
    }
    let Some(key) = key.map(str::trim).filter(|key| !key.is_empty()) else {
        return;
    };
    body["prompt_cache_key"] = json!(clamp_prompt_cache_key(key));
}

/// Hosts that answered 400 naming `prompt_cache_key` during this process.
fn rejected_prompt_cache_keys() -> &'static Mutex<HashSet<String>> {
    static REJECTED: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    REJECTED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn prompt_cache_key_rejected(host: &str) -> bool {
    rejected_prompt_cache_keys()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(host)
}

fn remember_prompt_cache_key_rejected(host: &str) {
    rejected_prompt_cache_keys()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(host.to_owned());
}

/// Hostname used as the per-provider memory key, without the path.
fn endpoint_host(endpoint: &str) -> String {
    let lower = endpoint.to_ascii_lowercase();
    let after_scheme = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest);
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme)
        .to_owned()
}

/// True when a rejected response names `prompt_cache_key` and the body still
/// carries it. Removes the field, remembers the host, and returns the retry
/// bytes. Any other 400 is left alone.
pub(crate) fn retry_body_without_prompt_cache_key(
    endpoint: &str,
    error: &ProviderError,
    body: &[u8],
) -> Option<Vec<u8>> {
    if error.kind() != ProviderErrorKind::Rejected {
        return None;
    }
    let names_field = error
        .message()
        .is_some_and(|message| message.to_ascii_lowercase().contains("prompt_cache_key"));
    if !names_field {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object_mut()?;
    object.remove("prompt_cache_key")?;
    remember_prompt_cache_key_rejected(&endpoint_host(endpoint));
    serde_json::to_vec(&value).ok()
}

/// Whether the serialized body includes a cache key, so the driver can keep
/// a retry copy only for those requests.
#[must_use]
pub(crate) fn body_has_prompt_cache_key(body: &[u8]) -> bool {
    body.windows(b"prompt_cache_key".len())
        .any(|window| window == b"prompt_cache_key")
}

/// Clamps a cache key to [`PROMPT_CACHE_KEY_MAX_CHARS`] Unicode scalars.
#[must_use]
pub(crate) fn clamp_prompt_cache_key(key: &str) -> String {
    key.chars().take(PROMPT_CACHE_KEY_MAX_CHARS).collect()
}

/// OpenRouter sticky-routing header. The session id is not clamped; the
/// header is omitted when the endpoint is not OpenRouter or the key is empty.
#[must_use]
pub(crate) fn openrouter_session_header(
    endpoint: &str,
    key: Option<&str>,
) -> Option<(String, String)> {
    if !endpoint.to_ascii_lowercase().contains("openrouter.ai") {
        return None;
    }
    let key = key.map(str::trim).filter(|key| !key.is_empty())?;
    Some(("x-session-id".to_owned(), key.to_owned()))
}

/// One stderr line a QA run can grep after a model response.
#[must_use]
pub(crate) fn usage_log_line(provider: &str, model: &str, usage: &Usage) -> String {
    format!(
        "[usage] provider={provider} model={model} input={} cache_read={} cache_write={} output={}",
        usage.input_tokens,
        usage.cache_read_tokens.unwrap_or(0),
        usage.cache_write_tokens.unwrap_or(0),
        usage.output_tokens,
    )
}

/// Prints [`usage_log_line`] for a terminal assistant message.
pub(crate) fn log_done_usage(provider: &str, model: &str, event: &StreamEvent) {
    let StreamEvent::Done { message } = event else {
        return;
    };
    let usage = message.usage.unwrap_or_default();
    eprintln!("{}", usage_log_line(provider, model, &usage));
}

fn mark_last_object(items: Option<&mut Value>) -> bool {
    let Some(Value::Array(items)) = items else {
        return false;
    };
    let Some(object) = items.last_mut().and_then(Value::as_object_mut) else {
        return false;
    };
    object.insert("cache_control".to_owned(), ephemeral());
    true
}

fn mark_last_system_text(system: Option<&mut Value>) -> bool {
    let Some(Value::Array(blocks)) = system else {
        return false;
    };
    for block in blocks.iter_mut().rev() {
        if block.get("type").and_then(Value::as_str) == Some("text")
            && let Some(object) = block.as_object_mut()
        {
            object.insert("cache_control".to_owned(), ephemeral());
            return true;
        }
    }
    false
}

fn mark_first_system_message(messages: Option<&mut Value>) -> bool {
    let Some(Value::Array(messages)) = messages else {
        return false;
    };
    for message in messages.iter_mut() {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "system" || role == "developer" {
            return mark_content(message.get_mut("content"));
        }
    }
    false
}

/// Marks the first and last eligible message and leaves replayed reasoning
/// messages byte-for-byte alone.
///
/// `budget` is the number of message breakpoints still available. The head
/// is preferred when only one slot remains, because that message (often the
/// compaction summary) is the stable prefix. The tail is marked as well
/// when a second slot remains and it is a different message.
fn mark_stable_ends(
    messages: Option<&mut Value>,
    budget: usize,
    skip_roles: &[&str],
    preserve_thinking: bool,
) {
    let Some(Value::Array(messages)) = messages else {
        return;
    };
    if budget == 0 {
        return;
    }
    let eligible: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            if skip_roles.contains(&role) {
                return false;
            }
            if preserve_thinking && carries_replayed_thinking(message) {
                return false;
            }
            content_is_markable(message.get("content"))
        })
        .map(|(index, _)| index)
        .collect();
    let Some(&head) = eligible.first() else {
        return;
    };
    let mut chosen = vec![head];
    if budget >= 2
        && let Some(&tail) = eligible.last()
        && tail != head
    {
        chosen.push(tail);
    }
    for index in chosen.into_iter().rev() {
        mark_content(messages[index].get_mut("content"));
    }
}

/// True when this message replays provider reasoning that the next request
/// must send unchanged.
fn carries_replayed_thinking(message: &Value) -> bool {
    if message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
    {
        return true;
    }
    if message
        .get("reasoning_details")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return true;
    }
    let Some(Value::Array(blocks)) = message.get("content") else {
        return false;
    };
    blocks.iter().any(|block| {
        matches!(
            block.get("type").and_then(Value::as_str),
            Some("thinking" | "redacted_thinking")
        )
    })
}

fn content_is_markable(content: Option<&Value>) -> bool {
    match content {
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(blocks)) => blocks.iter().any(|block| {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            CACHEABLE_BLOCKS.contains(&kind)
        }),
        _ => false,
    }
}

fn mark_trailing_messages(messages: Option<&mut Value>, budget: usize, skip_roles: &[&str]) {
    let Some(Value::Array(messages)) = messages else {
        return;
    };
    let mut marked = 0;
    for message in messages.iter_mut().rev() {
        if marked >= budget {
            break;
        }
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if skip_roles.contains(&role) {
            continue;
        }
        if mark_content(message.get_mut("content")) {
            marked += 1;
        }
    }
}

fn mark_content(content: Option<&mut Value>) -> bool {
    let Some(content) = content else {
        return false;
    };
    match content {
        Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            let text = std::mem::take(text);
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"},
            }]);
            true
        }
        Value::Array(blocks) => {
            for block in blocks.iter_mut().rev() {
                let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                if CACHEABLE_BLOCKS.contains(&kind)
                    && let Some(object) = block.as_object_mut()
                {
                    object.insert("cache_control".to_owned(), ephemeral());
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use mycode_core::{
        AssistantMessage, ContentBlock, Message, ReasoningLevel, Request, StopReason, TextBlock,
        ThinkingBlock, ToolSpec, Usage, UserMessage,
    };
    use serde_json::{Value, json};

    use super::{
        ANTHROPIC_BREAKPOINT_CAP, apply_chat_cache_breakpoints, apply_prompt_cache_key,
        clamp_prompt_cache_key, explicit_chat_cache, openrouter_session_header,
        retry_body_without_prompt_cache_key, usage_log_line, wants_prompt_cache_key,
    };
    use crate::anthropic_messages::build_body as anthropic_body;
    use crate::openai_completions::build_body as chat_body;
    use crate::openai_responses::build_body as responses_body;
    use crate::wire_common::usage_from_value;
    use mycode_core::{ProviderError, ProviderErrorKind};

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_owned(),
            description: name.to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn user(text: &str) -> Message {
        Message::User(UserMessage::text(text))
    }

    fn count_cache_control(value: &Value) -> usize {
        match value {
            Value::Object(map) => {
                let here = usize::from(map.contains_key("cache_control"));
                here + map.values().map(count_cache_control).sum::<usize>()
            }
            Value::Array(items) => items.iter().map(count_cache_control).sum(),
            _ => 0,
        }
    }

    fn cache_type(value: &Value) -> Option<&str> {
        value.get("cache_control")?.get("type")?.as_str()
    }

    #[test]
    fn anthropic_breakpoints_are_last_tool_last_system_and_last_two_messages() {
        let request = Request::new()
            .with_system_prompt("rules")
            .with_system_prompt("tools stay cached")
            .with_tool(tool("read"))
            .with_tool(tool("grep"))
            .with_message(user("one"))
            .with_message(user("two"))
            .with_message(user("three"))
            .with_message(Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(ThinkingBlock::new("hidden").with_signature("sig-1")),
                    ContentBlock::Text(TextBlock::new("visible")),
                ],
                usage: None,
                stop_reason: StopReason::Stop,
            }));
        let body = anthropic_body(
            "claude-sonnet-4-6",
            "https://api.anthropic.com/v1/messages",
            &request,
        );
        assert!(body["system"].is_array());
        assert!(body["system"][0].get("cache_control").is_none());
        assert_eq!(cache_type(&body["system"][1]), Some("ephemeral"));
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(cache_type(&body["tools"][1]), Some("ephemeral"));
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert!(
            body["messages"][1]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            cache_type(&body["messages"][2]["content"][0]),
            Some("ephemeral")
        );
        let last = &body["messages"][3]["content"];
        assert!(last[0].get("cache_control").is_none());
        assert_eq!(last[0]["type"], "thinking");
        assert_eq!(cache_type(&last[1]), Some("ephemeral"));
        assert_eq!(count_cache_control(&body), ANTHROPIC_BREAKPOINT_CAP);
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn minimax_anthropic_endpoint_gets_the_same_breakpoints() {
        let request = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_message(user("hi"));
        let body = anthropic_body(
            "MiniMax-M2",
            "https://api.minimax.io/anthropic/v1/messages",
            &request,
        );
        assert_eq!(cache_type(&body["system"][0]), Some("ephemeral"));
        assert_eq!(cache_type(&body["tools"][0]), Some("ephemeral"));
        assert_eq!(
            cache_type(&body["messages"][0]["content"][0]),
            Some("ephemeral")
        );
        assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);
    }

    #[test]
    fn openrouter_anthropic_and_gemini_get_chat_breakpoints() {
        let mut request = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_tool(tool("grep"))
            .with_message(user("one"))
            .with_message(user("two"))
            .with_message(user("three"));
        request.prompt_cache_key = Some("session-9".to_owned());
        for model in ["anthropic/claude-sonnet-4.6", "google/gemini-2.5-pro"] {
            let body = chat_body(
                model,
                "https://openrouter.ai/api/v1/chat/completions",
                &request,
            );
            assert_eq!(cache_type(&body["tools"][1]), Some("ephemeral"));
            assert!(body["tools"][0].get("cache_control").is_none());
            assert_eq!(
                cache_type(&body["messages"][0]["content"][0]),
                Some("ephemeral")
            );
            assert_eq!(body["messages"][0]["content"][0]["text"], "stable");
            assert_eq!(
                cache_type(&body["messages"][1]["content"][0]),
                Some("ephemeral")
            );
            assert_eq!(body["messages"][1]["content"][0]["text"], "one");
            assert!(
                body["messages"][2]["content"].as_str().is_some(),
                "a middle message stays a string so the next turn does not rewrite it"
            );
            assert!(
                body["messages"][2]["content"]
                    .get("cache_control")
                    .is_none()
            );
            assert_eq!(
                cache_type(&body["messages"][3]["content"][0]),
                Some("ephemeral")
            );
            assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);
            assert!(body.get("prompt_cache_key").is_none());
            assert!(explicit_chat_cache(
                model,
                "https://openrouter.ai/api/v1/chat/completions"
            ));
        }
        assert_eq!(
            openrouter_session_header(
                "https://openrouter.ai/api/v1/chat/completions",
                Some("session-9")
            ),
            Some(("x-session-id".to_owned(), "session-9".to_owned()))
        );
    }

    #[test]
    fn dashscope_qwen_is_explicit_and_glm_on_the_same_host_is_not() {
        let request = Request::new()
            .with_system_prompt("stable")
            .with_message(user("hi"));
        let endpoint = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";
        let qwen = chat_body("qwen-plus", endpoint, &request);
        assert_eq!(
            cache_type(&qwen["messages"][0]["content"][0]),
            Some("ephemeral")
        );
        assert_eq!(
            cache_type(&qwen["messages"][1]["content"][0]),
            Some("ephemeral")
        );
        let glm = chat_body("glm-4.7", endpoint, &request);
        assert_eq!(count_cache_control(&glm), 0);
        assert!(!explicit_chat_cache("glm-4.7", endpoint));
        assert!(explicit_chat_cache("qwq-plus", endpoint));
    }

    #[test]
    fn zai_glm_breakpoints_cover_a_compaction_summary_prefix() {
        let request = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_message(user("COMPACTION SUMMARY\n\ngoals and files"))
            .with_message(user("continue from the summary"))
            .with_message(user("and the next step"));
        for endpoint in [
            "https://api.z.ai/api/coding/paas/v4/chat/completions",
            "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        ] {
            let body = chat_body("glm-5.3", endpoint, &request);
            assert_eq!(
                cache_type(&body["messages"][0]["content"][0]),
                Some("ephemeral"),
                "{endpoint}"
            );
            assert_eq!(cache_type(&body["tools"][0]), Some("ephemeral"));
            assert_eq!(
                cache_type(&body["messages"][1]["content"][0]),
                Some("ephemeral"),
                "the compaction summary stays the sticky head breakpoint"
            );
            assert!(
                body["messages"][2]["content"].as_str().is_some(),
                "the middle turn stays a string"
            );
            assert_eq!(
                cache_type(&body["messages"][3]["content"][0]),
                Some("ephemeral")
            );
            assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);
            assert!(body.get("prompt_cache_key").is_none());
            assert!(explicit_chat_cache("glm-5.3", endpoint));
        }
    }

    /// The compaction summary call sets `reasoning = Off`. Breakpoints must
    /// not turn thinking back on, and the stored summary stays the head
    /// breakpoint on the resumed turn.
    #[test]
    fn summary_off_keeps_disabled_thinking_and_the_summary_breakpoint() {
        let summary = Request::new()
            .with_system_prompt("You are performing a CONTEXT CHECKPOINT COMPACTION.")
            .with_reasoning(ReasoningLevel::Off)
            .with_message(user(
                "Summarize the following conversation for continuation:\n\ntranscript",
            ));
        let endpoint = "https://api.z.ai/api/paas/v4/chat/completions";
        let body = chat_body("glm-5.3", endpoint, &summary);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body["thinking"].get("clear_thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("prompt_cache_key").is_none());
        assert_eq!(
            cache_type(&body["messages"][0]["content"][0]),
            Some("ephemeral")
        );
        assert_eq!(
            cache_type(&body["messages"][1]["content"][0]),
            Some("ephemeral")
        );
        assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);

        let minimax = anthropic_body(
            "MiniMax-M3",
            "https://api.minimax.io/anthropic/v1/messages",
            &summary,
        );
        assert_eq!(minimax["thinking"]["type"], "disabled");
        assert!(minimax.get("output_config").is_none());
        assert!(minimax["thinking"].get("budget_tokens").is_none());
        assert_eq!(
            cache_type(&minimax["system"][0]),
            Some("ephemeral"),
            "system breakpoint survives an Off summary"
        );
        assert!(count_cache_control(&minimax) <= ANTHROPIC_BREAKPOINT_CAP);

        let resumed = Request::new()
            .with_system_prompt("stable rules")
            .with_reasoning(ReasoningLevel::Max)
            .with_message(user("COMPACTION SUMMARY\n\ngoals and files"))
            .with_message(user("continue"));
        let later = resumed.clone().with_message(user("next"));
        let first = chat_body("glm-5.3", endpoint, &resumed);
        let second = chat_body("glm-5.3", endpoint, &later);
        assert_eq!(first["thinking"]["type"], "enabled");
        assert_eq!(first["thinking"]["clear_thinking"], false);
        assert_eq!(first["reasoning_effort"], "max");
        assert_eq!(
            cache_type(&first["messages"][1]["content"][0]),
            Some("ephemeral"),
            "the summary stays the sticky head breakpoint"
        );
        assert_eq!(second["messages"][1], first["messages"][1]);
        assert!(
            second["messages"][2]["content"].as_str().is_some(),
            "the middle user turn is not rewritten when the tail moves"
        );
    }

    fn assistant(thinking: &str, text: &str, signature: Option<&str>) -> Message {
        let mut block = ThinkingBlock::new(thinking);
        if let Some(signature) = signature {
            block = block.with_signature(signature);
        }
        Message::Assistant(AssistantMessage {
            blocks: vec![
                ContentBlock::Thinking(block),
                ContentBlock::Text(TextBlock::new(text)),
            ],
            usage: None,
            stop_reason: StopReason::Stop,
        })
    }

    #[test]
    fn reasoning_details_are_not_rewritten_by_chat_breakpoints() {
        let mut body = json!({
            "model": "MiniMax-M3",
            "tools": [{"type": "function", "function": {"name": "read"}}],
            "messages": [
                {"role": "system", "content": "stable"},
                {"role": "user", "content": "COMPACTION SUMMARY\n\nkept"},
                {
                    "role": "assistant",
                    "content": "visible answer",
                    "reasoning_details": [{
                        "type": "reasoning.text",
                        "id": "reasoning-text-1",
                        "text": "plan the edit exactly"
                    }]
                },
                {"role": "user", "content": "continue"}
            ]
        });
        let assistant = body["messages"][2].clone();
        apply_chat_cache_breakpoints(&mut body);
        assert_eq!(body["messages"][2], assistant);
        assert!(body["messages"][2]["content"].as_str().is_some());
    }

    #[test]
    fn glm53_max_on_the_anthropic_route_sends_effort_and_keeps_breakpoints() {
        let request = Request::new()
            .with_system_prompt("stable rules")
            .with_reasoning(ReasoningLevel::Max)
            .with_message(user("go"));
        for endpoint in [
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            "https://api.z.ai/api/anthropic/v1/messages",
        ] {
            let body = anthropic_body("glm-5.3", endpoint, &request);
            assert_eq!(body["thinking"]["type"], "enabled", "{endpoint}");
            assert_eq!(body["thinking"]["clear_thinking"], false, "{endpoint}");
            assert_eq!(body["output_config"]["effort"], "max", "{endpoint}");
            assert!(body.get("reasoning_effort").is_none(), "{endpoint}");
            assert!(
                body["thinking"].get("budget_tokens").is_none(),
                "{endpoint}"
            );
            assert_eq!(
                cache_type(&body["system"][0]),
                Some("ephemeral"),
                "{endpoint}"
            );
            assert_eq!(
                cache_type(&body["messages"][0]["content"][0]),
                Some("ephemeral"),
                "{endpoint}"
            );
            assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);
        }
    }

    #[test]
    fn glm53_max_thinking_keeps_reasoning_content_byte_stable() {
        let history = Request::new()
            .with_system_prompt("stable rules")
            .with_reasoning(ReasoningLevel::Max)
            .with_tool(tool("read"))
            .with_message(user("COMPACTION SUMMARY\n\nkept goals"))
            .with_message(assistant("plan the edit exactly", "visible answer", None))
            .with_message(user("continue"));
        let follow_up = history.clone().with_message(user("and the next step"));
        for endpoint in [
            "https://api.z.ai/api/coding/paas/v4/chat/completions",
            "https://api.z.ai/api/paas/v4/chat/completions",
            "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
        ] {
            let first = chat_body("glm-5.3", endpoint, &history);
            let second = chat_body("glm-5.3", endpoint, &follow_up);
            assert_eq!(first["thinking"]["type"], "enabled", "{endpoint}");
            assert_eq!(first["thinking"]["clear_thinking"], false, "{endpoint}");
            assert_eq!(first["reasoning_effort"], "max", "{endpoint}");
            assert!(first.get("prompt_cache_key").is_none(), "{endpoint}");
            assert_eq!(cache_type(&first["tools"][0]), Some("ephemeral"));
            let replayed = &first["messages"][2];
            assert_eq!(replayed["reasoning_content"], "plan the edit exactly");
            assert_eq!(replayed["content"], "visible answer");
            assert!(replayed.get("cache_control").is_none());
            assert!(replayed["content"].get("cache_control").is_none());
            assert_eq!(
                &second["messages"][2], replayed,
                "the next turn must resend the same reasoning_content"
            );
            assert_eq!(second["messages"][1], first["messages"][1]);
            assert_eq!(second["thinking"], first["thinking"]);
            assert_eq!(second["reasoning_effort"], first["reasoning_effort"]);
            assert!(count_cache_control(&first) <= ANTHROPIC_BREAKPOINT_CAP);
            assert_eq!(chat_body("glm-5.3", endpoint, &history), first);
        }
    }

    #[test]
    fn minimax_m3_max_thinking_keeps_thinking_blocks_byte_stable() {
        let history = Request::new()
            .with_system_prompt("stable rules")
            .with_reasoning(ReasoningLevel::Max)
            .with_tool(tool("read"))
            .with_message(user("COMPACTION SUMMARY\n\nkept goals"))
            .with_message(assistant(
                "plan the edit exactly",
                "visible answer",
                Some("sig-stable"),
            ))
            .with_message(user("continue"));
        let follow_up = history.clone().with_message(user("and the next step"));
        for endpoint in [
            "https://api.minimax.io/anthropic/v1/messages",
            "https://api.minimax.cn/anthropic/v1/messages",
        ] {
            let first = anthropic_body("MiniMax-M3", endpoint, &history);
            let second = anthropic_body("MiniMax-M3", endpoint, &follow_up);
            assert_eq!(first["thinking"]["type"], "adaptive", "{endpoint}");
            assert!(first["thinking"].get("budget_tokens").is_none());
            assert!(first.get("output_config").is_none());
            assert!(first.get("prompt_cache_key").is_none());
            let replayed = &first["messages"][1]["content"];
            assert_eq!(replayed[0]["type"], "thinking");
            assert_eq!(replayed[0]["thinking"], "plan the edit exactly");
            assert_eq!(replayed[0]["signature"], "sig-stable");
            assert!(replayed[0].get("cache_control").is_none());
            assert!(replayed[1].get("cache_control").is_none());
            assert_eq!(replayed[1]["text"], "visible answer");
            assert_eq!(&second["messages"][1], &first["messages"][1]);
            assert_eq!(second["system"], first["system"]);
            assert_eq!(second["thinking"], first["thinking"]);
            assert!(count_cache_control(&first) <= ANTHROPIC_BREAKPOINT_CAP);
            assert_eq!(anthropic_body("MiniMax-M3", endpoint, &history), first);
        }
    }

    #[test]
    fn implicit_hosts_get_no_cache_control() {
        let request = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_message(user("hi"));
        let cases = [
            ("deepseek-chat", "https://api.deepseek.com/chat/completions"),
            ("MiniMax-M2", "https://api.minimaxi.com/v1/chat/completions"),
            (
                "z-ai/glm-4.7",
                "https://openrouter.ai/api/v1/chat/completions",
            ),
            (
                "deepseek/deepseek-chat",
                "https://openrouter.ai/api/v1/chat/completions",
            ),
        ];
        for (model, endpoint) in cases {
            let body = chat_body(model, endpoint, &request);
            assert_eq!(count_cache_control(&body), 0, "{model} {endpoint}");
            assert!(body.get("prompt_cache_key").is_none(), "{model}");
        }
    }

    #[test]
    fn prompt_cache_key_is_clamped_on_openai_family_hosts() {
        let mut request = Request::new().with_message(user("hi"));
        request.prompt_cache_key = Some("s".repeat(80));
        let hosts = [
            "https://api.openai.com/v1/chat/completions",
            "https://example.openai.azure.com/openai/v1/chat/completions",
            "https://example.cognitiveservices.azure.com/openai/v1/chat/completions",
            "https://api.x.ai/v1/chat/completions",
            "https://api.mistral.ai/v1/chat/completions",
            "https://api.cerebras.ai/v1/chat/completions",
            "https://api.deepinfra.com/v1/openai/chat/completions",
            "https://api.venice.ai/api/v1/chat/completions",
        ];
        for endpoint in hosts {
            assert!(wants_prompt_cache_key(endpoint), "{endpoint}");
            let body = chat_body("gpt-5", endpoint, &request);
            assert_eq!(
                body["prompt_cache_key"].as_str().unwrap().chars().count(),
                64,
                "{endpoint}"
            );
            assert_eq!(count_cache_control(&body), 0);
        }
        let responses = responses_body("gpt-5", "https://api.openai.com/v1/responses", &request);
        assert_eq!(
            responses["prompt_cache_key"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            64
        );
        let plain = chat_body(
            "gpt-5",
            "https://api.openai.com/v1/chat/completions",
            &Request::new(),
        );
        assert!(plain.get("prompt_cache_key").is_none());
        assert_eq!(clamp_prompt_cache_key("short"), "short");
    }

    #[test]
    fn prompt_cache_key_stays_off_openrouter_and_glm() {
        let mut request = Request::new();
        request.prompt_cache_key = Some("session".to_owned());
        let mut body = json!({});
        apply_prompt_cache_key(
            &mut body,
            "https://openrouter.ai/api/v1/chat/completions",
            Some("session"),
        );
        assert!(body.get("prompt_cache_key").is_none());
        apply_prompt_cache_key(
            &mut body,
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            Some("session"),
        );
        assert!(body.get("prompt_cache_key").is_none());
        assert!(openrouter_session_header("https://api.openai.com/v1", Some("session")).is_none());
        assert!(openrouter_session_header("https://openrouter.ai/api/v1", Some("  ")).is_none());
    }

    #[test]
    fn each_provider_family_gets_its_cache_fields() {
        let mut request = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_message(user("hello"));
        request.prompt_cache_key = Some("session-1".to_owned());

        // Anthropic-protocol presets and the Anthropic-compatible URLs a
        // custom provider can point at. Bedrock and Vertex are not catalog
        // presets; the Messages adapter still marks them.
        let anthropic_endpoints = [
            ("claude-sonnet-4-6", "https://api.anthropic.com/v1/messages"),
            ("claude-sonnet-4-6", "https://cc.freemodel.dev/v1/messages"),
            (
                "claude-sonnet-4-6",
                "https://api.subconscious.dev/v1/messages",
            ),
            (
                "claude-sonnet-4-6",
                "https://tinker.thinkingmachines.dev/services/tinker-prod/anthropic/api/v1/messages",
            ),
            ("MiniMax-M3", "https://api.minimax.io/anthropic/v1/messages"),
            ("kimi-k2.6", "https://api.moonshot.cn/anthropic/v1/messages"),
            (
                "deepseek-v4-pro",
                "https://api.deepseek.com/anthropic/v1/messages",
            ),
            (
                "claude-sonnet-4-6",
                "https://bedrock-runtime.us-east-1.amazonaws.com/anthropic/v1/messages",
            ),
            (
                "claude-sonnet-4-6",
                "https://us-east5-aiplatform.googleapis.com/v1/projects/p/locations/us-east5/publishers/anthropic/models/claude:streamRawPredict",
            ),
        ];
        for (model, endpoint) in anthropic_endpoints {
            let body = anthropic_body(model, endpoint, &request);
            assert!(
                count_cache_control(&body) > 0,
                "{model} {endpoint} should carry cache_control"
            );
            assert!(body.get("prompt_cache_key").is_none(), "{endpoint}");
            assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);
        }

        // Chat families: (model, endpoint, cache_control, prompt_cache_key).
        let chat = [
            (
                "gpt-5",
                "https://api.openai.com/v1/chat/completions",
                false,
                true,
            ),
            (
                "gpt-5",
                "https://example.openai.azure.com/openai/v1/chat/completions",
                false,
                true,
            ),
            (
                "grok-4",
                "https://api.x.ai/v1/chat/completions",
                false,
                true,
            ),
            (
                "mistral-large",
                "https://api.mistral.ai/v1/chat/completions",
                false,
                true,
            ),
            (
                "llama-3.3-70b",
                "https://api.cerebras.ai/v1/chat/completions",
                false,
                true,
            ),
            (
                "meta-llama/Llama-3.3-70B",
                "https://api.deepinfra.com/v1/openai/chat/completions",
                false,
                true,
            ),
            (
                "llama-3.3-70b",
                "https://api.venice.ai/api/v1/chat/completions",
                false,
                true,
            ),
            (
                "anthropic/claude-sonnet-4.6",
                "https://openrouter.ai/api/v1/chat/completions",
                true,
                false,
            ),
            (
                "google/gemini-2.5-pro",
                "https://openrouter.ai/api/v1/chat/completions",
                true,
                false,
            ),
            (
                "z-ai/glm-5.3",
                "https://openrouter.ai/api/v1/chat/completions",
                false,
                false,
            ),
            (
                "deepseek/deepseek-v4-pro",
                "https://openrouter.ai/api/v1/chat/completions",
                false,
                false,
            ),
            (
                "qwen3.7-max",
                "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
                true,
                false,
            ),
            (
                "qwen3-coder-plus",
                "https://coding.dashscope.aliyuncs.com/v1/chat/completions",
                true,
                false,
            ),
            (
                "qwen-plus",
                "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1/chat/completions",
                true,
                false,
            ),
            (
                "glm-4.7",
                "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
                false,
                false,
            ),
            (
                "glm-5.3",
                "https://api.z.ai/api/coding/paas/v4/chat/completions",
                true,
                false,
            ),
            (
                "glm-5.3",
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                true,
                false,
            ),
            (
                "deepseek-v4-pro",
                "https://api.deepseek.com/chat/completions",
                false,
                false,
            ),
            (
                "kimi-k2.6",
                "https://api.moonshot.cn/v1/chat/completions",
                false,
                false,
            ),
            (
                "kimi-k2.6",
                "https://api.moonshot.ai/v1/chat/completions",
                false,
                false,
            ),
            (
                "kimi-for-coding",
                "https://api.kimi.com/coding/v1/chat/completions",
                false,
                false,
            ),
            (
                "llama-3.3-70b-versatile",
                "https://api.groq.com/openai/v1/chat/completions",
                false,
                false,
            ),
            (
                "meta-llama/Llama-3.3-70B",
                "https://api.together.xyz/v1/chat/completions",
                false,
                false,
            ),
            (
                "accounts/fireworks/routers/kimi-latest",
                "https://api.fireworks.ai/inference/v1/chat/completions",
                false,
                true,
            ),
            (
                "deepseek-ai/DeepSeek-V4-Pro",
                "https://api.siliconflow.cn/v1/chat/completions",
                false,
                true,
            ),
            (
                "doubao-seed-2-0-pro",
                "https://ark.cn-beijing.volces.com/api/v3/chat/completions",
                false,
                false,
            ),
            (
                "ernie-4.5",
                "https://qianfan.baidubce.com/v2/chat/completions",
                false,
                false,
            ),
            (
                "gemini-2.5-pro",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
                false,
                false,
            ),
            (
                "qwen3:8b",
                "https://ollama.com/v1/chat/completions",
                false,
                true,
            ),
            (
                "qwen3:8b",
                "http://127.0.0.1:11434/v1/chat/completions",
                false,
                true,
            ),
            (
                "gpt-4o",
                "https://api.githubcopilot.com/chat/completions",
                false,
                true,
            ),
            (
                "sonar",
                "https://api.perplexity.ai/v1/chat/completions",
                false,
                true,
            ),
        ];
        for (model, endpoint, cache, key) in chat {
            let body = chat_body(model, endpoint, &request);
            assert_eq!(
                count_cache_control(&body) > 0,
                cache,
                "{model} {endpoint} cache_control"
            );
            assert_eq!(
                body.get("prompt_cache_key").and_then(Value::as_str),
                key.then_some("session-1"),
                "{model} {endpoint} prompt_cache_key"
            );
            assert!(count_cache_control(&body) <= ANTHROPIC_BREAKPOINT_CAP);
        }

        let responses = responses_body(
            "gpt-5",
            "https://chatgpt.com/backend-api/codex/responses",
            &request,
        );
        assert!(responses.get("cache_control").is_none());
        assert_eq!(responses["prompt_cache_key"], "session-1");
    }

    #[test]
    fn every_catalog_preset_body_matches_its_family() {
        let document = crate::catalog::bundled();
        assert!(
            document.providers.len() >= 193,
            "vendored catalog shrank: {}",
            document.providers.len()
        );
        let mut request = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_message(user("hello"));
        request.prompt_cache_key = Some("session-1".to_owned());

        let mut anthropic = 0usize;
        let mut completions = 0usize;
        let mut responses = 0usize;
        for provider in &document.providers {
            let endpoint = provider.base_url.as_str();
            let kind = provider.kind.as_str();
            match kind {
                "anthropic-messages" => anthropic += 1,
                "openai-completions" => completions += 1,
                "openai-responses" => responses += 1,
                other => panic!("{} has unknown kind {other}", provider.id),
            }
            assert!(!provider.models.is_empty(), "{} has no models", provider.id);
            for model in &provider.models {
                let body = match kind {
                    "anthropic-messages" => anthropic_body(&model.id, endpoint, &request),
                    "openai-responses" => responses_body(&model.id, endpoint, &request),
                    _ => chat_body(&model.id, endpoint, &request),
                };
                let marked = count_cache_control(&body);
                assert!(
                    marked <= ANTHROPIC_BREAKPOINT_CAP,
                    "{} {} has {marked} breakpoints",
                    provider.id,
                    model.id
                );
                let expect_cache = kind == "anthropic-messages"
                    || (kind == "openai-completions" && explicit_chat_cache(&model.id, endpoint));
                assert_eq!(
                    marked > 0,
                    expect_cache,
                    "{} {} {endpoint} cache_control",
                    provider.id,
                    model.id
                );
                let expect_key = (kind != "anthropic-messages" && wants_prompt_cache_key(endpoint))
                    .then_some("session-1");
                assert_eq!(
                    body.get("prompt_cache_key").and_then(Value::as_str),
                    expect_key,
                    "{} {} {endpoint} prompt_cache_key",
                    provider.id,
                    model.id
                );
            }
            let header = openrouter_session_header(endpoint, Some("session-1"));
            if endpoint.to_ascii_lowercase().contains("openrouter.ai") {
                assert_eq!(
                    header
                        .as_ref()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                    Some(("x-session-id", "session-1")),
                    "{}",
                    provider.id
                );
            } else {
                assert!(header.is_none(), "{}", provider.id);
            }
        }
        assert_eq!(anthropic, 8);
        assert_eq!(completions, 185);
        assert_eq!(responses, 1);
        assert!(document.provider("openai-codex").is_some());

        // Pinned families, independent of the predicate the loop calls.
        let pinned = [
            ("anthropic", "claude", true, false),
            ("minimax", "MiniMax-M3", true, false),
            ("minimax-cn", "MiniMax-M3", true, false),
            ("freemodel", "", true, false),
            ("subconscious", "", true, false),
            ("thinkingmachines", "", true, false),
            ("openrouter", "anthropic/claude", true, false),
            ("openrouter", "google/gemini", true, false),
            ("openrouter", "qwen/", false, false),
            ("alibaba", "qwen", true, false),
            ("alibaba", "deepseek", false, false),
            ("alibaba-cn", "qwen", true, false),
            ("alibaba-coding-plan", "qwen", true, false),
            ("alibaba-token-plan-cn", "qwen", true, false),
            ("zai", "glm", true, false),
            ("zai-coding-plan", "glm", true, false),
            ("zhipuai", "glm", true, false),
            ("zhipuai-coding-plan", "glm", true, false),
            ("deepseek", "deepseek", false, false),
            ("groq", "", false, false),
            ("moonshotai", "kimi", false, false),
            ("moonshotai-cn", "kimi", false, false),
            ("kimi-code-plan-cn", "", false, false),
            ("kimi-code-plan-global", "", false, false),
            ("volcengine", "doubao", false, false),
            ("volcengine-coding-plan", "doubao", false, false),
            ("openai", "", false, true),
            ("xai", "", false, true),
            ("mistral", "", false, true),
            ("cerebras", "", false, true),
            ("github-copilot", "", false, true),
            ("perplexity", "", false, true),
            ("fireworks-ai", "", false, true),
            ("siliconflow", "", false, true),
            ("siliconflow-cn", "", false, true),
            ("ollama-cloud", "", false, true),
            ("openai-codex", "", false, true),
        ];
        for (id, model_needle, cache, key) in pinned {
            let provider = document
                .provider(id)
                .unwrap_or_else(|| panic!("missing {id}"));
            let model = provider
                .models
                .iter()
                .find(|model| model.id.contains(model_needle))
                .unwrap_or_else(|| panic!("{id} has no model containing {model_needle:?}"));
            let body = match provider.kind.as_str() {
                "anthropic-messages" => anthropic_body(&model.id, &provider.base_url, &request),
                "openai-responses" => responses_body(&model.id, &provider.base_url, &request),
                _ => chat_body(&model.id, &provider.base_url, &request),
            };
            assert_eq!(
                count_cache_control(&body) > 0,
                cache,
                "{id} {} cache_control",
                model.id
            );
            assert_eq!(
                body.get("prompt_cache_key").is_some(),
                key,
                "{id} {} prompt_cache_key",
                model.id
            );
        }
    }

    #[test]
    fn prompt_cache_key_rejection_is_remembered_for_the_host() {
        let endpoint = "https://probe.example.test/v1/chat/completions";
        let error = ProviderError::with_message(
            ProviderErrorKind::Rejected,
            "HTTP 400: unknown field prompt_cache_key",
        );
        let original = br#"{"model":"x","prompt_cache_key":"session-1"}"#;
        let stripped = retry_body_without_prompt_cache_key(endpoint, &error, original)
            .expect("named 400 retries");
        let value: Value = serde_json::from_slice(&stripped).unwrap();
        assert!(value.get("prompt_cache_key").is_none());
        assert!(!wants_prompt_cache_key(endpoint));
        let mut body = json!({});
        apply_prompt_cache_key(&mut body, endpoint, Some("session-1"));
        assert!(body.get("prompt_cache_key").is_none());

        let unnamed =
            ProviderError::with_message(ProviderErrorKind::Rejected, "HTTP 400: invalid request");
        assert!(
            retry_body_without_prompt_cache_key(
                "https://other.example.test/v1/chat/completions",
                &unnamed,
                original
            )
            .is_none()
        );
        assert!(wants_prompt_cache_key(
            "https://other.example.test/v1/chat/completions"
        ));
    }

    #[test]
    fn usage_log_line_matches_the_qa_shape() {
        let line = usage_log_line(
            "minimax",
            "MiniMax-M2",
            &Usage {
                input_tokens: 12345,
                output_tokens: 420,
                cache_read_tokens: Some(11800),
                cache_write_tokens: None,
                prompt_tokens: 12345,
            },
        );
        assert_eq!(
            line,
            "[usage] provider=minimax model=MiniMax-M2 input=12345 cache_read=11800 cache_write=0 output=420"
        );
    }

    /// Request-body snapshots for the families this crate sends.
    ///
    /// Shapes follow `sst/opencode` `packages/opencode/src/provider/transform.ts`:
    /// `applyCaching` (up to four `cache_control` breakpoints), `reasoningVariants`
    /// / `reasoningEffort` / `providerOptions` / `smallOptions` (effort and
    /// thinking fields). Bedrock and Vertex are not catalog presets; a custom
    /// Anthropic URL still gets `cache_control`.
    #[test]
    fn opencode_family_snapshots_cover_effort_cache_and_usage() {
        let mut base = Request::new()
            .with_system_prompt("stable")
            .with_tool(tool("read"))
            .with_message(user("hello"));
        base.prompt_cache_key = Some("session-1".to_owned());

        let off = base.clone().with_reasoning(ReasoningLevel::Off);
        let max = base.with_reasoning(ReasoningLevel::Max);

        let claude = anthropic_body(
            "claude-sonnet-4-6",
            "https://api.anthropic.com/v1/messages",
            &max,
        );
        assert_eq!(claude["thinking"]["type"], "adaptive");
        assert_eq!(claude["output_config"]["effort"], "max");
        assert!(claude["thinking"].get("budget_tokens").is_none());
        assert_eq!(cache_type(&claude["tools"][0]), Some("ephemeral"));
        assert_eq!(cache_type(&claude["system"][0]), Some("ephemeral"));
        assert_eq!(
            cache_type(&claude["messages"][0]["content"][0]),
            Some("ephemeral")
        );
        assert!(count_cache_control(&claude) <= ANTHROPIC_BREAKPOINT_CAP);
        assert!(claude.get("prompt_cache_key").is_none());
        let claude_off = anthropic_body(
            "claude-sonnet-4-6",
            "https://api.anthropic.com/v1/messages",
            &off,
        );
        assert_eq!(claude_off["thinking"]["type"], "disabled");
        assert!(claude_off.get("output_config").is_none());
        assert!(count_cache_control(&claude_off) > 0);

        let older = anthropic_body(
            "claude-sonnet-4-5",
            "https://api.anthropic.com/v1/messages",
            &max,
        );
        assert_eq!(older["thinking"]["type"], "enabled");
        assert!(older["thinking"]["budget_tokens"].as_u64().unwrap_or(0) > 0);
        assert!(older.get("output_config").is_none());
        let older_off = anthropic_body(
            "claude-sonnet-4-5",
            "https://api.anthropic.com/v1/messages",
            &off,
        );
        assert_eq!(older_off["thinking"]["type"], "disabled");

        let openai = chat_body("gpt-5", "https://api.openai.com/v1/chat/completions", &max);
        assert_eq!(openai["reasoning_effort"], "max");
        assert!(openai.get("thinking").is_none());
        assert_eq!(openai["prompt_cache_key"], "session-1");
        assert_eq!(count_cache_control(&openai), 0);
        let openai_off = chat_body("gpt-5", "https://api.openai.com/v1/chat/completions", &off);
        assert_eq!(openai_off["reasoning_effort"], "none");

        let responses = responses_body("gpt-5", "https://api.openai.com/v1/responses", &max);
        assert_eq!(responses["reasoning"]["effort"], "max");
        assert!(responses.get("reasoning_effort").is_none());
        assert_eq!(responses["prompt_cache_key"], "session-1");
        assert_eq!(count_cache_control(&responses), 0);
        let responses_off = responses_body("gpt-5", "https://api.openai.com/v1/responses", &off);
        assert_eq!(responses_off["reasoning"]["effort"], "none");
        let codex = responses_body(
            "gpt-5",
            "https://chatgpt.com/backend-api/codex/responses",
            &max,
        );
        assert_eq!(codex["reasoning"]["effort"], "max");
        assert_eq!(codex["prompt_cache_key"], "session-1");

        let router = chat_body(
            "anthropic/claude-sonnet-4.6",
            "https://openrouter.ai/api/v1/chat/completions",
            &max,
        );
        assert_eq!(router["reasoning"]["effort"], "max");
        assert!(router.get("thinking").is_none());
        assert!(router.get("prompt_cache_key").is_none());
        assert_eq!(cache_type(&router["tools"][0]), Some("ephemeral"));
        assert_eq!(
            cache_type(&router["messages"][0]["content"][0]),
            Some("ephemeral")
        );
        assert_eq!(
            cache_type(&router["messages"][1]["content"][0]),
            Some("ephemeral")
        );
        assert!(count_cache_control(&router) <= ANTHROPIC_BREAKPOINT_CAP);
        let router_off = chat_body(
            "anthropic/claude-sonnet-4.6",
            "https://openrouter.ai/api/v1/chat/completions",
            &off,
        );
        assert_eq!(router_off["reasoning"]["effort"], "none");
        let gemini_router = chat_body(
            "google/gemini-2.5-pro",
            "https://openrouter.ai/api/v1/chat/completions",
            &max,
        );
        assert_eq!(gemini_router["reasoning"]["effort"], "max");
        assert!(count_cache_control(&gemini_router) > 0);
        assert!(gemini_router.get("prompt_cache_key").is_none());

        let gemini = chat_body(
            "gemini-2.5-pro",
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            &max,
        );
        assert_eq!(gemini["reasoning_effort"], "max");
        assert_eq!(count_cache_control(&gemini), 0);
        assert!(gemini.get("prompt_cache_key").is_none());
        let gemini_off = chat_body(
            "gemini-2.5-pro",
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            &off,
        );
        assert_eq!(gemini_off["reasoning_effort"], "none");

        let qwen = chat_body(
            "qwen3.7-max",
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            &max,
        );
        assert_eq!(qwen["enable_thinking"], true);
        assert_eq!(qwen["reasoning_effort"], "max");
        assert!(qwen.get("thinking").is_none());
        assert!(qwen.get("prompt_cache_key").is_none());
        assert_eq!(
            cache_type(&qwen["messages"][0]["content"][0]),
            Some("ephemeral")
        );
        assert!(count_cache_control(&qwen) <= ANTHROPIC_BREAKPOINT_CAP);
        let qwen_off = chat_body(
            "qwen3.7-max",
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
            &off,
        );
        assert_eq!(qwen_off["enable_thinking"], false);
        assert!(qwen_off.get("reasoning_effort").is_none());

        let glm = chat_body(
            "glm-5.3",
            "https://api.z.ai/api/paas/v4/chat/completions",
            &max,
        );
        assert_eq!(glm["thinking"]["type"], "enabled");
        assert_eq!(glm["thinking"]["clear_thinking"], false);
        assert_eq!(glm["reasoning_effort"], "max");
        assert!(glm.get("prompt_cache_key").is_none());
        assert_eq!(cache_type(&glm["tools"][0]), Some("ephemeral"));
        assert!(count_cache_control(&glm) <= ANTHROPIC_BREAKPOINT_CAP);
        let glm_off = chat_body(
            "glm-5.3",
            "https://open.bigmodel.cn/api/paas/v4/chat/completions",
            &off,
        );
        assert_eq!(glm_off["thinking"]["type"], "disabled");
        assert!(glm_off["thinking"].get("clear_thinking").is_none());
        assert!(glm_off.get("reasoning_effort").is_none());

        for (model, endpoint, key) in [
            ("grok-4", "https://api.x.ai/v1/chat/completions", true),
            (
                "llama-3.3-70b-versatile",
                "https://api.groq.com/openai/v1/chat/completions",
                false,
            ),
            (
                "mistral-large",
                "https://api.mistral.ai/v1/chat/completions",
                true,
            ),
            (
                "llama-3.3-70b",
                "https://llm.example.test/v1/chat/completions",
                true,
            ),
        ] {
            let body = chat_body(model, endpoint, &max);
            assert_eq!(body["reasoning_effort"], "max", "{model}");
            assert!(body.get("thinking").is_none(), "{model}");
            assert_eq!(count_cache_control(&body), 0, "{model}");
            assert_eq!(
                body.get("prompt_cache_key").and_then(Value::as_str),
                key.then_some("session-1"),
                "{model}"
            );
            let disabled = chat_body(model, endpoint, &off);
            assert_eq!(disabled["reasoning_effort"], "none", "{model}");
        }

        let mut flash = chat_body(
            "deepseek-flash",
            "https://api.deepseek.com/chat/completions",
            &max,
        );
        let alias = chat_body(
            "deepseek-v4-flash",
            "https://api.deepseek.com/chat/completions",
            &max,
        );
        flash["model"] = json!("deepseek-v4-flash");
        assert_eq!(flash, alias);
        assert_eq!(alias["thinking"]["type"], "enabled");
        assert_eq!(alias["reasoning_effort"], "max");
        assert!(alias.get("prompt_cache_key").is_none());
        assert_eq!(count_cache_control(&alias), 0);
        let pro = chat_body(
            "deepseek-v4-pro",
            "https://api.deepseek.com/chat/completions",
            &off,
        );
        assert_eq!(pro["thinking"]["type"], "disabled");
        assert!(pro.get("reasoning_effort").is_none());

        let document = crate::catalog::bundled();
        for id in ["bedrock", "vertex", "google", "azure"] {
            assert!(
                document.provider(id).is_none(),
                "{id} is not a catalog preset"
            );
        }
        assert!(document.providers.iter().all(|provider| {
            let id = provider.id.to_ascii_lowercase();
            !id.contains("bedrock") && !id.contains("vertex")
        }));
        let custom = anthropic_body(
            "claude-sonnet-4-6",
            "https://bedrock-runtime.us-east-1.amazonaws.com/anthropic/v1/messages",
            &max,
        );
        assert!(count_cache_control(&custom) > 0);
        assert!(custom.get("prompt_cache_key").is_none());
        assert_eq!(custom["thinking"]["type"], "adaptive");
        assert_eq!(custom["output_config"]["effort"], "max");

        let anthropic_usage = usage_from_value(&json!({
            "input_tokens": 100,
            "cache_read_input_tokens": 80,
            "cache_creation_input_tokens": 20,
            "output_tokens": 4,
        }));
        assert_eq!(anthropic_usage.cache_read_tokens, Some(80));
        assert_eq!(anthropic_usage.cache_write_tokens, Some(20));
        let openai_usage = usage_from_value(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 4,
            "prompt_tokens_details": { "cached_tokens": 80 },
        }));
        assert_eq!(openai_usage.cache_read_tokens, Some(80));
        assert_eq!(openai_usage.cache_write_tokens, None);
        let deepseek_usage = usage_from_value(&json!({
            "prompt_tokens": 100,
            "completion_tokens": 4,
            "prompt_cache_hit_tokens": 80,
            "prompt_cache_miss_tokens": 20,
        }));
        assert_eq!(deepseek_usage.cache_read_tokens, Some(80));
        assert_eq!(deepseek_usage.cache_write_tokens, None);
        assert_eq!(deepseek_usage.prompt_tokens, 100);
        let gemini_usage = usage_from_value(&json!({
            "promptTokenCount": 100,
            "candidatesTokenCount": 4,
            "cachedContentTokenCount": 80,
        }));
        assert_eq!(gemini_usage.cache_read_tokens, Some(80));
    }

    #[test]
    fn kimi_coding_plan_models_match_from_the_id_on_a_proxy() {
        let mut request = Request::new()
            .with_system_prompt("stable")
            .with_reasoning(ReasoningLevel::Max)
            .with_message(assistant("plan the edit", "visible answer", None))
            .with_message(user("next"));
        request.prompt_cache_key = Some("session-1".to_owned());
        let hosts = [
            "https://api.kimi.com/coding/v1/chat/completions",
            "http://127.0.0.1:18080/kimi/v1/chat/completions",
        ];
        for endpoint in hosts {
            let on_kimi = endpoint.contains("api.kimi.com");
            for model in ["k3", "k3-256k"] {
                let body = chat_body(model, endpoint, &request);
                assert_eq!(body["reasoning_effort"], "max", "{model} {endpoint}");
                assert!(body.get("thinking").is_none(), "{model} {endpoint}");
                assert_eq!(
                    body["messages"][1]["reasoning_content"], "plan the edit",
                    "{model} {endpoint}"
                );
                assert_eq!(body["messages"][1]["content"], "visible answer");
                assert_eq!(count_cache_control(&body), 0, "{model}");
                assert_eq!(
                    body.get("prompt_cache_key").is_some(),
                    !on_kimi,
                    "{model} {endpoint}"
                );
            }
            for model in ["kimi-for-coding", "kimi-for-coding-highspeed"] {
                let body = chat_body(model, endpoint, &request);
                assert_eq!(body["thinking"]["type"], "enabled", "{model} {endpoint}");
                assert!(body.get("reasoning_effort").is_none(), "{model}");
                assert_eq!(body["messages"][1]["reasoning_content"], "plan the edit");
                assert_eq!(count_cache_control(&body), 0);
                assert_eq!(body.get("prompt_cache_key").is_some(), !on_kimi);
            }
        }

        let off = Request::new().with_reasoning(ReasoningLevel::Off);
        let proxy = "http://127.0.0.1:18080/kimi/v1/chat/completions";
        for model in ["k3", "k3-256k"] {
            let body = chat_body(model, proxy, &off);
            assert!(body.get("thinking").is_none(), "{model}");
            assert!(body.get("reasoning_effort").is_none(), "{model}");
        }
        let coding_off = chat_body("kimi-for-coding", proxy, &off);
        assert_eq!(coding_off["thinking"]["type"], "disabled");
    }
}
