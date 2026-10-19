//! Anthropic Messages wire protocol adapter.
//!
//! Covers Anthropic and Anthropic-compatible gateways (Z.AI GLM coding plans,
//! custom relays). Thinking signatures round-trip verbatim, including
//! signature-only blocks whose reasoning text is empty. The request payload
//! follows the model and endpoint: MiniMax stays `adaptive`, every Kimi model
//! uses adaptive effort, and Z.AI does not receive a token budget.

use serde_json::{Value, json};

use mycode_core::{
    AssistantMessage, ContentBlock, Message, StopReason, ThinkingBlock, ToolSpec, Usage,
};
use mycode_core::{ProviderError, ProviderErrorKind, Request, StreamEvent};

use crate::driver::FrameReducer;
use crate::wire_common::{
    MAX_STREAM_INDEX, append_interruption, assembled_stop_reason, charge_stream, merge_usage,
    provider_error_detail, record_provider_stop, usage_from_value,
};

/// Output ceiling used only when models.dev publishes no `limit.output`.
///
/// The Messages API requires `max_tokens`. This is `OUTPUT_TOKEN_MAX` from
/// `sst/opencode` `packages/opencode/src/provider/transform.ts`.
/// `maxOutputTokens` there is
/// `Math.min(model.limit.output, OUTPUT_TOKEN_MAX) || OUTPUT_TOKEN_MAX`.
/// That minimum is not applied: a published `limit.output` (glm's 131072,
/// for example) is sent as-is. The compaction summary request has its own
/// ceiling and does not use this fallback.
const MAX_TOKENS_WHEN_UNPUBLISHED: u64 = 32_000;

/// Converts one provider-neutral request into a Messages body.
#[must_use]
pub(crate) fn build_body(model: &str, endpoint: &str, request: &Request) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        convert_message(model, endpoint, message, &mut messages);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let max_tokens = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .unwrap_or(MAX_TOKENS_WHEN_UNPUBLISHED);
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": true,
    });
    if !request.system_prompt.is_empty() {
        let blocks: Vec<Value> = request
            .system_prompt
            .iter()
            .filter(|part| !part.is_empty())
            .map(|part| json!({"type": "text", "text": part}))
            .collect();
        if !blocks.is_empty() {
            body["system"] = json!(blocks);
        }
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(level) = request.reasoning {
        crate::wire_common::apply_anthropic_thinking(&mut body, model, endpoint, level);
    } else if let Some(token) = request.reasoning_token.as_deref() {
        body["output_config"] = json!({ "effort": token });
    }
    crate::cache::apply_anthropic_message_breakpoints(&mut body);
    body
}

/// Whether a Messages body replays provider thinking in an assistant turn,
/// so the driver attempts [`retry_body_without_thinking`] on a rejection.
#[must_use]
pub(crate) fn replays_thinking(body: &Value) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
                .filter_map(|message| message.get("content").and_then(Value::as_array))
                .flatten()
                .any(is_thinking_block)
        })
}

fn is_thinking_block(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("thinking" | "redacted_thinking")
    )
}

/// Retry body for a 400 that rejects a replayed thinking signature.
///
/// Recent Claude models bind each thinking block to the exact prefix that
/// produced it. A changed system prompt or tool list (an MCP server joined,
/// AGENTS.md was edited) or a compaction that kept a verbatim tail
/// invalidates every older block, and the API answers `Invalid signature in
/// thinking block`. The documented recovery is to drop every thinking block
/// and send once more; text and tool calls stay. `None` when the error is a
/// different 400 or the body carries no thinking.
pub(crate) fn retry_body_without_thinking(
    error: &mycode_core::ProviderError,
    body: &[u8],
) -> Option<Vec<u8>> {
    if error.kind() != mycode_core::ProviderErrorKind::Rejected {
        return None;
    }
    let message = error.message()?.to_ascii_lowercase();
    if !(message.contains("signature") && message.contains("thinking")) {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let messages = value.get_mut("messages")?.as_array_mut()?;
    let mut stripped = false;
    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = blocks.len();
        blocks.retain(|block| !is_thinking_block(block));
        stripped |= blocks.len() != before;
    }
    if !stripped {
        return None;
    }
    // A turn that was only thinking has nothing left to send.
    messages.retain(|message| {
        message
            .get("content")
            .and_then(Value::as_array)
            .is_none_or(|blocks| !blocks.is_empty())
    });
    serde_json::to_vec(&value).ok()
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": crate::wire_common::tool_parameters(tool),
    })
}

