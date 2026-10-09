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
    /// Stable name for tests and the PR table.
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
    chat_row(model, endpoint).name
}

/// Messages family name for the first matching thinking row.
#[cfg(test)]
#[must_use]
pub(crate) fn anthropic_family_name(model: &str, endpoint: &str) -> &'static str {
    anthropic_row(model, endpoint).name
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
    let _family = row.name;
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
    let _family = row.name;
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

fn chat_row(model: &str, endpoint: &str) -> &'static ChatRow {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    CHAT_THINKING
        .iter()
        .find(|row| (row.matches)(&model_id, &host))
        .unwrap_or_else(|| {
            CHAT_THINKING
                .last()
                .expect("chat thinking table ends with generic")
        })
}

fn anthropic_row(model: &str, endpoint: &str) -> &'static AnthropicRow {
    let model_id = model.to_ascii_lowercase();
    let host = endpoint.to_ascii_lowercase();
    ANTHROPIC_THINKING
        .iter()
        .find(|row| (row.matches)(&model_id, &host))
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
    if level == ReasoningLevel::Off {
        body["thinking"] = json!({ "type": "disabled" });
        return;
    }
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
    if is_k3_model(model_id) {
        if let Some(token) = k3_effort(level) {
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
    } else {
        body["thinking"] = json!({ "type": "enabled" });
    }
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
        body["thinking"] = json!({ "type": "disabled" });
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
mod tests {
    use mycode_core::ReasoningLevel;
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
}
