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
    MAX_STREAM_INDEX, append_interruption, assembled_stop_reason, charge_stream, glm_target,
    map_stop_reason, merge_usage, provider_error_detail, usage_from_value,
};

/// Output ceiling used only when models.dev publishes no `limit.output`.
///
/// The Messages API requires `max_tokens`. A published model uses its own
/// output cap; this fallback is not a guess about any particular model.
const MAX_TOKENS_WHEN_UNPUBLISHED: u64 = 8192;

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

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.params_schema,
    })
}

fn convert_message(model: &str, endpoint: &str, message: &Message, messages: &mut Vec<Value>) {
    let glm = glm_target(model, endpoint);
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
                    // whenever a signature exists. GLM coding plans also accept
                    // the unsigned thinking text from the previous turn.
                    ContentBlock::Thinking(thinking) => {
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

    fn current_id(&self) -> Option<String> {
        match self.blocks.get(self.current)? {
            BlockAccumulator::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        }
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
                    if id.is_empty() || name.is_empty() {
                        continue;
                    }
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
        if let Some(detail) = self.interrupt.clone() {
            let note_target = blocks.iter_mut().rev().find_map(|block| match block {
                ContentBlock::Text(text) => Some(&mut text.text),
                _ => None,
            });
            if let Some(text) = note_target {
                append_interruption(text, &detail);
            } else {
                let mut text = String::new();
                append_interruption(&mut text, &detail);
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

impl FrameReducer for MessagesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        if data.trim().is_empty() {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            if self.has_content() {
                self.begin_interrupt("invalid messages frame");
                return vec![self.assemble()];
            }
            return vec![crate::driver::protocol_error("invalid messages frame")];
        };
        let event_type = event["type"].as_str().unwrap_or_default();
        match event_type {
            "message_start" => {
                let parsed = usage_from_value(&event["message"]["usage"]);
                let merged = merge_usage(Some(self.snapshot()), parsed);
                self.apply_usage(merged);
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
                            if !matches!(
                                self.blocks.get(self.current),
                                Some(BlockAccumulator::Thinking { .. })
                            ) {
                                self.blocks.push(BlockAccumulator::Thinking {
                                    text: String::new(),
                                    signature: None,
                                });
                                self.current = self.blocks.len() - 1;
                            }
                            if let BlockAccumulator::Thinking { text, .. } =
                                &mut self.blocks[self.current]
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
                    let reason = map_stop_reason(stop);
                    if reason == StopReason::Error {
                        self.begin_interrupt(stop);
                    } else if self.stop_reason != Some(StopReason::Error) {
                        self.stop_reason = Some(reason);
                    }
                }
                if event.get("usage").is_some() {
                    let parsed = usage_from_value(&event["usage"]);
                    let merged = merge_usage(Some(self.snapshot()), parsed);
                    self.apply_usage(merged);
                }
            }
            "message_stop" => {
                self.message_stopped = true;
                return vec![self.assemble()];
            }
            "error" => {
                let detail = provider_error_detail(&event).unwrap_or_else(|| {
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("provider error frame")
                        .to_owned()
                });
                self.begin_interrupt(&detail);
                return vec![self.assemble()];
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
