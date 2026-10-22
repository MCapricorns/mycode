use mycode_config::{ReasoningLevel, Request};

use super::apply_reasoning_effort;
use crate::family::minimax_target;

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
    assert_eq!(by_model["reasoning_split"], true);
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
    assert_eq!(completions["reasoning_split"], true);
    assert_ne!(completions["thinking"]["type"], "enabled");

    let responses = crate::openai_responses::build_body(
        "minimax/minimax-m2",
        "https://api.example.com/responses",
        &on,
    );
    assert_eq!(responses["thinking"]["type"], "adaptive");
    assert!(responses.get("reasoning_split").is_none());

    let minimax_messages = crate::anthropic_messages::build_body(
        "MiniMax-M2.5",
        "https://api.minimax.cn/anthropic/v1/messages",
        &on,
    );
    assert_eq!(minimax_messages["thinking"]["type"], "adaptive");
    assert!(minimax_messages["thinking"].get("budget_tokens").is_none());
    assert!(minimax_messages.get("reasoning_split").is_none());

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
    assert_eq!(body["thinking"]["clear_thinking"], false);
    assert_eq!(body["output_config"]["effort"], "high");
    assert!(body["thinking"].get("budget_tokens").is_none());
    assert!(body.get("reasoning_effort").is_none());

    let max = Request::new().with_reasoning(ReasoningLevel::Max);
    for endpoint in [
        "https://open.bigmodel.cn/api/anthropic/v1/messages",
        "https://api.z.ai/api/anthropic/v1/messages",
    ] {
        let glm53 = crate::anthropic_messages::build_body("glm-5.3", endpoint, &max);
        assert_eq!(glm53["thinking"]["type"], "enabled", "{endpoint}");
        assert_eq!(glm53["thinking"]["clear_thinking"], false, "{endpoint}");
        assert_eq!(glm53["output_config"]["effort"], "max", "{endpoint}");
        assert!(glm53.get("reasoning_effort").is_none(), "{endpoint}");
        assert!(
            glm53["thinking"].get("budget_tokens").is_none(),
            "{endpoint}"
        );
    }

    let off = crate::anthropic_messages::build_body(
        "glm-5.3",
        "https://api.z.ai/api/anthropic/v1/messages",
        &Request::new().with_reasoning(ReasoningLevel::Off),
    );
    assert_eq!(off["thinking"]["type"], "disabled");
    assert!(off["thinking"].get("clear_thinking").is_none());
    assert!(off.get("output_config").is_none());
    assert!(off.get("reasoning_effort").is_none());

    let mapped = [
        (ReasoningLevel::On, "max"),
        (ReasoningLevel::Minimal, "low"),
        (ReasoningLevel::Low, "low"),
        (ReasoningLevel::Medium, "high"),
        (ReasoningLevel::Xhigh, "max"),
    ];
    for (level, effort) in mapped {
        let body = crate::anthropic_messages::build_body(
            "glm-5.3",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            &Request::new().with_reasoning(level),
        );
        assert_eq!(body["output_config"]["effort"], effort, "{level:?}");
        assert_eq!(body["thinking"]["clear_thinking"], false, "{level:?}");
    }
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
fn k3_ids_send_reasoning_effort_from_the_model_id_alone() {
    let proxy = "http://127.0.0.1:18080/kimi/v1/chat/completions";
    let coding = "https://api.kimi.com/coding/v1/chat/completions";
    for endpoint in [proxy, coding] {
        for model in [
            "k3",
            "k3-256k",
            "K3-256K",
            "acme/k3",
            "acme/k3-256k",
            "kimi-k3",
        ] {
            let mut max = serde_json::json!({});
            apply_reasoning_effort(&mut max, model, endpoint, ReasoningLevel::Max);
            assert_eq!(max["reasoning_effort"], "max", "{model} {endpoint}");
            assert!(max.get("thinking").is_none(), "{model} {endpoint}");

            let mut off = serde_json::json!({});
            apply_reasoning_effort(&mut off, model, endpoint, ReasoningLevel::Off);
            assert!(off.get("thinking").is_none(), "{model} {endpoint}");
            assert!(off.get("reasoning_effort").is_none(), "{model} {endpoint}");
        }
        let mut coding_max = serde_json::json!({});
        apply_reasoning_effort(
            &mut coding_max,
            "kimi-for-coding",
            endpoint,
            ReasoningLevel::Max,
        );
        assert_eq!(coding_max["thinking"]["type"], "enabled", "{endpoint}");
        assert_eq!(coding_max["reasoning_effort"], "max", "{endpoint}");
        let mut coding_high = serde_json::json!({});
        apply_reasoning_effort(
            &mut coding_high,
            "vendor/kimi-for-coding",
            endpoint,
            ReasoningLevel::High,
        );
        assert_eq!(coding_high["thinking"]["type"], "enabled", "{endpoint}");
        assert_eq!(coding_high["reasoning_effort"], "high", "{endpoint}");
        let mut coding_low = serde_json::json!({});
        apply_reasoning_effort(
            &mut coding_low,
            "kimi-for-coding",
            endpoint,
            ReasoningLevel::Low,
        );
        assert_eq!(coding_low["reasoning_effort"], "low", "{endpoint}");
        let mut coding_off = serde_json::json!({});
        apply_reasoning_effort(
            &mut coding_off,
            "kimi-for-coding",
            endpoint,
            ReasoningLevel::Off,
        );
        assert_eq!(coding_off["thinking"]["type"], "disabled", "{endpoint}");
        assert!(coding_off.get("reasoning_effort").is_none(), "{endpoint}");

        let mut fast = serde_json::json!({});
        apply_reasoning_effort(
            &mut fast,
            "kimi-for-coding-highspeed",
            endpoint,
            ReasoningLevel::Max,
        );
        assert_eq!(fast["thinking"]["type"], "enabled", "{endpoint}");
        assert!(fast.get("reasoning_effort").is_none(), "{endpoint}");
        let mut fast_off = serde_json::json!({});
        apply_reasoning_effort(
            &mut fast_off,
            "kimi-for-coding-highspeed",
            endpoint,
            ReasoningLevel::Off,
        );
        assert_eq!(fast_off["thinking"]["type"], "disabled", "{endpoint}");
        assert!(fast_off.get("reasoning_effort").is_none(), "{endpoint}");
    }

    let mut medium = serde_json::json!({});
    apply_reasoning_effort(&mut medium, "k3-256k", proxy, ReasoningLevel::Medium);
    assert_eq!(medium["reasoning_effort"], "high");
    let mut low = serde_json::json!({});
    apply_reasoning_effort(&mut low, "k3", proxy, ReasoningLevel::Minimal);
    assert_eq!(low["reasoning_effort"], "low");

    for model in ["k30", "k3x", "mk3", "task-k3"] {
        let mut off = serde_json::json!({});
        apply_reasoning_effort(&mut off, model, proxy, ReasoningLevel::Off);
        assert_eq!(off["reasoning_effort"], "none", "{model}");
        assert!(off.get("thinking").is_none(), "{model}");
        assert_eq!(
            crate::family::reasoning_replay(model, proxy),
            super::ReasoningReplay::Omit,
            "{model}"
        );
    }
    let mut kimi_k30 = serde_json::json!({});
    apply_reasoning_effort(&mut kimi_k30, "kimi-k30", proxy, ReasoningLevel::Off);
    assert_eq!(kimi_k30["thinking"]["type"], "disabled");
    assert!(kimi_k30.get("reasoning_effort").is_none());
    assert_eq!(
        crate::family::reasoning_replay("k3", proxy),
        super::ReasoningReplay::Content
    );
    assert_eq!(
        crate::family::reasoning_replay(
            "k3-256k",
            "https://api.kimi.com/coding/v1/chat/completions"
        ),
        super::ReasoningReplay::Content
    );
    assert_eq!(
        crate::family::reasoning_replay(
            "kimi-for-coding",
            "http://127.0.0.1:18080/kimi/v1/chat/completions"
        ),
        super::ReasoningReplay::Content
    );
}

