//! One table per concern, keyed by provider or model family.
//!
//! OpenCode decides the same things in
//! `packages/opencode/src/provider/transform.ts` (`variants`, `options`,
//! `smallOptions`, `applyCaching`, `reasoningVariants`) after
//! `packages/opencode/src/provider/provider.ts` `fromModelsDevModel` picks
//! `api.npm` (unknown providers fall back to `@ai-sdk/openai-compatible`).
//! The first matching row wins. The last row of each table is that generic
//! fallback, so a new model on an unknown host needs no code change.

use serde_json::{Value, json};

use mycode_core::ReasoningLevel;

use crate::wire_common::{ReasoningReplay, ThinkingWire};

/// Hosts that reject `prompt_cache_key` or cache by some other mechanism.
///
/// A substring match on the lowercased endpoint. Unknown hosts are not
/// listed: they are probed, and a 400 that names the field drops it for
/// the rest of the process.
pub(crate) const PROMPT_CACHE_KEY_OMIT_HOSTS: &[&str] = &[
    "openrouter.ai",
    "api.z.ai",
    "bigmodel.cn",
    "zhipu",
    "deepseek.com",
    "minimax",
    "dashscope",
    "aliyuncs.com",
    "moonshot.ai",
    "moonshot.cn",
    "kimi.com",
    "kimi.ai",
    "api.groq.com",
    "api.together.ai",
    "api.together.xyz",
    "volces.com",
    "volcengine.com",
    "qianfan",
    "baidubce.com",
    "generativelanguage.googleapis.com",
    // Messages hosts reject unknown chat fields. The Messages builder never
    // writes prompt_cache_key; this keeps a completions-shaped call on the
    // same host from probing it.
    "api.anthropic.com",
    "freemodel.dev",
    "subconscious.dev",
    "thinkingmachines.dev",
];

/// Usage JSON keys. One parser accepts the union because the payload does
/// not name the provider. DeepSeek `prompt_cache_miss_tokens` is absent on
/// purpose: `prompt_tokens` already includes hit and miss.
pub(crate) const USAGE_INPUT_KEYS: &[&str] = &[
    "input_tokens",
    "prompt_tokens",
    "input",
    "promptTokenCount",
    "prompt_token_count",
];

pub(crate) const USAGE_OUTPUT_KEYS: &[&str] = &[
    "output_tokens",
    "completion_tokens",
    "output",
    "candidatesTokenCount",
    "candidates_token_count",
];

pub(crate) const USAGE_CACHE_READ_KEYS: &[&str] = &[
    "cache_read_tokens",
    "cache_read_input_tokens",
    "cached_tokens",
    "prompt_cache_hit_tokens",
    "cachedContentTokenCount",
    "cached_content_token_count",
];

pub(crate) const USAGE_CACHE_READ_NESTED_KEYS: &[&str] = &["cached_tokens", "cache_read_tokens"];

pub(crate) const USAGE_CACHE_WRITE_KEYS: &[&str] = &[
    "cache_creation_input_tokens",
    "cache_write_input_tokens",
    "cache_write_tokens",
];

pub(crate) const USAGE_CACHE_WRITE_NESTED_KEYS: &[&str] =
    &["cache_write_tokens", "cache_creation_input_tokens"];

pub(crate) const USAGE_CACHE_WRITE_EPHEMERAL_KEYS: &[&str] =
    &["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatFamily {
    OpenRouter,
    DashScope,
    Zai,
    MiniMax,
    Kimi,
    Qwen,
    DeepSeek,
    /// `@ai-sdk/openai-compatible` in OpenCode: `reasoning_effort` only.
    Generic,
}

struct ChatRow {
    family: ChatFamily,
    /// Stable name for tests and the PR table; only the cfg(test) name
    /// accessors read it, so the field is dead in a non-test build.
    #[cfg_attr(not(test), allow(dead_code))]
    name: &'static str,
    matches: fn(&str, &str) -> bool,
}

