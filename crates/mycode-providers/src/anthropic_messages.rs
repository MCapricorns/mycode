//! Anthropic Messages wire protocol adapter.
//!
//! Covers Anthropic and Anthropic-compatible gateways (Z.AI GLM coding plans,
//! custom relays). Thinking signatures round-trip verbatim, including
//! signature-only blocks whose reasoning text is empty; the adapter never
//! enables thinking explicitly, so default-thinking models keep their own
//! configuration.

use serde_json::{Value, json};

use mycode_core::{
    AssistantMessage, ContentBlock, Message, StopReason, ThinkingBlock, ToolSpec, Usage,
};
use mycode_core::{ProviderError, ProviderErrorKind, ReasoningLevel, Request, StreamEvent};

use crate::driver::FrameReducer;
use crate::wire_common::{
    MAX_STREAM_INDEX, charge_stream, map_stop_reason, merge_usage, usage_from_value,
};

/// Output ceiling sent with every request; the Messages API requires it.
pub const MAX_TOKENS_DEFAULT: u64 = 4096;

/// Converts one provider-neutral request into a Messages body.
#[must_use]
pub(crate) fn build_body(model: &str, endpoint: &str, request: &Request) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        convert_message(message, &mut messages);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS_DEFAULT,
        "messages": messages,
        "stream": true,
    });
    if !request.system_prompt.is_empty() {
        body["system"] = json!(request.system_prompt.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(level) = request.reasoning {
        match level {
            ReasoningLevel::Off => {
                body["thinking"] = json!({ "type": "disabled" });
            }
            // MiniMax's Anthropic-compatible API uses the same on-value as
            // its OpenAI body: `adaptive`, not `enabled`.
            ReasoningLevel::On if crate::wire_common::minimax_target(model, endpoint) => {
                body["thinking"] = json!({ "type": "adaptive" });
            }
            level => {
                // Thinking budget must stay below max_tokens; raise the cap
                // so the budget always fits. Rungs follow models.dev effort
                // tokens rather than a hardcoded three-step list.
                let budget = match level {
                    ReasoningLevel::Minimal | ReasoningLevel::Low => 1_024,
                    ReasoningLevel::On | ReasoningLevel::Medium => 4_096,
                    ReasoningLevel::High => 16_384,
                    ReasoningLevel::Xhigh | ReasoningLevel::Max => 32_768,
                    ReasoningLevel::Off => 0,
                };
                let max_tokens = body["max_tokens"].as_u64().unwrap_or(MAX_TOKENS_DEFAULT);
                if max_tokens <= budget {
                    body["max_tokens"] = json!(budget + MAX_TOKENS_DEFAULT);
                }
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            }
        }
    }
    body
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.params_schema,
    })
}

fn convert_message(message: &Message, messages: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            messages.push(json!({"role": "user", "content": block_content(&user.content)}));
        }
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
                    // whenever a signature exists.
                    ContentBlock::Thinking(thinking) => {
                        let signature = thinking.signature.as_deref()?;
                        Some(json!({
                            "type": "thinking",
                            "thinking": thinking.text,
                            "signature": signature,
                        }))
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
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": result.tool_call_id,
                    "content": content,
                    "is_error": result.is_error,
                }],
            }));
        }
        Message::Custom(_) => {}
    }
}

fn block_content(content: &[ContentBlock]) -> Value {
    let parts: Vec<Value> = content
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
        .collect();
    json!(parts)
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
    current: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    stop_reason: Option<StopReason>,
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

    fn current_id(&self) -> Option<String> {
        match self.blocks.get(self.current)? {
            BlockAccumulator::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        }
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
        for block in &self.blocks {
            match block {
                BlockAccumulator::Thinking { text, signature } => {
                    let mut thinking = ThinkingBlock::new(text.clone());
                    thinking.signature = signature.clone();
                    blocks.push(ContentBlock::Thinking(thinking));
                }
                BlockAccumulator::Text { text } => {
                    blocks.push(ContentBlock::Text(mycode_core::TextBlock::new(
                        text.clone(),
                    )));
                }
                BlockAccumulator::ToolUse {
                    id,
                    name,
                    arguments,
                } => {
                    let arguments =
                        serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
                    blocks.push(ContentBlock::ToolCall(mycode_core::ToolCall::new(
                        id.clone(),
                        name.clone(),
                        arguments,
                    )));
                }
                BlockAccumulator::Empty => {}
            }
        }
        // XML-filtered calls arrive with an `end_turn` stop reason; any
        // dispatched call set must read as tool use (length stays).
        let has_calls = self
            .blocks
            .iter()
            .any(|block| matches!(block, BlockAccumulator::ToolUse { .. }));
        let stop_reason = if has_calls && self.stop_reason != Some(StopReason::Length) {
            StopReason::ToolUse
        } else {
            self.stop_reason.unwrap_or(StopReason::Stop)
        };
        StreamEvent::Done {
            message: AssistantMessage {
                blocks,
                usage: Some(Usage {
                    input_tokens: self.input_tokens,
                    output_tokens: self.output_tokens,
                    cache_read_tokens: self.cache_read_tokens,
                }),
                stop_reason,
            },
        }
    }
}

