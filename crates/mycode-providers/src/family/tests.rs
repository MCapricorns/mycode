use mycode_config::ReasoningLevel;
use serde_json::json;

use super::{CHAT_THINKING, ChatFamily, anthropic_family_name, chat_family_name};
use crate::wire_common::{ReasoningReplay, ThinkingWire};

#[test]
fn chat_thinking_table_ends_with_the_generic_row() {
    let last = CHAT_THINKING.last().expect("table");
    assert_eq!(last.family, ChatFamily::Generic);
    assert_eq!(last.name, "generic");
    assert!((last.matches)(
        "brand-new-model",
        "https://api.brand-new.example/v1"
    ));
    let names: Vec<_> = CHAT_THINKING.iter().map(|row| row.name).collect();
    assert_eq!(
        names,
        [
            "openrouter",
            "dashscope",
            "opendesign",
            "zai",
            "minimax",
            "kimi",
            "qwen",
            "deepseek",
            "generic",
        ]
    );
}

#[test]
fn host_rows_win_over_model_rows_and_unknown_is_generic() {
    assert_eq!(
        chat_family_name("glm-5.3", "https://openrouter.ai/api/v1/chat/completions"),
        "openrouter"
    );
    assert_eq!(
        chat_family_name(
            "deepseek-v4-pro",
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        ),
        "dashscope"
    );
    assert_eq!(
        chat_family_name("glm-5.3", "https://api.z.ai/api/paas/v4/chat/completions"),
        "zai"
    );
    assert_eq!(
        chat_family_name("k3", "http://127.0.0.1:18080/kimi/v1/chat/completions"),
        "kimi"
    );
    assert_eq!(
        chat_family_name(
            "brand-new-model",
            "https://api.brand-new.example/v1/chat/completions"
        ),
        "generic"
    );
    assert_eq!(
        anthropic_family_name("brand-new-model", "https://api.anthropic.com/v1/messages"),
        "claude-budget"
    );
    assert_eq!(
        anthropic_family_name("claude-sonnet-4-6", "https://api.anthropic.com/v1/messages"),
        "claude-adaptive"
    );
}

#[test]
fn claude_5_off_is_lowest_effort_and_older_claude_is_disabled() {
    let endpoint = "https://api.anthropic.com/v1/messages";
    for model in [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-opus-5-6",
        "claude-haiku-5-5",
        "claude-fable-5-1",
        "anthropic.claude-opus-5",
    ] {
        let mut body = json!({});
        super::apply_anthropic_thinking_policy(&mut body, model, endpoint, ReasoningLevel::Off);
        assert!(body.get("thinking").is_none(), "{model}: {body}");
        assert_eq!(body["output_config"]["effort"], "low", "{model}");
        assert!(super::keeps_thinking_when_off(model), "{model}");
    }
    for model in ["claude-sonnet-4-6", "claude-opus-4-8", "claude-sonnet-4-5"] {
        let mut body = json!({});
        super::apply_anthropic_thinking_policy(&mut body, model, endpoint, ReasoningLevel::Off);
        assert_eq!(body["thinking"]["type"], "disabled", "{model}");
        assert!(!super::keeps_thinking_when_off(model), "{model}");
    }
    assert!(!super::keeps_thinking_when_off("glm-5.3"));
    assert!(super::keeps_thinking_when_off("k3-256k"));
}

/// MiniMax has no off switch: M3.1 400s on `thinking.type = "disabled"`,
/// so `Off` must wire `adaptive` — the only mode the family accepts.
#[test]
fn minimax_off_degrades_to_adaptive() {
    for endpoint in [
        "https://api.minimax.cn/anthropic/v1/messages",
        "https://api.minimaxi.com/anthropic/v1/messages",
    ] {
        let mut body = json!({});
        super::apply_anthropic_thinking_policy(
            &mut body,
            "MiniMax-M3.1-Flash-Preview",
            endpoint,
            ReasoningLevel::Off,
        );
        assert_eq!(body["thinking"]["type"], "adaptive", "{endpoint}: {body}");
        assert!(
            super::keeps_thinking_when_off("MiniMax-M3.1-Flash-Preview"),
            "{endpoint}"
        );
    }
    let mut body = json!({});
    super::apply_chat_thinking(
        &mut body,
        "minimax-m2",
        "https://api.minimax.cn/v1/chat/completions",
        ReasoningLevel::Off,
        super::ThinkingWire::Completions,
    );
    assert_eq!(body["thinking"]["type"], "adaptive", "{body}");
    assert_eq!(body["reasoning_split"], true, "{body}");
}

#[test]
fn opendesign_sends_plain_reasoning_effort_for_vendor_models() {
    let endpoint = "https://amr-link.open-design.ai/v1/chat/completions";
    for model in ["glm-5.3-flash", "deepseek-v4-flash", "kimi-k2.7-code"] {
        assert_eq!(chat_family_name(model, endpoint), "opendesign");
        let mut body = json!({});
        super::apply_chat_thinking(
            &mut body,
            model,
            endpoint,
            ReasoningLevel::Xhigh,
            ThinkingWire::Completions,
        );
        assert_eq!(body["reasoning_effort"], "xhigh", "{model}");
        assert!(body.get("thinking").is_none(), "{model}: {body}");
    }
    let mut off = json!({});
    super::apply_chat_thinking(
        &mut off,
        "deepseek-v4-flash",
        endpoint,
        ReasoningLevel::Off,
        ThinkingWire::Completions,
    );
    assert_eq!(off["reasoning_effort"], "none");
}

#[test]
fn unknown_provider_uses_generic_openai_fields() {
    let endpoint = "https://api.brand-new.example/v1/chat/completions";
    let mut max = json!({});
    super::apply_chat_thinking(
        &mut max,
        "brand-new-model",
        endpoint,
        ReasoningLevel::Max,
        ThinkingWire::Completions,
    );
    assert_eq!(max["reasoning_effort"], "max");
    assert!(max.get("thinking").is_none());

    let mut off = json!({});
    super::apply_chat_thinking(
        &mut off,
        "brand-new-model",
        endpoint,
        ReasoningLevel::Off,
        ThinkingWire::Completions,
    );
    assert_eq!(off["reasoning_effort"], "none");

    let mut responses = json!({});
    super::apply_chat_thinking(
        &mut responses,
        "brand-new-model",
        "https://api.brand-new.example/v1/responses",
        ReasoningLevel::Max,
        ThinkingWire::Responses,
    );
    assert_eq!(responses["reasoning"]["effort"], "max");
    assert!(responses.get("reasoning_effort").is_none());

    assert_eq!(
        super::reasoning_replay("brand-new-model", endpoint),
        ReasoningReplay::Omit
    );
    assert!(!super::explicit_chat_cache("brand-new-model", endpoint));
    assert!(!super::omits_prompt_cache_key(endpoint));
}

#[test]
fn kimi_effort_follows_the_id_mapping() {
    for (level, token) in [
        (ReasoningLevel::Low, "low"),
        (ReasoningLevel::Medium, "high"),
        (ReasoningLevel::Max, "max"),
    ] {
        assert_eq!(super::kimi_effort_token(level), Some(token));
    }
    let mut body = json!({});
    super::apply_chat_thinking(
        &mut body,
        "kimi-for-coding",
        "http://127.0.0.1:9/v1/chat/completions",
        ReasoningLevel::Max,
        ThinkingWire::Completions,
    );
    assert_eq!(body["reasoning_effort"], "max");
}