/// Chat-completions thinking, first match wins.
///
/// Host rows come before model rows so OpenRouter and DashScope keep their
/// own field even when the model id is GLM, Kimi, or DeepSeek.
const CHAT_THINKING: &[ChatRow] = &[
    ChatRow {
        family: ChatFamily::OpenRouter,
        name: "openrouter",
        matches: host_openrouter,
    },
    ChatRow {
        family: ChatFamily::DashScope,
        name: "dashscope",
        matches: host_dashscope,
    },
    // The OpenDesign gateway takes plain `reasoning_effort` for every model
    // it routes (verified per level against the gateway); the vendor
    // `thinking` objects of the GLM, Kimi, and DeepSeek rows are not its API.
    ChatRow {
        family: ChatFamily::Generic,
        name: "opendesign",
        matches: host_opendesign,
    },
    ChatRow {
        family: ChatFamily::Zai,
        name: "zai",
        matches: glm_target,
    },
    ChatRow {
        family: ChatFamily::MiniMax,
        name: "minimax",
        matches: minimax_target,
    },
    ChatRow {
        family: ChatFamily::Kimi,
        name: "kimi",
        matches: match_kimi,
    },
    ChatRow {
        family: ChatFamily::Qwen,
        name: "qwen",
        matches: match_qwen,
    },
    ChatRow {
        family: ChatFamily::DeepSeek,
        name: "deepseek",
        matches: match_deepseek,
    },
    ChatRow {
        family: ChatFamily::Generic,
        name: "generic",
        matches: always,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnthropicFamily {
    MiniMax,
    Kimi,
    Zai,
    ClaudeAdaptive,
    /// Older Claude, and any other model on the Messages wire.
    ClaudeBudget,
}

struct AnthropicRow {
    family: AnthropicFamily,
    /// Stable name for tests and the PR table; only the cfg(test) name
    /// accessors read it, so the field is dead in a non-test build.
    #[cfg_attr(not(test), allow(dead_code))]
    name: &'static str,
    matches: fn(&str, &str) -> bool,
}

const ANTHROPIC_THINKING: &[AnthropicRow] = &[
    AnthropicRow {
        family: AnthropicFamily::MiniMax,
        name: "minimax",
        matches: minimax_target,
    },
    AnthropicRow {
        family: AnthropicFamily::Kimi,
        name: "kimi",
        matches: match_kimi,
    },
    AnthropicRow {
        family: AnthropicFamily::Zai,
        name: "zai",
        matches: glm_target,
    },
    AnthropicRow {
        family: AnthropicFamily::ClaudeAdaptive,
        name: "claude-adaptive",
        matches: match_claude_adaptive,
    },
    AnthropicRow {
        family: AnthropicFamily::ClaudeBudget,
        name: "claude-budget",
        matches: always,
    },
];

struct ReplayRow {
    kind: ReasoningReplay,
    matches: fn(&str, &str) -> bool,
}

const REPLAY: &[ReplayRow] = &[
    ReplayRow {
        kind: ReasoningReplay::MiniMax,
        matches: minimax_target,
    },
    ReplayRow {
        kind: ReasoningReplay::Content,
        matches: match_content_replay,
    },
    ReplayRow {
        kind: ReasoningReplay::Omit,
        matches: always,
    },
];

/// `None` means this row does not claim the host. The first `Some` wins.
/// OpenRouter claims every `openrouter.ai` host so GLM there does not fall
/// through to another row.
struct CacheRow {
    claims: fn(&str, &str) -> Option<bool>,
}

const CHAT_CACHE: &[CacheRow] = &[
    CacheRow {
        claims: claim_openrouter_cache,
    },
    CacheRow {
        claims: claim_zai_cache,
    },
    CacheRow {
        claims: claim_dashscope_cache,
    },
];

/// Chat family name for the first matching thinking row.
#[cfg(test)]
#[must_use]
pub(crate) fn chat_family_name(model: &str, endpoint: &str) -> &'static str {
    chat_row(&model.to_ascii_lowercase(), &endpoint.to_ascii_lowercase()).name
}

/// Messages family name for the first matching thinking row.
#[cfg(test)]
#[must_use]
pub(crate) fn anthropic_family_name(model: &str, endpoint: &str) -> &'static str {
    anthropic_row(&model.to_ascii_lowercase(), &endpoint.to_ascii_lowercase()).name
}