fn convert_message(model: &str, endpoint: &str, message: &Message, messages: &mut Vec<Value>) {
    let glm = crate::family::glm_target(model, endpoint);
    match message {
        Message::User(user) => push_user_turn(messages, block_content(&user.content)),
        Message::Assistant(assistant) => {
            let content: Vec<Value> = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(json!({
                        "type": "text",
                        "text": text.text,
                    })),
                    // Signatures replay verbatim; empty thinking text is kept
                    // whenever a signature exists. GLM coding plans also accept
                    // the unsigned thinking text from the previous turn.
                    ContentBlock::Thinking(thinking) => {
                        if let Some(data) = redacted_thinking_data(thinking) {
                            return Some(json!({
                                "type": "redacted_thinking",
                                "data": data,
                            }));
                        }
                        if let Some(signature) = thinking.signature.as_deref() {
                            return Some(json!({
                                "type": "thinking",
                                "thinking": thinking.text,
                                "signature": signature,
                            }));
                        }
                        if glm && !thinking.text.is_empty() {
                            return Some(json!({
                                "type": "thinking",
                                "thinking": thinking.text,
                            }));
                        }
                        None
                    }
                    ContentBlock::ToolCall(call) => Some(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    })),
                    ContentBlock::Image(_) => None,
                })
                .collect();
            if !content.is_empty() {
                messages.push(json!({"role": "assistant", "content": content}));
            }
        }
        Message::ToolResult(result) => {
            let content = crate::wire_common::join_text(&result.content);
            push_user_turn(
                messages,
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": result.tool_call_id,
                    "content": content,
                    "is_error": result.is_error,
                })],
            );
        }
    }
}

/// Appends user content, joining it onto a trailing turn of tool results.
///
/// The parallel results of one assistant turn go back in a single user
/// message, followed by any steer typed while they ran. Anthropic documents
/// one message per batch; a message per result teaches the model to stop
/// calling tools in parallel. Other user turns stay separate, so plain text
/// history keeps the shape (and the cached prefix) it always had.
fn push_user_turn(messages: &mut Vec<Value>, content: Vec<Value>) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut)
        && !blocks.is_empty()
        && blocks
            .iter()
            .all(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
    {
        blocks.extend(content);
        return;
    }
    messages.push(json!({"role": "user", "content": content}));
}

fn block_content(content: &[ContentBlock]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(json!({"type": "text", "text": text.text})),
            ContentBlock::Image(image) => Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type,
                    "data": image.data,
                },
            })),
            _ => None,
        })
        .collect()
}

/// Replay key for an Anthropic `redacted_thinking` block kept on a
/// [`ThinkingBlock`]. Its encrypted `data` has no text or signature, so
/// without this the block was dropped from the next request, and a tool
/// loop with thinking enabled was rejected for not opening with thinking.
const REDACTED_THINKING: &str = "redacted_thinking";

fn redacted_thinking_data(thinking: &ThinkingBlock) -> Option<String> {
    let replay: Value = serde_json::from_str(thinking.replay.as_deref()?).ok()?;
    replay.get(REDACTED_THINKING)?.as_str().map(str::to_owned)
}

/// One content block being assembled from deltas.
#[derive(Default)]
enum BlockAccumulator {
    #[default]
    Empty,
    Thinking {
        text: String,
        signature: Option<String>,
    },
    /// Encrypted reasoning that must go back to the API unchanged.
    RedactedThinking {
        data: String,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
}

/// Accumulates Messages SSE events.
#[derive(Default)]
pub(crate) struct MessagesReducer {
    blocks: Vec<BlockAccumulator>,
    /// Wire `index` → position in `blocks`. Native blocks get their own
    /// slots, so one can never overwrite a call the XML filter added or a
    /// block opened for a delta that arrived without `content_block_start`.
    block_slots: std::collections::HashMap<u64, usize>,
    current: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    prompt_tokens: u64,
    stop_reason: Option<StopReason>,
    /// Detail for [`StopReason::Error`] when the stream fails after bytes.
    interrupt: Option<String>,
    message_stopped: bool,
    terminal_sent: bool,
    /// Bytes retained across text, thinking, and tool-argument fragments.
    accumulated: usize,
    /// Extracts `<tool_call>` markup some endpoints stream as plain text.
    xml: crate::xml_tool_calls::XmlToolCallParser,
    /// Counter for synthetic ids minted by the XML filter.
    xml_calls: usize,
}

impl MessagesReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn begin_interrupt(&mut self, detail: &str) {
        if self.interrupt.is_none() {
            self.interrupt = Some(detail.to_owned());
        }
        self.stop_reason = Some(StopReason::Error);
    }

