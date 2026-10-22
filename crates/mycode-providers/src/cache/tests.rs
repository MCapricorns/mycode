use mycode_config::{
    AssistantMessage, ContentBlock, Message, ReasoningLevel, Request, StopReason, TextBlock,
    ThinkingBlock, ToolSpec, UserMessage,
};
use serde_json::{Value, json};

use super::{
    ANTHROPIC_BREAKPOINT_CAP, apply_chat_cache_breakpoints, apply_prompt_cache_key,
    clamp_prompt_cache_key, explicit_chat_cache, openrouter_session_header,
    retry_body_without_prompt_cache_key, wants_prompt_cache_key,
};
use crate::anthropic_messages::build_body as anthropic_body;
use crate::openai_completions::build_body as chat_body;
use crate::openai_responses::build_body as responses_body;
use crate::wire_common::usage_from_value;
use mycode_config::{ProviderError, ProviderErrorKind};

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
    assert_eq!(minimax["thinking"]["type"], "adaptive");
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

mod families;