/// Applies chat or Responses reasoning fields from [`CHAT_THINKING`].
pub(crate) fn apply_chat_thinking(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
    wire: ThinkingWire,
) {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    let row = chat_row(&model_id, &host);
    match row.family {
        ChatFamily::OpenRouter => apply_openrouter(body, level),
        ChatFamily::DashScope => apply_dashscope(body, &model_id, level),
        ChatFamily::Zai => apply_zai(body, level),
        ChatFamily::MiniMax => apply_minimax(body, level, wire),
        ChatFamily::Kimi => apply_kimi_chat(body, &model_id, level),
        ChatFamily::Qwen => {
            apply_enable_thinking(body, level);
            if let Some(token) = effort_token(level) {
                body["reasoning_effort"] = json!(token);
            }
        }
        ChatFamily::DeepSeek => apply_deepseek(body, &model_id, level),
        ChatFamily::Generic => apply_openai_effort(body, &model_id, level, wire),
    }
}

/// Applies Messages thinking fields from [`ANTHROPIC_THINKING`].
pub(crate) fn apply_anthropic_thinking_policy(
    body: &mut Value,
    model: &str,
    endpoint: &str,
    level: ReasoningLevel,
) {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    let row = anthropic_row(&model_id, &host);
    match row.family {
        AnthropicFamily::MiniMax => apply_minimax(body, level, ThinkingWire::Anthropic),
        AnthropicFamily::Kimi => apply_kimi_anthropic(body, level),
        AnthropicFamily::Zai => apply_zai_anthropic(body, level),
        AnthropicFamily::ClaudeAdaptive => apply_claude_adaptive(body, &model_id, level),
        AnthropicFamily::ClaudeBudget => apply_claude_budget(body, level),
    }
}

/// How an assistant turn's reasoning is echoed on the next chat request.
pub(crate) fn reasoning_replay(model: &str, endpoint: &str) -> ReasoningReplay {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    REPLAY
        .iter()
        .find(|row| (row.matches)(&model_id, &host))
        .map(|row| row.kind)
        .unwrap_or(ReasoningReplay::Omit)
}

/// Whether a chat body should carry `cache_control` breakpoints.
#[must_use]
pub(crate) fn explicit_chat_cache(model: &str, endpoint: &str) -> bool {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    CHAT_CACHE
        .iter()
        .find_map(|row| (row.claims)(&model_id, &host))
        .unwrap_or(false)
}

/// MiniMax Messages pins the head and the tail. Every other Messages model
/// uses the last two cacheable blocks, matching OpenCode `applyCaching`.
#[must_use]
pub(crate) fn anthropic_pins_stable_ends(model: &str) -> bool {
    model.to_ascii_lowercase().contains("minimax")
}

/// True when this endpoint must not be probed with `prompt_cache_key`.
#[must_use]
pub(crate) fn omits_prompt_cache_key(endpoint: &str) -> bool {
    let endpoint = endpoint.to_ascii_lowercase();
    PROMPT_CACHE_KEY_OMIT_HOSTS
        .iter()
        .any(|host| endpoint.contains(host))
}

/// MiniMax (including minimaxi.com) rejects `thinking.type = "enabled"`.
#[must_use]
pub(crate) fn minimax_target(model: &str, endpoint: &str) -> bool {
    match_minimax(&model.to_ascii_lowercase(), &endpoint.to_ascii_lowercase())
}

