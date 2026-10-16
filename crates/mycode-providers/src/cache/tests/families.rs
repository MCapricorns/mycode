//! Per-family request snapshots: cache fields, effort, usage, and schemas.

use super::*;

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
fn prompt_cache_key_rejection_is_remembered_for_the_host() {
    let endpoint = "https://probe.example.test/v1/chat/completions";
    let error = ProviderError::with_message(
        ProviderErrorKind::Rejected,
        "HTTP 400: unknown field prompt_cache_key",
    );
    let original = br#"{"model":"x","prompt_cache_key":"session-1"}"#;
    let stripped =
        retry_body_without_prompt_cache_key(endpoint, &error, original).expect("named 400 retries");
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
    // Unpublished adaptive Messages uses OUTPUT_TOKEN_MAX. No budget raise.
    assert_eq!(claude["max_tokens"], 32_000);
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
    assert_eq!(older["thinking"]["budget_tokens"], 32_768);
    // Max budget is 32768. The 32000 fallback is not larger, so the
    // budget row still raises max_tokens to budget + 4096.
    assert_eq!(older["max_tokens"], 36_864);
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
    assert!(openai.get("max_tokens").is_none());
    assert_eq!(openai["prompt_cache_key"], "session-1");
    assert_eq!(count_cache_control(&openai), 0);
    let openai_off = chat_body("gpt-5", "https://api.openai.com/v1/chat/completions", &off);
    assert_eq!(openai_off["reasoning_effort"], "none");

    let responses = responses_body("gpt-5", "https://api.openai.com/v1/responses", &max);
    assert_eq!(responses["reasoning"]["effort"], "max");
    assert!(responses.get("reasoning_effort").is_none());
    assert!(responses.get("max_output_tokens").is_none());
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

/// `OUTPUT_TOKEN_MAX` fills a missing Messages limit. A published
/// `limit.output` is not passed through `maxOutputTokens`'s
/// `Math.min(..., 32000)`. Chat and Responses still omit the field
/// when the catalog publishes nothing.
#[test]
fn unpublished_messages_max_tokens_follows_output_token_max_without_a_cap() {
    let endpoint = "https://api.anthropic.com/v1/messages";
    let high = Request::new().with_reasoning(ReasoningLevel::High);
    let high_body = anthropic_body("claude-sonnet-4-5", endpoint, &high);
    assert_eq!(high_body["thinking"]["budget_tokens"], 16_384);
    // 32000 is already above the High budget, so it stays 32000
    // (the old 8192 fallback was raised to 20480).
    assert_eq!(high_body["max_tokens"], 32_000);

    let mut zero = Request::new().with_reasoning(ReasoningLevel::Max);
    zero.max_output_tokens = Some(0);
    let zero_body = anthropic_body("claude-sonnet-4-6", endpoint, &zero);
    assert_eq!(zero_body["max_tokens"], 32_000);
    assert_eq!(zero_body["thinking"]["type"], "adaptive");

    let mut published = Request::new().with_reasoning(ReasoningLevel::Max);
    published.max_output_tokens = Some(131_072);
    let glm = anthropic_body(
        "glm-5.3",
        "https://api.z.ai/api/anthropic/v1/messages",
        &published,
    );
    assert_eq!(glm["max_tokens"], 131_072);
    let older = anthropic_body("claude-sonnet-4-5", endpoint, &published);
    assert_eq!(older["max_tokens"], 131_072);
    assert_eq!(older["thinking"]["budget_tokens"], 32_768);

    let chat = "https://api.openai.com/v1/chat/completions";
    let responses = "https://api.openai.com/v1/responses";
    let bare = Request::new();
    assert!(chat_body("gpt-5", chat, &bare).get("max_tokens").is_none());
    assert!(
        responses_body("gpt-5", responses, &bare)
            .get("max_output_tokens")
            .is_none()
    );
    assert_eq!(chat_body("gpt-5", chat, &published)["max_tokens"], 131_072);
    assert_eq!(
        responses_body("gpt-5", responses, &published)["max_output_tokens"],
        131_072
    );
    let mut unpublished_zero = published;
    unpublished_zero.max_output_tokens = Some(0);
    assert!(
        chat_body("gpt-5", chat, &unpublished_zero)
            .get("max_tokens")
            .is_none()
    );
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
        let coding = chat_body("kimi-for-coding", endpoint, &request);
        assert_eq!(coding["thinking"]["type"], "enabled", "{endpoint}");
        assert_eq!(coding["reasoning_effort"], "max", "{endpoint}");
        assert_eq!(coding["messages"][1]["reasoning_content"], "plan the edit");
        assert_eq!(count_cache_control(&coding), 0);
        assert_eq!(coding.get("prompt_cache_key").is_some(), !on_kimi);
        let fast = chat_body("kimi-for-coding-highspeed", endpoint, &request);
        assert_eq!(fast["thinking"]["type"], "enabled", "{endpoint}");
        assert!(fast.get("reasoning_effort").is_none(), "{endpoint}");
        assert_eq!(fast["messages"][1]["reasoning_content"], "plan the edit");
        assert_eq!(count_cache_control(&fast), 0);
        assert_eq!(fast.get("prompt_cache_key").is_some(), !on_kimi);
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
    assert!(coding_off.get("reasoning_effort").is_none());
}

/// models.dev publishes `low` / `high` / `max` for `kimi-for-coding`.
/// OpenCode sends that as `reasoningEffort` on `@ai-sdk/openai-compatible`
/// (`reasoningEffort` in `transform.ts`), which is wire `reasoning_effort`,
/// in addition to the K2 `thinking` toggle. `kimi-for-coding-highspeed`
/// publishes no effort list.
#[test]
fn kimi_for_coding_sends_published_effort_beside_thinking() {
    let hosts = [
        "https://api.kimi.com/coding/v1/chat/completions",
        "http://127.0.0.1:18080/kimi/v1/chat/completions",
    ];
    let cases = [
        (ReasoningLevel::Low, Some("low"), "enabled"),
        (ReasoningLevel::High, Some("high"), "enabled"),
        (ReasoningLevel::Max, Some("max"), "enabled"),
        (ReasoningLevel::Off, None, "disabled"),
    ];
    for endpoint in hosts {
        for (level, effort, thinking) in cases {
            let body = chat_body(
                "kimi-for-coding",
                endpoint,
                &Request::new().with_reasoning(level),
            );
            assert_eq!(body["thinking"]["type"], thinking, "{level:?} {endpoint}");
            assert_eq!(
                body.get("reasoning_effort").and_then(Value::as_str),
                effort,
                "{level:?} {endpoint}"
            );
            let fast = chat_body(
                "kimi-for-coding-highspeed",
                endpoint,
                &Request::new().with_reasoning(level),
            );
            assert!(
                fast.get("reasoning_effort").is_none(),
                "{level:?} {endpoint}"
            );
        }
    }
}

/// Moonshot rejects schemars `$ref` + `$defs` as infinite recursion.
/// OpenCode `sanitizeMoonshot` keeps a bare `$ref` and `sanitizeGemini`
/// copies `$ref` through. Every wire inlines the refs instead.
#[test]
fn tool_schemas_inline_refs_on_every_wire() {
    let spec = ToolSpec {
        name: "edit".to_owned(),
        description: "edit a file".to_owned(),
        params_schema: json!({
            "type": "object",
            "$defs": {
                "EditOp": {
                    "type": "object",
                    "properties": {
                        "occurrence": {
                            "$ref": "#/$defs/Occurrence",
                            "description": "which match"
                        }
                    }
                },
                "Occurrence": { "type": "string", "enum": ["unique", "all", "nth"] }
            },
            "properties": {
                "operations": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/EditOp" }
                }
            }
        }),
    };
    let request = Request::new().with_tool(spec);
    let bodies = [
        chat_body(
            "kimi-for-coding",
            "https://api.kimi.com/coding/v1/chat/completions",
            &request,
        ),
        chat_body(
            "gpt-5",
            "https://api.openai.com/v1/chat/completions",
            &request,
        ),
        responses_body("gpt-5", "https://api.openai.com/v1/responses", &request),
        anthropic_body(
            "claude-sonnet-4-6",
            "https://api.anthropic.com/v1/messages",
            &request,
        ),
    ];
    for body in &bodies {
        let text = body.to_string();
        assert!(!text.contains("\"$ref\""), "{text}");
        assert!(!text.contains("\"$defs\""), "{text}");
        assert!(!text.contains("\"definitions\""), "{text}");
        assert!(text.contains("unique"), "{text}");
        assert!(text.contains("which match"), "{text}");
    }
}