    fn has_content(&self) -> bool {
        self.blocks.iter().any(|block| match block {
            BlockAccumulator::Empty => false,
            BlockAccumulator::Thinking { text, signature } => {
                !text.is_empty() || signature.is_some()
            }
            BlockAccumulator::RedactedThinking { data } => !data.is_empty(),
            BlockAccumulator::Text { text } => !text.is_empty(),
            BlockAccumulator::ToolUse {
                id,
                name,
                arguments,
            } => !id.is_empty() || !name.is_empty() || !arguments.is_empty(),
        })
    }

    fn assemble(&mut self) -> StreamEvent {
        self.terminal_sent = true;
        // Trailing text still held by the XML filter joins the message.
        for piece in self.xml.finish() {
            if let crate::xml_tool_calls::XmlPiece::Text(text) = piece {
                match self.blocks.get_mut(self.current) {
                    Some(BlockAccumulator::Text { text: block }) => block.push_str(&text),
                    _ if !text.is_empty() => {
                        self.blocks.push(BlockAccumulator::Text { text });
                    }
                    _ => {}
                }
            }
        }
        let mut blocks = Vec::new();
        for block in std::mem::take(&mut self.blocks) {
            match block {
                BlockAccumulator::Thinking { text, signature } => {
                    let mut thinking = ThinkingBlock::new(text);
                    thinking.signature = signature;
                    blocks.push(ContentBlock::Thinking(thinking));
                }
                BlockAccumulator::RedactedThinking { data } => {
                    if !data.is_empty() {
                        let replay = json!({ REDACTED_THINKING: data }).to_string();
                        blocks.push(ContentBlock::Thinking(
                            ThinkingBlock::new("").with_replay(replay),
                        ));
                    }
                }
                BlockAccumulator::Text { text } => {
                    blocks.push(ContentBlock::Text(mycode_core::TextBlock::new(text)));
                }
                BlockAccumulator::ToolUse {
                    id,
                    name,
                    arguments,
                } => {
                    if id.is_empty() || name.is_empty() {
                        continue;
                    }
                    let arguments =
                        serde_json::from_str::<Value>(&arguments).unwrap_or_else(|_| json!({}));
                    blocks.push(ContentBlock::ToolCall(mycode_core::ToolCall::new(
                        id, name, arguments,
                    )));
                }
                BlockAccumulator::Empty => {}
            }
        }
        if let Some(detail) = self.interrupt.as_deref() {
            let note_target = blocks.iter_mut().rev().find_map(|block| match block {
                ContentBlock::Text(text) => Some(&mut text.text),
                _ => None,
            });
            if let Some(text) = note_target {
                append_interruption(text, detail);
            } else {
                let mut text = String::new();
                append_interruption(&mut text, detail);
                blocks.push(ContentBlock::Text(mycode_core::TextBlock::new(text)));
            }
        }
        // XML-filtered calls arrive with an `end_turn` stop reason; any
        // dispatched call set must read as tool use (length stays).
        // An interruption is not tool use.
        let has_calls = blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall(_)));
        let stop_reason =
            assembled_stop_reason(self.interrupt.is_some(), has_calls, self.stop_reason);
        StreamEvent::Done {
            message: AssistantMessage {
                blocks,
                usage: Some(self.snapshot()),
                stop_reason,
            },
        }
    }

    fn snapshot(&self) -> Usage {
        Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            prompt_tokens: self.prompt_tokens,
        }
    }

    fn apply_usage(&mut self, usage: Usage) {
        self.input_tokens = usage.input_tokens;
        self.output_tokens = usage.output_tokens;
        self.cache_read_tokens = usage.cache_read_tokens;
        self.cache_write_tokens = usage.cache_write_tokens;
        self.prompt_tokens = usage.prompt_tokens;
    }
}