/// Zhipu / Z.AI / BigModel, including a model id that contains `glm`.
#[must_use]
pub(crate) fn glm_target(model: &str, endpoint: &str) -> bool {
    match_zai(&model.to_ascii_lowercase(), &endpoint.to_ascii_lowercase())
}

/// First matching chat thinking row. Inputs are already lowercased.
fn chat_row(model_id: &str, host: &str) -> &'static ChatRow {
    CHAT_THINKING
        .iter()
        .find(|row| (row.matches)(model_id, host))
        .unwrap_or_else(|| {
            CHAT_THINKING
                .last()
                .expect("chat thinking table ends with generic")
        })
}

/// First matching Messages thinking row. Inputs are already lowercased.
fn anthropic_row(model_id: &str, host: &str) -> &'static AnthropicRow {
    ANTHROPIC_THINKING
        .iter()
        .find(|row| (row.matches)(model_id, host))
        .unwrap_or_else(|| {
            ANTHROPIC_THINKING
                .last()
                .expect("anthropic thinking table ends with budget")
        })
}

fn always(_model: &str, _host: &str) -> bool {
    true
}

fn host_openrouter(_model: &str, host: &str) -> bool {
    host.contains("openrouter.ai")
}

fn host_dashscope(_model: &str, host: &str) -> bool {
    host.contains("dashscope") || host.contains("aliyuncs")
}

fn host_opendesign(_model: &str, host: &str) -> bool {
    host.contains("open-design.ai")
}

fn match_zai(model: &str, host: &str) -> bool {
    model.contains("glm")
        || host.contains("z.ai")
        || host.contains("bigmodel")
        || host.contains("zhipu")
}

fn match_minimax(model: &str, host: &str) -> bool {
    model.contains("minimax") || host.contains("minimax")
}

fn match_kimi(model: &str, host: &str) -> bool {
    is_k3_model(model)
        || model.contains("kimi")
        || model.contains("moonshot")
        || model.contains("k2p")
        || host.contains("api.kimi.com")
        || host.contains("moonshot.ai")
        || host.contains("moonshot.cn")
        || host.contains("moonshotai.cn")
}

fn match_qwen(model: &str, _host: &str) -> bool {
    model.contains("qwen") || model.contains("qwq")
}

fn match_deepseek(model: &str, host: &str) -> bool {
    model.contains("deepseek") || host.contains("deepseek")
}

fn match_claude_adaptive(model: &str, _host: &str) -> bool {
    claude_adaptive(model)
}

fn match_content_replay(model: &str, host: &str) -> bool {
    match_zai(model, host)
        || match_deepseek(model, host)
        || model.contains("kimi")
        || model.contains("moonshot")
        || host.contains("moonshot")
        || host.contains("api.kimi.com")
        || host.contains("kimi.ai")
        || is_k3_model(model)
}

fn claim_openrouter_cache(model: &str, host: &str) -> Option<bool> {
    if !host.contains("openrouter.ai") {
        return None;
    }
    Some(model.contains("anthropic") || model.contains("claude") || model.contains("gemini"))
}

fn claim_zai_cache(_model: &str, host: &str) -> Option<bool> {
    if host.contains("api.z.ai") || host.contains("bigmodel.cn") || host.contains("zhipu") {
        Some(true)
    } else {
        None
    }
}

