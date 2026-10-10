//! Message model: the shared conversation values exchanged by the user, the
//! model, and tools.
//!
//! Serde uses the default externally-tagged representation. Wire encoding
//! belongs to `mycode-providers`; durable session encoding belongs to
//! `mycode-agent`. See `docs/core.md`.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One history entry shared by the agent, the provider request, and live
/// events. Cloning bumps a refcount instead of copying tool output.
pub type SharedMessage = Arc<Message>;

/// One entry in a conversation history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Message {
    /// A message authored by the user (prompts).
    User(UserMessage),
    /// A message produced by the model.
    Assistant(AssistantMessage),
    /// The result of executing a tool call.
    ToolResult(ToolResultMessage),
}

/// A user-authored message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserMessage {
    pub content: Vec<ContentBlock>,
}

impl UserMessage {
    /// Builds a plain-text user message.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextBlock::new(text))],
        }
    }
}

/// A model-produced message: ordered content blocks plus turn metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub blocks: Vec<ContentBlock>,
    /// Token usage as reported by the provider, when available.
    pub usage: Option<Usage>,
    pub stop_reason: StopReason,
}

impl AssistantMessage {
    /// Concatenated text of all text content blocks.
    #[must_use]
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A single unit of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContentBlock {
    /// Plain text.
    Text(TextBlock),
    /// Model reasoning ("thinking") content.
    Thinking(ThinkingBlock),
    /// A request to invoke a tool.
    ToolCall(ToolCall),
    /// Binary payload (currently only used for images).
    Image(BinaryData),
}

/// Plain text content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    /// The text itself.
    pub text: String,
}

impl TextBlock {
    /// Creates plain text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl From<String> for TextBlock {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl Serialize for TextBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.text.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TextBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// Human-visible model reasoning or summary text.
///
/// The optional `signature` carries provider reasoning-integrity fields (e.g.
/// Anthropic thinking signatures). Adapters must preserve it verbatim across
/// round-trips, including signature-only blocks whose `text` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingBlock {
    /// The reasoning text.
    pub text: String,
    /// Provider reasoning-integrity signature, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Vendor replay blob for the next Chat Completions request.
    ///
    /// MiniMax stores either `{"reasoning_details":[...]}` (the original
    /// array, replayed verbatim) or `{"reasoning_content":true}` when `text`
    /// itself is `reasoning_content`. Absent on older turns and on providers
    /// that do not use this field. Anthropic signatures stay on `signature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<String>,
}

impl ThinkingBlock {
    /// Creates reasoning text without a signature.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            signature: None,
            replay: None,
        }
    }

    /// Attaches a provider signature.
    #[must_use]
    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }

    /// Attaches a Chat Completions replay blob. An empty blob is ignored.
    #[must_use]
    pub fn with_replay(mut self, replay: impl Into<String>) -> Self {
        let replay = replay.into();
        self.replay = (!replay.is_empty()).then_some(replay);
        self
    }
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Provider-assigned call id (opaque string; matched by
    /// [`ToolResultMessage::tool_call_id`]).
    ///
    /// The value is not a packed encoding. Adapters must not split or parse it
    /// to recover other identifiers.
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// Arguments as raw JSON; validated against the tool's schema at dispatch
    /// time (`mycode-tools`).
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Short target shown next to the tool name: a path, query, or command.
    #[must_use]
    pub fn target(&self) -> String {
        tool_target(&self.name, &self.arguments)
    }

    /// Creates a tool call with an opaque provider-assigned id.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// One-line target for a tool call, taken from the arguments the model sent.
///
/// The UI shows this beside the tool name so a `read` is a path, not a black box.
#[must_use]
pub fn tool_target(name: &str, arguments: &serde_json::Value) -> String {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let joined = match name {
        "read" | "write" | "edit" => text("path"),
        "grep" | "find" => join_target(&text("pattern"), &text("path")),
        "shell" | "powershell" | "bash" | "cmd" => {
            if text("mode") == "program" {
                let args = arguments
                    .get("args")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                join_target(&text("program"), &args)
            } else {
                text("command")
            }
        }
        "web_search" => text("query"),
        "fetch_content" => arguments
            .get("urls")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned(),
        "search_tool" | "use_tool" => text("name"),
        "agent" => join_target(&text("agent"), &text("description")),
        _ => text("path"),
    };
    let flat = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(160).collect()
}

/// Display label: `read  src/main.rs`. The name stays the first token.
#[must_use]
pub fn tool_label(name: &str, target: &str) -> String {
    let target = target.trim();
    if target.is_empty() {
        name.to_owned()
    } else {
        format!("{name}  {target}")
    }
}

fn join_target(left: &str, right: &str) -> String {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => String::new(),
        (false, true) => left.to_owned(),
        (true, false) => right.to_owned(),
        (false, false) => format!("{left}  {right}"),
    }
}

/// The outcome of executing a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultMessage {
    /// Id of the [`ToolCall`] this answers.
    pub tool_call_id: String,
    /// Content visible to the model.
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    /// Structured details for the UI layer only — never enters LLM context
    /// (structured diffs, cwd, …). Splitting `details` from `content` keeps
    /// tokens out of the model loop (pi's ToolResult pattern).
    pub details: Option<serde_json::Value>,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StopReason {
    /// Natural end of turn.
    Stop,
    /// The model wants to call tools.
    ToolUse,
    /// Output was cut off by a length/token limit.
    Length,
    /// Generation ended because of an error.
    Error,
}

/// Visible line kept with whatever thinking and text already arrived when a
/// provider stream fails. Adapters and the agent share this spelling.
#[must_use]
pub fn interrupted_response_text(detail: &str) -> String {
    let mut flat = String::new();
    for ch in detail.chars() {
        if flat.chars().count() >= 240 {
            break;
        }
        if ch.is_control() {
            if !flat.ends_with(' ') {
                flat.push(' ');
            }
            continue;
        }
        flat.push(ch);
    }
    let flat = flat.trim();
    let detail = if flat.is_empty() {
        "the response ended early"
    } else {
        flat
    };
    format!("[error] the response was interrupted: {detail}")
}

/// Token usage reported by a provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens served from the provider cache, when reported.
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    /// Prompt tokens written into the provider cache, when reported.
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    /// Prompt size for the context meter.
    ///
    /// OpenAI-style `prompt_tokens` already includes cache reads. Anthropic
    /// `input_tokens` excludes them, so adapters set this to the sum.
    #[serde(default)]
    pub prompt_tokens: u64,
}

/// Binary content (base64) with its MIME type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryData {
    /// Base64-encoded bytes, as expected by provider image APIs.
    pub data: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime_type: String,
}

#[cfg(test)]
mod tests {
    use super::tool_target;
    use serde_json::json;

    #[test]
    fn shell_and_agent_targets_ignore_retired_names() {
        let script = json!({"mode": "script", "command": "echo hi"});
        assert_eq!(tool_target("shell", &script), "echo hi");
        assert_eq!(tool_target("powershell", &script), "echo hi");
        assert_eq!(tool_target("bash", &script), "echo hi");
        let program = json!({
            "mode": "program",
            "program": "/bin/echo",
            "args": ["ok"]
        });
        assert_eq!(tool_target("shell", &program), "/bin/echo ok");
        let delegated = json!({"agent": "scout", "description": "map the crate"});
        assert_eq!(tool_target("agent", &delegated), "scout map the crate");
        assert_eq!(tool_target("task", &delegated), "");
        assert_eq!(tool_target("exec", &program), "");
    }
}