impl MessagesReducer {
    fn start_block(&mut self, event: &Value) -> Vec<StreamEvent> {
        let index = event["index"].as_u64().unwrap_or_default();
        if index > MAX_STREAM_INDEX {
            self.terminal_sent = true;
            return vec![crate::driver::protocol_error(
                "content block index exceeds the stream limit",
            )];
        }
        let block = &event["content_block"];
        let accumulator = match block["type"].as_str().unwrap_or_default() {
            "thinking" => BlockAccumulator::Thinking {
                text: block["thinking"].as_str().unwrap_or_default().to_owned(),
                signature: block["signature"].as_str().map(str::to_owned),
            },
            "redacted_thinking" => BlockAccumulator::RedactedThinking {
                data: block["data"].as_str().unwrap_or_default().to_owned(),
            },
            "tool_use" => BlockAccumulator::ToolUse {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
                arguments: String::new(),
            },
            _ => BlockAccumulator::Text {
                text: block["text"].as_str().unwrap_or_default().to_owned(),
            },
        };
        let weight = match &accumulator {
            BlockAccumulator::Thinking { text, signature } => {
                text.len() + signature.as_ref().map_or(0, String::len)
            }
            BlockAccumulator::RedactedThinking { data } => data.len(),
            BlockAccumulator::ToolUse {
                id,
                name,
                arguments,
            } => id.len() + name.len() + arguments.len(),
            BlockAccumulator::Text { text } => text.len(),
            BlockAccumulator::Empty => 0,
        };
        if !charge_stream(&mut self.accumulated, weight) {
            self.terminal_sent = true;
            return vec![crate::driver::protocol_error(
                "stream exceeded the output limit",
            )];
        }
        let position = match self.block_slots.get(&index) {
            Some(&position) => position,
            None => {
                self.blocks.push(BlockAccumulator::Empty);
                let position = self.blocks.len() - 1;
                self.block_slots.insert(index, position);
                position
            }
        };
        self.blocks[position] = accumulator;
        self.current = position;
        Vec::new()
    }

    /// Resolves the slot in `blocks` for `event`'s wire `index`, or returns
    /// [`Self::current`] when the event omits `index`.
    fn slot_for(&self, event: &Value) -> usize {
        match event.get("index").and_then(Value::as_u64) {
            Some(index) => self
                .block_slots
                .get(&index)
                .copied()
                .unwrap_or(self.current),
            None => self.current,
        }
    }

    fn apply_delta(&mut self, event: &Value) -> Vec<StreamEvent> {
        let target = self.slot_for(event);
        let delta = &event["delta"];
        match delta["type"].as_str().unwrap_or_default() {
            "text_delta" => {
                self.apply_text_delta(target, delta["text"].as_str().unwrap_or_default())
            }
            "thinking_delta" => {
                self.apply_thinking_delta(target, delta["thinking"].as_str().unwrap_or_default())
            }
            "signature_delta" => {
                let Some(value) = delta["signature"].as_str() else {
                    return Vec::new();
                };
                if !value.is_empty() && !charge_stream(&mut self.accumulated, value.len()) {
                    self.terminal_sent = true;
                    return vec![crate::driver::protocol_error(
                        "stream exceeded the output limit",
                    )];
                }
                if let Some(BlockAccumulator::Thinking { signature, .. }) =
                    self.blocks.get_mut(target)
                {
                    match signature {
                        Some(existing) => existing.push_str(value),
                        None => *signature = Some(value.to_owned()),
                    }
                }
                Vec::new()
            }
            "input_json_delta" => {
                self.apply_input_json(target, delta["partial_json"].as_str().unwrap_or_default())
            }
            _ => Vec::new(),
        }
    }