#[test]
fn deepseek_flash_and_v4_flash_share_the_effort_shape() {
    let endpoint = "https://api.deepseek.com/chat/completions";
    let mut flash_off = serde_json::json!({});
    let mut alias_off = serde_json::json!({});
    apply_reasoning_effort(
        &mut flash_off,
        "deepseek-flash",
        endpoint,
        ReasoningLevel::Off,
    );
    apply_reasoning_effort(
        &mut alias_off,
        "deepseek-v4-flash",
        endpoint,
        ReasoningLevel::Off,
    );
    assert_eq!(flash_off, alias_off);
    assert_eq!(flash_off["thinking"]["type"], "disabled");
    assert!(flash_off.get("reasoning_effort").is_none());

    let mut flash_max = serde_json::json!({});
    let mut alias_max = serde_json::json!({});
    let mut pro_max = serde_json::json!({});
    apply_reasoning_effort(
        &mut flash_max,
        "deepseek-flash",
        endpoint,
        ReasoningLevel::Max,
    );
    apply_reasoning_effort(
        &mut alias_max,
        "deepseek-v4-flash",
        endpoint,
        ReasoningLevel::Max,
    );
    apply_reasoning_effort(
        &mut pro_max,
        "deepseek-v4-pro",
        endpoint,
        ReasoningLevel::Max,
    );
    assert_eq!(flash_max, alias_max);
    assert_eq!(flash_max["thinking"]["type"], "enabled");
    assert_eq!(flash_max["reasoning_effort"], "max");
    assert_eq!(pro_max["thinking"]["type"], "enabled");
    assert_eq!(pro_max["reasoning_effort"], "max");

    let mut pro_off = serde_json::json!({});
    apply_reasoning_effort(
        &mut pro_off,
        "deepseek-v4-pro",
        endpoint,
        ReasoningLevel::Off,
    );
    assert_eq!(pro_off["thinking"]["type"], "disabled");
    assert!(pro_off.get("reasoning_effort").is_none());

    let mapped = [
        (ReasoningLevel::Minimal, "low"),
        (ReasoningLevel::Low, "low"),
        (ReasoningLevel::Medium, "high"),
        (ReasoningLevel::High, "high"),
        (ReasoningLevel::Xhigh, "high"),
        (ReasoningLevel::On, "high"),
        (ReasoningLevel::Max, "max"),
    ];
    for (level, effort) in mapped {
        let mut body = serde_json::json!({});
        apply_reasoning_effort(&mut body, "deepseek-flash", endpoint, level);
        assert_eq!(body["reasoning_effort"], effort, "{level:?}");
        assert_eq!(body["thinking"]["type"], "enabled", "{level:?}");
    }

    // The server 400s `deepseek-v4.1-flash` as an unknown model name.
    // The substring still selects the current-model fields.
    let mut rejected_name = serde_json::json!({});
    apply_reasoning_effort(
        &mut rejected_name,
        "deepseek-v4.1-flash",
        endpoint,
        ReasoningLevel::Max,
    );
    assert_eq!(rejected_name["reasoning_effort"], "max");
    assert_eq!(rejected_name["thinking"]["type"], "enabled");

    for model in [
        "deepseek-chat",
        "deepseek-reasoner",
        "deepseek-r1",
        "deepseek-v3",
    ] {
        let mut on = serde_json::json!({});
        apply_reasoning_effort(&mut on, model, endpoint, ReasoningLevel::Max);
        assert_eq!(on["thinking"]["type"], "enabled", "{model}");
        assert!(on.get("reasoning_effort").is_none(), "{model}");
        let mut off = serde_json::json!({});
        apply_reasoning_effort(&mut off, model, endpoint, ReasoningLevel::Off);
        assert_eq!(off["thinking"]["type"], "disabled", "{model}");
        assert!(off.get("reasoning_effort").is_none(), "{model}");
    }
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

    // Groq, Moonshot, Fireworks, SiliconFlow, and Volcengine report the
    // same nested cached_tokens field. Ollama usually omits it.
    for (label, usage) in [
        (
            "groq",
            serde_json::json!({"prompt_tokens": 50, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 40}}),
        ),
        (
            "moonshot",
            serde_json::json!({"prompt_tokens": 50, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 40}}),
        ),
        (
            "fireworks",
            serde_json::json!({"prompt_tokens": 50, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 40}}),
        ),
        (
            "siliconflow",
            serde_json::json!({"prompt_tokens": 50, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 40}}),
        ),
        (
            "volcengine",
            serde_json::json!({"prompt_tokens": 50, "completion_tokens": 2, "prompt_tokens_details": {"cached_tokens": 40}}),
        ),
    ] {
        let parsed = super::usage_from_value(&usage);
        assert_eq!(parsed.cache_read_tokens, Some(40), "{label}");
        assert_eq!(parsed.prompt_tokens, 50, "{label}");
    }
    let ollama = super::usage_from_value(&serde_json::json!({
        "prompt_tokens": 12,
        "completion_tokens": 3,
    }));
    assert_eq!(ollama.cache_read_tokens, None);
    assert_eq!(ollama.prompt_tokens, 12);
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
