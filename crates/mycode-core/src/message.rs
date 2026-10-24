//! Message model: the shared conversation values exchanged by the user, the
//! model, and tools.
//!
//! Serde uses the default externally-tagged representation. Wire encoding
//! belongs to `mycode-providers`; durable session encoding belongs to
//! `mycode-agent`. See `wiki/index.html`.

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
    /// Vendor replay blob for the next request on the same wire.
    ///
    /// MiniMax stores either `{"reasoning_details":[...]}` (the original
    /// array, replayed verbatim) or `{"reasoning_content":true}` when `text`
    /// itself is `reasoning_content`. Anthropic stores
    /// `{"redacted_thinking":"<data>"}` for an encrypted block that has no
    /// text. Absent on older turns and on providers that do not use this
    /// field. Anthropic signatures stay on `signature`.
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

    /// Attaches a vendor replay blob (Chat Completions reasoning details,
    /// Anthropic redacted thinking). An empty blob is ignored.
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
        "read" => {
            let path = text("path");
            let offset = arguments.get("offset").and_then(serde_json::Value::as_u64);
            let limit = arguments.get("limit").and_then(serde_json::Value::as_u64);
            match (offset, limit) {
                (Some(start), Some(limit)) => format!("{path} L{start}+{limit}"),
                (Some(start), None) => format!("{path} L{start}"),
                (None, Some(limit)) => format!("{path} +{limit}"),
                (None, None) => path,
            }
        }
        "write" | "edit" => text("path"),
        "grep" | "find" => join_target(&text("pattern"), &text("path")),
        "shell" | "powershell" | "bash" | "zsh" | "sh" | "cmd" => {
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
        // Retired tools. The arms stay so old session ledgers replay with the
        // description the model originally sent.
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
        // The retired `run_code` carried a `description`; the current one
        // carries only a `script`, so a description-only target left every
        // code-mode row a bare `run_code` with no hint of what it ran. Fall
        // back to the tools the script calls, in first-call order.
        "run_code" => {
            let description = text("description");
            if !description.is_empty() {
                description
            } else {
                script_call_summary(&text("script"))
            }
        }
        "wait_code" => text("cell_id"),
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

/// The distinct tools a code-mode script calls, in first-call order
/// (`read · grep · edit`), read from its `call("name", …)` literals.
///
/// A script whose calls are all computed (no literal name) falls back to
/// its first meaningful line, so the row still names something the script
/// does instead of nothing.
fn script_call_summary(script: &str) -> String {
    let mut names: Vec<&str> = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find("call") {
        // Not a host call when `call` is the tail of a longer name
        // (`recall`, `callback`).
        let embedded = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
        let after = &rest[at + 4..];
        rest = after;
        if embedded {
            continue;
        }
        let Some(after) = after.trim_start().strip_prefix('(') else {
            continue;
        };
        let after = after.trim_start();
        let quote = match after.chars().next() {
            Some(quote @ ('"' | '\'')) => quote,
            _ => continue,
        };
        let Some((name, _)) = after[1..].split_once(quote) else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    if !names.is_empty() {
        return names.join(" · ");
    }
    script
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .unwrap_or("")
        .to_owned()
}

fn join_target(left: &str, right: &str) -> String {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => String::new(),
        (false, true) => left.to_owned(),
        (true, false) => right.to_owned(),
        // The caller flattens whitespace runs, so a single space is what the
        // UI sees.
        (false, false) => format!("{left} {right}"),
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

/// Detail spelling committed when the user stops a turn. Frontends show
/// their own cancel banner, so display projections strip the line this
/// detail produces while model-facing history keeps it.
pub const USER_INTERRUPT_DETAIL: &str = "interrupted by user";

/// Start of every [`interrupted_response_text`] line. Frontends match the
/// whole prefix, not a bare `[error]`, so build output the model quotes
/// (sbt prints `[error] …`) stays answer text.
pub const INTERRUPTED_RESPONSE_PREFIX: &str = "[error] the response was interrupted:";

/// Visible line kept with whatever thinking and text already arrived when a
/// provider stream fails. Adapters and the agent share this spelling.
#[must_use]
pub fn interrupted_response_text(detail: &str) -> String {
    const MAX_DETAIL_CHARS: usize = 240;
    let mut flat = String::new();
    let mut kept = 0usize;
    for ch in detail.chars() {
        if kept >= MAX_DETAIL_CHARS {
            break;
        }
        if ch.is_control() {
            if !flat.ends_with(' ') {
                flat.push(' ');
                kept += 1;
            }
            continue;
        }
        flat.push(ch);
        kept += 1;
    }
    let flat = flat.trim();
    let detail = if flat.is_empty() {
        "the response ended early"
    } else {
        flat
    };
    format!("{INTERRUPTED_RESPONSE_PREFIX} {detail}")
}

/// Removes a trailing [`interrupted_response_text`] line written with
/// [`USER_INTERRUPT_DETAIL`]. The note stays in the committed message —
/// replayed context tells the model its reply was cut — but a transcript
/// row duplicates the frontends' cancel banner. Other interruption
/// details stay visible: they are the only record of a real failure.
pub fn strip_user_interruption(text: &mut String) {
    let note = interrupted_response_text(USER_INTERRUPT_DETAIL);
    if !text.ends_with(note.as_str()) {
        return;
    }
    text.truncate(text.len() - note.len());
    text.truncate(text.trim_end().len());
    // A reply that was only the marker leaves whitespace, not an empty row.
    if text.trim().is_empty() {
        text.clear();
    }
}

/// Result text for a call the history never answered.
const UNANSWERED_CALL_TEXT: &str = "tool call has no recorded result: the turn ended before \
    it finished (an interrupt, a crash, or the app closing). Its effects, if any, are unknown; \
    check before relying on them.";

/// Makes the tool-call pairing of `messages` valid for every wire.
///
/// Providers reject an assistant `tool_call` that no result answers, and a
/// result whose call is not on the assistant message right before it. A
/// replayed ledger can hold either shape after a crash, a closed window, or
/// a turn that stopped recording mid-tool; without this every later request
/// in the session fails. Each run of results after an assistant message
/// keeps one result per call, in the run's order; missing calls get an
/// error result appended in call order; stray results are dropped.
///
/// The edit is deterministic, so repeated requests send identical bytes and
/// the provider prefix cache holds. Returns whether anything changed.
pub fn pair_tool_results(messages: &mut Vec<SharedMessage>) -> bool {
    if tool_pairing_is_valid(messages) {
        return false;
    }
    let mut paired = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        index += 1;
        let calls: Vec<&str> = match message.as_ref() {
            Message::Assistant(assistant) => assistant_call_ids(assistant).collect(),
            // A result that follows no assistant call list is stray.
            Message::ToolResult(_) => continue,
            Message::User(_) => Vec::new(),
        };
        paired.push(Arc::clone(message));
        if calls.is_empty() {
            continue;
        }
        let mut answered: Vec<&str> = Vec::with_capacity(calls.len());
        while let Some(next) = messages.get(index) {
            let Message::ToolResult(result) = next.as_ref() else {
                break;
            };
            index += 1;
            let id = result.tool_call_id.as_str();
            if calls.contains(&id) && !answered.contains(&id) {
                answered.push(id);
                paired.push(Arc::clone(next));
            }
        }
        for id in calls {
            if !answered.contains(&id) {
                paired.push(Arc::new(Message::ToolResult(ToolResultMessage {
                    tool_call_id: id.to_owned(),
                    content: vec![ContentBlock::Text(TextBlock::new(UNANSWERED_CALL_TEXT))],
                    is_error: true,
                    details: None,
                })));
            }
        }
    }
    *messages = paired;
    true
}

fn assistant_call_ids(assistant: &AssistantMessage) -> impl Iterator<Item = &str> {
    assistant.blocks.iter().filter_map(|block| match block {
        ContentBlock::ToolCall(call) => Some(call.id.as_str()),
        _ => None,
    })
}

/// The cheap check: every call answered exactly once right after its
/// assistant message, and no result anywhere else.
fn tool_pairing_is_valid(messages: &[SharedMessage]) -> bool {
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        index += 1;
        let calls: Vec<&str> = match message.as_ref() {
            Message::Assistant(assistant) => assistant_call_ids(assistant).collect(),
            Message::ToolResult(_) => return false,
            Message::User(_) => continue,
        };
        let mut answered = 0usize;
        while let Some(next) = messages.get(index) {
            let Message::ToolResult(result) = next.as_ref() else {
                break;
            };
            index += 1;
            let id = result.tool_call_id.as_str();
            let duplicate = messages[index - answered - 1..index - 1]
                .iter()
                .any(|earlier| matches!(earlier.as_ref(), Message::ToolResult(seen) if seen.tool_call_id == id));
            if !calls.contains(&id) || duplicate {
                return false;
            }
            answered += 1;
        }
        if answered != calls.len() {
            return false;
        }
    }
    true
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
    use super::{
        USER_INTERRUPT_DETAIL, interrupted_response_text, strip_user_interruption, tool_target,
    };
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

    #[test]
    fn tool_pairing_answers_missing_calls_and_drops_strays() {
        use super::{
            AssistantMessage, ContentBlock, Message, StopReason, ToolCall, ToolResultMessage,
            UserMessage, pair_tool_results,
        };
        use std::sync::Arc;

        let assistant = |ids: &[&str]| {
            Arc::new(Message::Assistant(AssistantMessage {
                blocks: ids
                    .iter()
                    .map(|id| ContentBlock::ToolCall(ToolCall::new(*id, "read", json!({}))))
                    .collect(),
                usage: None,
                stop_reason: StopReason::ToolUse,
            }))
        };
        let result = |id: &str| {
            Arc::new(Message::ToolResult(ToolResultMessage {
                tool_call_id: id.to_owned(),
                content: Vec::new(),
                is_error: false,
                details: None,
            }))
        };
        let user = |text: &str| Arc::new(Message::User(UserMessage::text(text)));
        let shape = |messages: &[Arc<Message>]| -> Vec<String> {
            messages
                .iter()
                .map(|message| match message.as_ref() {
                    Message::User(_) => "user".to_owned(),
                    Message::Assistant(_) => "assistant".to_owned(),
                    Message::ToolResult(result) => format!(
                        "{}{}",
                        result.tool_call_id,
                        if result.is_error { "!" } else { "" }
                    ),
                })
                .collect()
        };

        // Valid histories are left alone.
        let mut valid = vec![user("go"), assistant(&["a", "b"]), result("b"), result("a")];
        assert!(!pair_tool_results(&mut valid));
        assert_eq!(shape(&valid), ["user", "assistant", "b", "a"]);

        // A crash after the assistant step: `b` never got a result, a stray
        // result trails a user message, and `a` was recorded twice.
        let mut broken = vec![
            user("go"),
            assistant(&["a", "b"]),
            result("a"),
            result("a"),
            user("next"),
            result("zz"),
            assistant(&["c"]),
        ];
        assert!(pair_tool_results(&mut broken));
        assert_eq!(
            shape(&broken),
            ["user", "assistant", "a", "b!", "user", "assistant", "c!"]
        );
        // The repair is stable: a second pass changes nothing.
        assert!(!pair_tool_results(&mut broken));
    }

    #[test]
    fn retired_run_code_replays_from_its_description() {
        let args = json!({"code": "print(1)", "description": "plot the data"});
        assert_eq!(tool_target("run_code", &args), "plot the data");
    }

    #[test]
    fn run_code_target_names_the_tools_its_script_calls() {
        // The current `run_code` arguments carry no `description`, so the
        // row target comes from the script's `call` literals: distinct
        // names in first-call order.
        let args = json!({"script": "let a = call(\"read\", #{ path: \"x\" });\ncall(\"grep\", #{});\ncall(\"read\", #{});\ncomplete(a)"});
        assert_eq!(tool_target("run_code", &args), "read · grep");
        // A script with no literal call still names its first line, and a
        // longer name ending in `call` is not a host call.
        let args = json!({"script": "// setup\nlet total = 1 + 2;"});
        assert_eq!(tool_target("run_code", &args), "let total = 1 + 2;");
        let args = json!({"script": "recall(\"read\", #{});"});
        assert_eq!(tool_target("run_code", &args), "recall(\"read\", #{});");
    }

    #[test]
    fn wait_code_target_is_its_cell_id() {
        let args = json!({"cell_id": "cell-3"});
        assert_eq!(tool_target("wait_code", &args), "cell-3");
    }

    #[test]
    fn interruption_note_flattens_controls_and_caps_length() {
        assert_eq!(
            interrupted_response_text("plain detail"),
            "[error] the response was interrupted: plain detail"
        );
        // Control characters collapse into one space, and an all-control
        // detail falls back to the generic line.
        assert_eq!(
            interrupted_response_text("a\r\n\r\n\tb"),
            "[error] the response was interrupted: a b"
        );
        assert_eq!(
            interrupted_response_text("\r\n\t"),
            "[error] the response was interrupted: the response ended early"
        );
        // The budget counts characters, not bytes, so multibyte text keeps
        // its full width.
        let wide = "汉".repeat(300);
        let note = interrupted_response_text(&wide);
        let expected = format!("[error] the response was interrupted: {}", "汉".repeat(240));
        assert_eq!(note, expected);
    }

    #[test]
    fn strip_user_interruption_drops_only_the_cancel_line() {
        let note = interrupted_response_text(USER_INTERRUPT_DETAIL);
        // A bare marker — a cancel before any reply streamed — clears.
        let mut bare = note.clone();
        strip_user_interruption(&mut bare);
        assert!(bare.is_empty());
        // A partial reply keeps its text; the appended line goes.
        let mut partial = format!("half an answer\n{note}");
        strip_user_interruption(&mut partial);
        assert_eq!(partial, "half an answer");
        // Real failure details stay: they carry the only error record.
        let mut failure = interrupted_response_text("the response ended early");
        strip_user_interruption(&mut failure);
        assert_eq!(
            failure,
            "[error] the response was interrupted: the response ended early"
        );
        // A marker mid-text (not the tail) is the model's own output.
        let mut inline = format!("{note}\nmore after");
        strip_user_interruption(&mut inline);
        assert_eq!(inline, format!("{note}\nmore after"));
    }
}
