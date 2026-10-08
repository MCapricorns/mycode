//! Prompt-cache markers.
//!
//! Anthropic Messages bodies (Anthropic, MiniMax `/anthropic`, and any other
//! Anthropic-compatible endpoint) get at most four
//! `cache_control: {type: "ephemeral"}` breakpoints: the last tool, the last
//! system text block, then up to two trailing cacheable message blocks.
//! OpenRouter Anthropic and Gemini models, DashScope Qwen, and Z.AI / Zhipu
//! GLM get the same budget on a chat-completions body. OpenAI, Azure, xAI,
//! Mistral, Cerebras, DeepInfra, and Venice get a stable `prompt_cache_key`.
//! DeepSeek and OpenAI-compatible MiniMax stay on implicit caching.
//!
//! Z.AI's implicit cache is per API key and per backend. A compaction summary
//! replaces the message prefix (one expected miss) and the summary request
//! itself is a large unrelated prompt, which drops that key's implicit entry.
//! The next two turns can share a byte-identical prefix and still report
//! `cached_tokens: 0`. `cache_control` breakpoints are the marker those
//! endpoints accept and that pins the new prefix; `prompt_cache_key` is not
//! part of Z.AI's published schema and is not sent.

use serde_json::{Value, json};

use mycode_core::{StreamEvent, Usage};

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

const PROMPT_CACHE_KEY_HOSTS: &[&str] = &[
    "api.openai.com",
    "openai.azure.com",
    "cognitiveservices.azure.com",
    "api.x.ai",
    "api.mistral.ai",
    "api.cerebras.ai",
    "api.deepinfra.com",
    "venice.ai",
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
    mark_trailing_messages(body.get_mut("messages"), remaining.min(2), &[]);
}

/// Chat-completions shape of the same four-breakpoint budget: last tool,
/// first system or developer message, then up to two later messages.
pub(crate) fn apply_chat_cache_breakpoints(body: &mut Value) {
    let mut remaining = ANTHROPIC_BREAKPOINT_CAP;
    if mark_last_object(body.get_mut("tools")) {
        remaining = remaining.saturating_sub(1);
    }
    if mark_first_system_message(body.get_mut("messages")) {
        remaining = remaining.saturating_sub(1);
    }
    mark_trailing_messages(
        body.get_mut("messages"),
        remaining.min(2),
        &["system", "developer"],
    );
}

/// OpenRouter Anthropic/Gemini, DashScope Qwen, and Z.AI / Zhipu GLM accept
/// explicit `cache_control`. DeepSeek, OpenRouter GLM, and DashScope GLM do not.
#[must_use]
pub(crate) fn explicit_chat_cache(model: &str, endpoint: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let endpoint = endpoint.to_ascii_lowercase();
    if endpoint.contains("openrouter.ai") {
        return model.contains("anthropic") || model.contains("claude") || model.contains("gemini");
    }
    // api.z.ai and open.bigmodel.cn (including coding-plan paths) accept
    // Anthropic-style breakpoints on the OpenAI-compatible body. That is what
    // keeps a post-compaction prefix cached when implicit cache was evicted.
    if endpoint.contains("api.z.ai")
        || endpoint.contains("bigmodel.cn")
        || endpoint.contains("zhipu")
    {
        return true;
    }
    let dashscope = endpoint.contains("dashscope") || endpoint.contains("aliyuncs.com");
    dashscope && (model.contains("qwen") || model.contains("qwq"))
}

/// Hosts that honor a stable `prompt_cache_key` on the JSON body.
#[must_use]
pub(crate) fn wants_prompt_cache_key(endpoint: &str) -> bool {
    let endpoint = endpoint.to_ascii_lowercase();
    PROMPT_CACHE_KEY_HOSTS
        .iter()
        .any(|host| endpoint.contains(host))
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
        AssistantMessage, ContentBlock, Message, Request, StopReason, TextBlock, ThinkingBlock,
        ToolSpec, Usage, UserMessage,
    };
    use serde_json::{Value, json};

    use super::{
        ANTHROPIC_BREAKPOINT_CAP, apply_prompt_cache_key, clamp_prompt_cache_key,
        explicit_chat_cache, openrouter_session_header, usage_log_line, wants_prompt_cache_key,
    };
    use crate::anthropic_messages::build_body as anthropic_body;
    use crate::openai_completions::build_body as chat_body;
    use crate::openai_responses::build_body as responses_body;

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
                    ContentBlock::Thinking(ThinkingBlock::new("hidden")),
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
            assert!(
                body["messages"][1]["content"]
                    .get("cache_control")
                    .is_none()
            );
            assert!(
                body["messages"][1]["content"].as_array().unwrap()[0]
                    .get("cache_control")
                    .is_none()
            );
            assert_eq!(
                cache_type(&body["messages"][2]["content"][0]),
                Some("ephemeral")
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
            assert!(
                body["messages"][1]["content"][0]
                    .get("cache_control")
                    .is_none(),
                "the summary stays inside the prefix of the later breakpoint"
            );
            assert_eq!(
                cache_type(&body["messages"][2]["content"][0]),
                Some("ephemeral")
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
}