fn claim_dashscope_cache(model: &str, host: &str) -> Option<bool> {
    if !(host.contains("dashscope") || host.contains("aliyuncs.com")) {
        return None;
    }
    Some(model.contains("qwen") || model.contains("qwq"))
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

fn apply_zai_anthropic(body: &mut Value, level: ReasoningLevel) {
    // Claude Code on Z.AI's Anthropic route uses `thinking.type` and
    // `output_config.effort` (low / high / max). `reasoning_effort` is the
    // chat-completions name and is not sent here. `clear_thinking: false`
    // is preserved thinking: replayed reasoning has to stay byte-identical
    // or the prefix cache misses. `budget_tokens` is Anthropic's field and
    // this gateway does not take it.
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
    body["thinking"] = json!({ "type": "enabled", "clear_thinking": false });
    body["output_config"] = json!({ "effort": glm_anthropic_effort(level) });
}

fn apply_minimax(body: &mut Value, level: ReasoningLevel, wire: ThinkingWire) {
    // MiniMax has no off switch: M3.1 rejects `disabled` outright
    // ("requires adaptive thinking"), and `adaptive` is accepted by every
    // MiniMax model — `enabled` is the field the API actually refuses. So
    // `Off` degrades to the same wire shape as any other level; callers
    // sizing an output cap consult `keeps_thinking_when_off`.
    let _ = level;
    body["thinking"] = json!({ "type": "adaptive" });
    // Chat Completions inlines `<think>` unless this is set. The Messages
    // API already returns thinking blocks; an unknown field there can 400.
    if wire == ThinkingWire::Completions {
        body["reasoning_split"] = json!(true);
    }
}

fn apply_kimi_chat(body: &mut Value, model_id: &str, level: ReasoningLevel) {
    // K3 always thinks and rejects `thinking`. Effort is `low` / `high` /
    // `max` (default max). Off has no disable switch, so the field is omitted
    // rather than sending `{type:"disabled"}`, which this model 400s.
    // https://platform.kimi.com/docs/guide/use-reasoning-effort.md
    // A published models.dev list still gates the token. An id the active
    // catalog and the bundled baseline both omit keeps the id-based token so
    // coding-plan ids such as `k3` and `k3-256k` keep working.
    if is_k3_model(model_id) {
        if let Some(token) = kimi_effort_token(model_id, level, true) {
            body["reasoning_effort"] = json!(token);
        }
        return;
    }
    // K2.7 Code rejects `{type:"disabled"}`. Leaving the field off matches
    // that API and OpenCode, which has no off variant for it.
    if level == ReasoningLevel::Off && model_id.contains("kimi-k2.7-code") {
        return;
    }
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
    body["thinking"] = json!({ "type": "enabled" });
    // OpenCode `variants()` returns `{}` for an id containing `kimi`
    // (`packages/opencode/src/provider/transform.ts`) before the npm switch,
    // so that map never adds effort. When models.dev publishes an effort
    // list, `reasoningVariants` → `effortVariants` → `reasoningEffort` does.
    // `@ai-sdk/openai-compatible` returns `{ reasoningEffort }`, the wire
    // field `reasoning_effort`. The K2 toggle stays; the effort token is
    // sent beside it. The live or cached catalog is consulted first, then
    // the bundled baseline.
    if let Some(token) = kimi_effort_token(model_id, level, false) {
        body["reasoning_effort"] = json!(token);
    }
}

/// Effort token for a Kimi chat model.
///
/// `fallback_when_absent`: when neither the active nor bundled catalog lists
/// this model id, still send the id-based effort token. A row that lists a
/// toggle or an effort list gates the token the same way as every other Kimi
/// model.
fn kimi_effort_token(
    model_id: &str,
    level: ReasoningLevel,
    fallback_when_absent: bool,
) -> Option<&'static str> {
    let active = crate::catalog::active();
    effort_against(
        active.as_deref(),
        crate::catalog::bundled(),
        model_id,
        level,
        fallback_when_absent,
    )
}

fn effort_against(
    active: Option<&crate::catalog::CatalogDocument>,
    bundled: &crate::catalog::CatalogDocument,
    model_id: &str,
    level: ReasoningLevel,
    fallback_when_absent: bool,
) -> Option<&'static str> {
    let token = k3_effort(level)?;
    let publication = kimi_publication_from(active, bundled, model_id);
    match publication {
        Some(publication) if publication.published => publication
            .efforts
            .iter()
            .any(|effort| effort.eq_ignore_ascii_case(token))
            .then_some(token),
        _ if fallback_when_absent => Some(token),
        _ => None,
    }
}

