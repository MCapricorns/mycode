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
use mycode_core::{Request, StreamEvent};

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

/// Whether a serialized Messages body replays provider thinking, so the
/// driver keeps a copy for [`retry_body_without_thinking`].
#[must_use]
pub(crate) fn body_replays_thinking(body: &[u8]) -> bool {
    let has = |needle: &[u8]| body.windows(needle.len()).any(|window| window == needle);
    has(br#""type":"redacted_thinking""#) || has(br#""type":"thinking""#)
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
        blocks.retain(|block| {
            !matches!(
                block.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking")
            )
        });
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
mod tests {
    use mycode_core::{
        AssistantMessage, ContentBlock, Message, Request, StopReason, ThinkingBlock,
    };

    use super::MessagesReducer;
    use crate::anthropic_messages::build_body;
    use crate::driver::FrameReducer;

    fn thinking_of(message: &AssistantMessage) -> String {
        message
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Thinking(thinking) => Some(thinking.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn eof_before_message_stop_keeps_thinking() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"still here"}}"#,
        );
        let mycode_core::StreamEvent::Done { message } = reducer.finish() else {
            panic!("done");
        };
        assert_eq!(thinking_of(&message), "still here");
        assert!(message.text().contains("message_stop"));
        assert_eq!(message.stop_reason, StopReason::Error);
    }

    fn done(events: Vec<mycode_core::StreamEvent>) -> AssistantMessage {
        match events.into_iter().last() {
            Some(mycode_core::StreamEvent::Done { message }) => message,
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[test]
    fn redacted_thinking_replays_its_encrypted_data() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"ENCRYPTED"}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        );
        reducer.feed(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#);
        let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        let body = build_body(
            "claude-sonnet-5-5",
            "https://api.anthropic.com/v1/messages",
            &Request {
                messages: vec![std::sync::Arc::new(Message::Assistant(message))],
                ..Request::default()
            },
        );
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "redacted_thinking");
        assert_eq!(content[0]["data"], "ENCRYPTED");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn a_native_block_never_overwrites_an_xml_tool_call() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        );
        let xml = concat!(
            "<minimax:tool_call>",
            "<invoke name=\"find\">",
            "<parameter name=\"pattern\">*.rs</parameter>",
            "</invoke>",
            "</minimax:tool_call>",
        );
        let delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": xml},
        });
        reducer.feed(&delta.to_string());
        // The XML call took position 1; the native block at wire index 1
        // must get its own slot.
        reducer.feed(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_native","name":"read","input":{}}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        );
        let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
        let names: Vec<_> = message
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["find", "read"]);
    }

    #[test]
    fn content_block_deltas_route_by_wire_index_and_join_signature_deltas() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_a","name":"read","input":{}}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_b","name":"grep","input":{}}}"#,
        );
        // Deltas for block 1 and block 0 arriving after block 2 started must
        // still reach their own blocks instead of landing on `self.current`.
        reducer.feed(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.rs\"}"}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"x\"}"}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-"}}"#,
        );
        reducer.feed(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"part2"}}"#,
        );
        reducer.feed(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#);
        let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
        assert_eq!(thinking_of(&message), "plan");
        let signature = message.blocks.iter().find_map(|block| match block {
            ContentBlock::Thinking(thinking) => thinking.signature.as_deref(),
            _ => None,
        });
        assert_eq!(signature, Some("sig-part2"));
        let calls: Vec<_> = message
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => {
                    Some((call.id.as_str(), call.name.as_str(), &call.arguments))
                }
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "toolu_a");
        assert_eq!(calls[0].2["path"], "a.rs");
        assert_eq!(calls[1].0, "toolu_b");
        assert_eq!(calls[1].2["pattern"], "x");
    }

    #[test]
    fn error_event_keeps_prior_thinking() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(
            r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"partial"}}"#,
        );
        let events = reducer.feed(r#"{"type":"error","error":{"message":"overloaded"}}"#);
        let mycode_core::StreamEvent::Done { message } = events.into_iter().next().unwrap() else {
            panic!("done");
        };
        assert_eq!(thinking_of(&message), "partial");
        assert!(message.text().contains("overloaded"));
    }

    #[test]
    fn parallel_tool_results_and_a_steer_share_one_user_turn() {
        use mycode_core::{ToolCall, ToolResultMessage, UserMessage};
        let result = |id: &str| {
            std::sync::Arc::new(Message::ToolResult(ToolResultMessage {
                tool_call_id: id.to_owned(),
                content: vec![ContentBlock::Text(mycode_core::TextBlock::new("ok"))],
                is_error: false,
                details: None,
            }))
        };
        let request = Request {
            messages: vec![
                std::sync::Arc::new(Message::User(UserMessage::text("go"))),
                std::sync::Arc::new(Message::Assistant(AssistantMessage {
                    blocks: vec![
                        ContentBlock::ToolCall(ToolCall::new("a", "read", serde_json::json!({}))),
                        ContentBlock::ToolCall(ToolCall::new("b", "grep", serde_json::json!({}))),
                    ],
                    usage: None,
                    stop_reason: StopReason::ToolUse,
                })),
                result("a"),
                result("b"),
                std::sync::Arc::new(Message::User(UserMessage::text("also check tests"))),
            ],
            ..Request::default()
        };
        let body = build_body(
            "kimi-for-coding",
            "https://api.kimi.com/coding/v1/messages",
            &request,
        );
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 3, "{body}");
        let content = messages[2]["content"].as_array().expect("content");
        let kinds: Vec<_> = content
            .iter()
            .map(|block| block["type"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(kinds, ["tool_result", "tool_result", "text"]);
        assert_eq!(content[0]["tool_use_id"], "a");
        assert_eq!(content[1]["tool_use_id"], "b");
    }

    #[test]
    fn a_tool_result_never_joins_a_text_turn() {
        use mycode_core::{ToolResultMessage, UserMessage};
        let request = Request {
            messages: vec![
                std::sync::Arc::new(Message::User(UserMessage::text("summary"))),
                std::sync::Arc::new(Message::ToolResult(ToolResultMessage {
                    tool_call_id: "orphan".to_owned(),
                    content: Vec::new(),
                    is_error: false,
                    details: None,
                })),
            ],
            ..Request::default()
        };
        let body = build_body(
            "glm-5.3",
            "https://api.z.ai/api/anthropic/v1/messages",
            &request,
        );
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn a_rejected_signature_retries_without_any_thinking() {
        use mycode_core::{ProviderError, ProviderErrorKind};
        let body = serde_json::json!({
            "model": "claude-opus-5-5",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "go"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "", "signature": "sig"},
                    {"type": "tool_use", "id": "a", "name": "read", "input": {}}
                ]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "ok"}]},
                {"role": "assistant", "content": [{"type": "redacted_thinking", "data": "x"}]},
                {"role": "user", "content": [{"type": "text", "text": "next"}]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(super::body_replays_thinking(&bytes));
        let rejected = ProviderError::with_message(
            ProviderErrorKind::Rejected,
            "HTTP 400 Bad Request: messages.1.content.0: Invalid `signature` in `thinking` block. \
             The block is bound to a different conversation.",
        );
        let retried = super::retry_body_without_thinking(&rejected, &bytes).expect("retry");
        let retried: serde_json::Value = serde_json::from_slice(&retried).unwrap();
        let messages = retried["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4, "the thinking-only turn is dropped");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert!(
            !serde_json::to_string(&retried)
                .unwrap()
                .contains("thinking")
        );

        let other = ProviderError::with_message(ProviderErrorKind::Rejected, "HTTP 400: bad tool");
        assert!(super::retry_body_without_thinking(&other, &bytes).is_none());
        let plain = serde_json::to_vec(&serde_json::json!({"messages": []})).unwrap();
        assert!(!super::body_replays_thinking(&plain));
        assert!(super::retry_body_without_thinking(&rejected, &plain).is_none());
    }

    #[test]
    fn refusal_ends_the_message_as_a_visible_error() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#);
        let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
        assert_eq!(message.stop_reason, StopReason::Error);
        assert!(message.text().contains("refusal"), "{}", message.text());

        let mut reducer = MessagesReducer::new();
        reducer.feed(
            r#"{"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded"}}"#,
        );
        let message = done(reducer.feed(r#"{"type":"message_stop"}"#));
        assert_eq!(message.stop_reason, StopReason::Length);
    }

    #[test]
    fn glm_replays_unsigned_thinking() {
        let request = Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![ContentBlock::Thinking(ThinkingBlock::new("unsigned"))],
                usage: None,
                stop_reason: StopReason::Stop,
            }))],
            ..Request::default()
        };
        let glm = build_body(
            "glm-4.7",
            "https://open.bigmodel.cn/api/anthropic/v1/messages",
            &request,
        );
        assert_eq!(glm["messages"][0]["content"][0]["thinking"], "unsigned");
        let claude = build_body(
            "claude-sonnet-4-6",
            "https://api.anthropic.com/v1/messages",
            &request,
        );
        assert!(claude["messages"].as_array().unwrap().is_empty());
    }
}
