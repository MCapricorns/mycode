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

use parking_lot::Mutex;
use std::collections::HashSet;

use serde_json::{Value, json};

use mycode_core::{ProviderError, ProviderErrorKind};

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
        mark_trailing_messages(body.get_mut("messages"), remaining.min(2));
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
    rejected_prompt_cache_keys().lock().contains(host)
}

fn remember_prompt_cache_key_rejected(host: &str) {
    rejected_prompt_cache_keys().lock().insert(host.to_owned());
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

/// Whether the request body sets a cache key, so the driver only attempts a
/// rewrite without that field for those requests.
#[must_use]
pub(crate) fn has_prompt_cache_key(body: &Value) -> bool {
    body.get("prompt_cache_key").is_some()
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

fn mark_trailing_messages(messages: Option<&mut Value>, budget: usize) {
    let Some(Value::Array(messages)) = messages else {
        return;
    };
    let mut marked = 0;
    for message in messages.iter_mut().rev() {
        if marked >= budget {
            break;
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
mod tests;