struct KimiPublication {
    efforts: Vec<String>,
    published: bool,
}

/// Active catalog row when it names the model, otherwise the bundled row.
///
/// The first row that publishes a toggle or an effort list wins, the same
/// preference as `CatalogDocument::model_for_endpoint`. The id matches in
/// full, or as the last slash, colon, or backslash segment, so a capture
/// proxy can prefix the model.
fn kimi_publication_from(
    active: Option<&crate::catalog::CatalogDocument>,
    bundled: &crate::catalog::CatalogDocument,
    model_id: &str,
) -> Option<KimiPublication> {
    if let Some(active) = active
        && let Some(publication) = publication_in(active, model_id)
    {
        return Some(publication);
    }
    publication_in(bundled, model_id)
}

fn publication_in(
    catalog: &crate::catalog::CatalogDocument,
    model_id: &str,
) -> Option<KimiPublication> {
    let leaf = model_id.rsplit(['/', ':', '\\']).next().unwrap_or(model_id);
    let mut exact_options: Option<&crate::catalog::CatalogModel> = None;
    let mut leaf_options = None;
    let mut exact_any = None;
    let mut leaf_any = None;
    for provider in &catalog.providers {
        for model in &provider.models {
            let exact = model.id.eq_ignore_ascii_case(model_id);
            let leaf_hit = model.id.eq_ignore_ascii_case(leaf);
            if !exact && !leaf_hit {
                continue;
            }
            let published = model.reasoning_toggle || !model.reasoning_efforts.is_empty();
            if exact {
                if published && exact_options.is_none() {
                    exact_options = Some(model);
                }
                if exact_any.is_none() {
                    exact_any = Some(model);
                }
            }
            if leaf_hit {
                if published && leaf_options.is_none() {
                    leaf_options = Some(model);
                }
                if leaf_any.is_none() {
                    leaf_any = Some(model);
                }
            }
        }
    }
    let model = exact_options.or(leaf_options).or(exact_any).or(leaf_any)?;
    Some(KimiPublication {
        published: model.reasoning_toggle || !model.reasoning_efforts.is_empty(),
        efforts: model.reasoning_efforts.clone(),
    })
}

fn apply_kimi_anthropic(body: &mut Value, level: ReasoningLevel) {
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
    body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
    body["output_config"] = json!({ "effort": adaptive_effort(level) });
}

/// K3 effort tokens. `minimal` and `xhigh` are not on the K3 list.
fn k3_effort(level: ReasoningLevel) -> Option<&'static str> {
    match level {
        ReasoningLevel::Off | ReasoningLevel::On => None,
        ReasoningLevel::Minimal | ReasoningLevel::Low => Some("low"),
        ReasoningLevel::Medium | ReasoningLevel::High => Some("high"),
        ReasoningLevel::Xhigh | ReasoningLevel::Max => Some("max"),
    }
}

fn apply_enable_thinking(body: &mut Value, level: ReasoningLevel) {
    body["enable_thinking"] = json!(level != ReasoningLevel::Off);
}

fn apply_deepseek(body: &mut Value, model_id: &str, level: ReasoningLevel) {
    // Current chat models take the thinking toggle and `reasoning_effort`
    // `low` / `high` / `max` together. Chat does not accept `none`.
    // `deepseek-flash` and the legacy alias `deepseek-v4-flash` are one model.
    // https://api-docs.deepseek.com/guides/thinking_mode
    if deepseek_effort_model(model_id) {
        if level == ReasoningLevel::Off {
            body["thinking"] = json!({ "type": "disabled" });
            return;
        }
        body["thinking"] = json!({ "type": "enabled" });
        body["reasoning_effort"] = json!(deepseek_effort(level));
        return;
    }
    // Older ids (chat, reasoner, r1, v3) are the toggle. OpenCode's
    // `reasoningVariants` returns an empty map for those ids.
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
    } else {
        body["thinking"] = json!({ "type": "enabled" });
    }
}