    fn apply_text_delta(&mut self, mut target: usize, part: &str) -> Vec<StreamEvent> {
        if part.is_empty() {
            return Vec::new();
        }
        if !charge_stream(&mut self.accumulated, part.len()) {
            self.terminal_sent = true;
            return vec![crate::driver::protocol_error(
                "stream exceeded the output limit",
            )];
        }
        let mut events = Vec::new();
        for piece in self.xml.feed(part) {
            match piece {
                crate::xml_tool_calls::XmlPiece::Text(text) => {
                    // Some gateways emit text deltas without a preceding
                    // content_block_start; create the block the way the
                    // thinking path does, so the assembled Done message
                    // keeps the text the deltas already showed.
                    if !matches!(self.blocks.get(target), Some(BlockAccumulator::Text { .. })) {
                        self.blocks.push(BlockAccumulator::Text {
                            text: String::new(),
                        });
                        target = self.blocks.len() - 1;
                        self.current = target;
                    }
                    if let BlockAccumulator::Text { text: block } = &mut self.blocks[target] {
                        block.push_str(&text);
                    }
                    events.push(StreamEvent::TextDelta(text));
                }
                crate::xml_tool_calls::XmlPiece::ToolCall { name, arguments } => {
                    self.xml_calls += 1;
                    let id = format!("toolu-xml-{}", self.xml_calls);
                    events.push(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        partial_json: arguments.clone(),
                    });
                    self.blocks.push(BlockAccumulator::ToolUse {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }
        events
    }

    fn apply_thinking_delta(&mut self, mut target: usize, part: &str) -> Vec<StreamEvent> {
        if part.is_empty() {
            return Vec::new();
        }
        if !charge_stream(&mut self.accumulated, part.len()) {
            self.terminal_sent = true;
            return vec![crate::driver::protocol_error(
                "stream exceeded the output limit",
            )];
        }
        if !matches!(
            self.blocks.get(target),
            Some(BlockAccumulator::Thinking { .. })
        ) {
            self.blocks.push(BlockAccumulator::Thinking {
                text: String::new(),
                signature: None,
            });
            target = self.blocks.len() - 1;
            self.current = target;
        }
        if let BlockAccumulator::Thinking { text, .. } = &mut self.blocks[target] {
            text.push_str(part);
            return vec![StreamEvent::ThinkingDelta(part.to_owned())];
        }
        Vec::new()
    }

    fn apply_input_json(&mut self, target: usize, part: &str) -> Vec<StreamEvent> {
        if !part.is_empty() && !charge_stream(&mut self.accumulated, part.len()) {
            self.terminal_sent = true;
            return vec![crate::driver::protocol_error(
                "stream exceeded the output limit",
            )];
        }
        let Some(BlockAccumulator::ToolUse { id, arguments, .. }) = self.blocks.get_mut(target)
        else {
            return Vec::new();
        };
        let id = (!part.is_empty()).then(|| id.clone());
        arguments.push_str(part);
        id.map(|id| {
            vec![StreamEvent::ToolCallDelta {
                id,
                partial_json: part.to_owned(),
            }]
        })
        .unwrap_or_default()
    }

    fn apply_message_delta(&mut self, event: &Value) {
        if let Some(stop) = event["delta"]["stop_reason"].as_str() {
            record_provider_stop(&mut self.stop_reason, &mut self.interrupt, stop);
        }
        if event.get("usage").is_some() {
            let parsed = usage_from_value(&event["usage"]);
            let merged = merge_usage(Some(self.snapshot()), parsed);
            self.apply_usage(merged);
        }
    }
}

/// Maps an output-free `error` frame onto the HTTP status the same failure
/// carries as a refused request, so the shared retry rules apply to it.
fn error_frame_failure(event: &Value, detail: &str) -> ProviderError {
    let (kind, status) = match event["error"]["type"].as_str() {
        Some("overloaded_error") => (ProviderErrorKind::Unavailable, Some(529)),
        Some("rate_limit_error") => (ProviderErrorKind::Unavailable, Some(429)),
        Some("api_error") => (ProviderErrorKind::Unavailable, Some(500)),
        _ => (ProviderErrorKind::Rejected, None),
    };
    let error = ProviderError::with_message(kind, detail);
    match status {
        Some(status) => error.with_status(status),
        None => error,
    }
}

impl FrameReducer for MessagesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent || data.trim().is_empty() {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            if self.has_content() {
                self.begin_interrupt("invalid messages frame");
                return vec![self.assemble()];
            }
            return vec![crate::driver::protocol_error("invalid messages frame")];
        };
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let parsed = usage_from_value(&event["message"]["usage"]);
                let merged = merge_usage(Some(self.snapshot()), parsed);
                self.apply_usage(merged);
                Vec::new()
            }
            "content_block_start" => self.start_block(&event),
            "content_block_delta" => self.apply_delta(&event),
            "message_delta" => {
                self.apply_message_delta(&event);
                Vec::new()
            }
            "message_stop" => {
                self.message_stopped = true;
                vec![self.assemble()]
            }
            "error" => {
                let detail = provider_error_detail(&event).unwrap_or_else(|| {
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("provider error frame")
                        .to_owned()
                });
                if !self.has_content() {
                    // Nothing to keep: report the frame as the request failure
                    // it is, so the driver can retry an overload or rate limit.
                    self.terminal_sent = true;
                    return vec![StreamEvent::Error(error_frame_failure(&event, &detail))];
                }
                self.begin_interrupt(&detail);
                vec![self.assemble()]
            }
            _ => Vec::new(),
        }
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("messages stream ended after terminal");
        }
        if self.message_stopped || self.stop_reason.is_some() {
            return self.assemble();
        }
        if self.has_content() {
            self.begin_interrupt("messages stream ended before message_stop");
            return self.assemble();
        }
        crate::driver::protocol_error("messages stream ended before message_stop")
    }

    fn interrupt(&mut self, detail: &str) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("messages stream ended after terminal");
        }
        self.begin_interrupt(detail);
        self.assemble()
    }
}

#[cfg(test)]
mod tests;