impl FrameReducer for MessagesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return vec![crate::driver::protocol_error("invalid messages frame")];
        };
        let event_type = event["type"].as_str().unwrap_or_default();
        match event_type {
            "message_start" => {
                let parsed = usage_from_value(&event["message"]["usage"]);
                let merged = merge_usage(
                    Some(Usage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                        cache_read_tokens: self.cache_read_tokens,
                    }),
                    parsed,
                );
                self.input_tokens = merged.input_tokens;
                self.output_tokens = merged.output_tokens;
                self.cache_read_tokens = merged.cache_read_tokens;
            }
            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or_default();
                if index > MAX_STREAM_INDEX {
                    self.terminal_sent = true;
                    return vec![crate::driver::protocol_error(
                        "content block index exceeds the stream limit",
                    )];
                }
                let block = &event["content_block"];
                let accumulator = match block["type"].as_str().unwrap_or_default() {
                    "thinking" | "redacted_thinking" => BlockAccumulator::Thinking {
                        text: block["thinking"].as_str().unwrap_or_default().to_owned(),
                        signature: block["signature"].as_str().map(str::to_owned),
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
                let index = index as usize;
                while self.blocks.len() <= index {
                    self.blocks.push(BlockAccumulator::Empty);
                }
                self.blocks[index] = accumulator;
                self.current = index;
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let part = delta["text"].as_str().unwrap_or_default();
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
                                    if let BlockAccumulator::Text { text: block } = self
                                        .blocks
                                        .get_mut(self.current)
                                        .unwrap_or(&mut BlockAccumulator::Empty)
                                    {
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
                        return events;
                    }
                    "thinking_delta" => {
                        let part = delta["thinking"].as_str().unwrap_or_default();
                        if !part.is_empty() {
                            if !charge_stream(&mut self.accumulated, part.len()) {
                                self.terminal_sent = true;
                                return vec![crate::driver::protocol_error(
                                    "stream exceeded the output limit",
                                )];
                            }
                            if let BlockAccumulator::Thinking { text, .. } = self
                                .blocks
                                .get_mut(self.current)
                                .unwrap_or(&mut BlockAccumulator::Empty)
                            {
                                text.push_str(part);
                                return vec![StreamEvent::ThinkingDelta(part.to_owned())];
                            }
                        }
                    }
                    "signature_delta" => {
                        if let BlockAccumulator::Thinking { signature, .. } = self
                            .blocks
                            .get_mut(self.current)
                            .unwrap_or(&mut BlockAccumulator::Empty)
                            && let Some(value) = delta["signature"].as_str()
                        {
                            *signature = Some(value.to_owned());
                        }
                    }
                    "input_json_delta" => {
                        let part = delta["partial_json"].as_str().unwrap_or_default();
                        if !part.is_empty() && !charge_stream(&mut self.accumulated, part.len()) {
                            self.terminal_sent = true;
                            return vec![crate::driver::protocol_error(
                                "stream exceeded the output limit",
                            )];
                        }
                        if let Some(id) = self.current_id() {
                            if let BlockAccumulator::ToolUse { arguments, .. } =
                                &mut self.blocks[self.current]
                            {
                                arguments.push_str(part);
                            }
                            if !part.is_empty() {
                                return vec![StreamEvent::ToolCallDelta {
                                    id,
                                    partial_json: part.to_owned(),
                                }];
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {}
            "message_delta" => {
                if let Some(stop) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(map_stop_reason(stop));
                }
                if event.get("usage").is_some() {
                    let parsed = usage_from_value(&event["usage"]);
                    let merged = merge_usage(
                        Some(Usage {
                            input_tokens: self.input_tokens,
                            output_tokens: self.output_tokens,
                            cache_read_tokens: self.cache_read_tokens,
                        }),
                        parsed,
                    );
                    self.input_tokens = merged.input_tokens;
                    self.output_tokens = merged.output_tokens;
                    self.cache_read_tokens = merged.cache_read_tokens;
                }
            }
            "message_stop" => {
                self.message_stopped = true;
                return vec![self.assemble()];
            }
            "error" => {
                self.terminal_sent = true;
                return vec![StreamEvent::Error(ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("provider error frame"),
                ))];
            }
            "ping" => {}
            _ => {}
        }
        Vec::new()
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("messages stream ended after terminal");
        }
        if self.message_stopped || self.stop_reason.is_some() {
            return self.assemble();
        }
        crate::driver::protocol_error("messages stream ended before message_stop")
    }
}