fn deepseek_effort_model(model_id: &str) -> bool {
    model_id.contains("deepseek-flash") || model_id.contains("deepseek-v4")
}

/// Docs map minimal→low, low→low, medium→high, high→high, xhigh→high, max→max.
fn deepseek_effort(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Max => "max",
        ReasoningLevel::Off
        | ReasoningLevel::On
        | ReasoningLevel::Medium
        | ReasoningLevel::High
        | ReasoningLevel::Xhigh => "high",
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

fn apply_claude_adaptive(body: &mut Value, model_id: &str, level: ReasoningLevel) {
    if level == ReasoningLevel::Off {
        // Claude 5+ may answer `{type: "disabled"}` with a 400. Omitting
        // `thinking` runs them adaptive, and the lowest effort is the
        // closest they get to off.
        if claude_thinking_always_on(model_id) {
            body["output_config"] = json!({ "effort": "low" });
        } else {
            body["thinking"] = json!({ "type": "disabled" });
        }
        return;
    }
    let mut thinking = json!({ "type": "adaptive" });
    if claude_summarized_display(model_id) {
        thinking["display"] = json!("summarized");
    }
    body["thinking"] = thinking;
    body["output_config"] = json!({ "effort": adaptive_effort(level) });
}

fn apply_claude_budget(body: &mut Value, level: ReasoningLevel) {
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

/// Coding-plan effort on the Anthropic route. GLM-5.3 only accepts
/// `low` / `high` / `max`; enabled thinking with no effort token is `max`.
fn glm_anthropic_effort(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium | ReasoningLevel::High => "high",
        ReasoningLevel::Off => "low",
        ReasoningLevel::On | ReasoningLevel::Xhigh | ReasoningLevel::Max => "max",
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

/// K3 ids: `k3`, `k3-*`, `*/k3`, `*/k3-*`, and `kimi-k3` / `kimi-k3-*`.
///
/// `k30`, `k3x`, `mk3`, `task-k3`, and `kimi-k30` are not K3. OpenCode's
/// `isKimiFamily` only looks for "kimi" or "moonshot", so bare `k3` is a
/// row of our own: the coding plan publishes those ids, and a capture proxy
/// is not `api.kimi.com`.
fn is_k3_model(model_id: &str) -> bool {
    model_id.split(['/', '\\']).any(|segment| {
        segment == "k3"
            || segment.starts_with("k3-")
            || segment
                .strip_prefix("kimi-k3")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
    })
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

/// Claude generations that get no `thinking: disabled` for Off.
///
/// Fable, Mythos, Opus 5.5, and Sonnet 5.5 answer `disabled` with a 400,
/// and Opus 5 and Haiku 5.5 accept it only at some efforts. Which minor
/// release drops the switch next is not knowable ahead of time, so every
/// Claude 5 or later model takes the form all of them accept: adaptive
/// thinking at the lowest effort.
fn claude_thinking_always_on(model_id: &str) -> bool {
    if !model_id.contains("claude") {
        return false;
    }
    model_id.contains("fable")
        || model_id.contains("mythos")
        || claude_version(model_id).is_some_and(|(major, _)| major >= 5)
}

/// Whether `model` keeps thinking on when the request asks for
/// [`ReasoningLevel::Off`]. Thinking spends output tokens, so a caller that
/// sizes `max_tokens` for a no-thinking reply has to leave room for it.
#[must_use]
pub fn keeps_thinking_when_off(model: &str) -> bool {
    let model_id = model.to_ascii_lowercase();
    claude_thinking_always_on(&model_id)
        || is_k3_model(&model_id)
        || model_id.contains("kimi-k2.7-code")
        // MiniMax only ships `adaptive`; an `Off` request still thinks.
        || model_id.contains("minimax")
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

#[cfg(test)]
mod tests;
