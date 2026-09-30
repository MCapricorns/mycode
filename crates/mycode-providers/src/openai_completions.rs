//! OpenAI Chat Completions wire protocol adapter.
//!
//! Covers every OpenAI-compatible endpoint (OpenAI, DeepSeek, Kimi/Moonshot,
//! Z.AI gateways, custom `…/v1` bases). Vendor differences are data; this
//! adapter only owns the wire shape.

use serde_json::{Value, json};

use mycode_core::{ContentBlock, Message, StopReason, ToolSpec, Usage};
use mycode_core::{Request, StreamEvent};

use crate::driver::FrameReducer;
use crate::wire_common::{
    MAX_STREAM_INDEX, apply_reasoning_effort, assemble_blocks, charge_stream, join_text,
    map_stop_reason, merge_usage, usage_from_value,
};

/// Concatenation separator for multi-part system prompts.
const SYSTEM_JOIN: &str = "\n\n";

/// Converts one provider-neutral request into a completions body.
#[must_use]
pub(crate) fn build_body(model: &str, request: &Request) -> Value {
    let mut messages = Vec::new();
    if !request.system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": request.system_prompt.join(SYSTEM_JOIN),
        }));
    }
    for message in &request.messages {
        convert_message(message, &mut messages);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(level) = request.reasoning {
        apply_reasoning_effort(&mut body, level);
    }
    body
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.params_schema,
        },
    })
}

fn convert_message(message: &Message, messages: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            messages.push(json!({"role": "user", "content": user_content(&user.content)}));
        }
        Message::Assistant(assistant) => {
            let text = join_text(&assistant.blocks);
            let tool_calls: Vec<Value> = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(call) => Some(json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        },
                    })),
                    _ => None,
                })
                .collect();
            // Thinking blocks have no completions replay channel; providers
            // that need them expose their own reasoning fields.
            // A thinking-only assistant becomes `{"role":"assistant"}` with
            // no content; MiniMax and similar gateways reject that as
            // "unrecognized chat message".
            if text.is_empty() && tool_calls.is_empty() {
                return;
            }
            let mut wire = json!({"role": "assistant"});
            if !text.is_empty() {
                wire["content"] = json!(text);
            }
            if !tool_calls.is_empty() {
                wire["tool_calls"] = json!(tool_calls);
            }
            messages.push(wire);
        }
        Message::ToolResult(result) => {
            let content = join_text(&result.content);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": content,
            }));
        }
        Message::Custom(_) => {}
    }
}

fn user_content(content: &[ContentBlock]) -> Value {
    let has_image = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(_)));
    if !has_image {
        return json!(join_text(content));
    }
    let parts: Vec<Value> = content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => json!({"type": "text", "text": text.text}),
            ContentBlock::Image(image) => json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{}", image.mime_type, image.data)},
            }),
            _ => json!({"type": "text", "text": ""}),
        })
        .collect();
    json!(parts)
}

/// One streaming tool call being stitched from argument fragments.
#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: String,
    arguments: String,
    /// Argument bytes held while the call id is still unknown.
    pending: String,
}

/// Accumulates Chat Completions stream chunks.
#[derive(Default)]
pub(crate) struct CompletionsReducer {
    thinking: String,
    text: String,
    tool_calls: Vec<ToolCallAccumulator>,
    usage: Option<Usage>,
    stop_reason: Option<StopReason>,
    terminal_sent: bool,
    /// Bytes retained across text, thinking, and tool-argument fragments.
    accumulated: usize,
    /// Extracts `<tool_call>` markup some endpoints stream as plain text.
    xml: crate::xml_tool_calls::XmlToolCallParser,
    /// Counter for synthetic ids minted by the XML filter.
    xml_calls: usize,
}

impl CompletionsReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn assemble(&mut self) -> StreamEvent {
        for piece in self.xml.finish() {
            if let crate::xml_tool_calls::XmlPiece::Text(text) = piece {
                self.text.push_str(&text);
            }
        }
        let blocks = assemble_blocks(
            &self.thinking,
            &self.text,
            self.tool_calls.iter().map(|call| {
                (
                    call.id.as_deref().unwrap_or_default(),
                    call.name.as_str(),
                    call.arguments.as_str(),
                )
            }),
        );
        // XML-filtered calls arrive without a `tool_calls` finish reason;
        // any dispatched call set must read as tool use (length stays).
        let stop_reason =
            if !self.tool_calls.is_empty() && self.stop_reason != Some(StopReason::Length) {
                StopReason::ToolUse
            } else {
                self.stop_reason.unwrap_or(StopReason::Stop)
            };
        StreamEvent::Done {
            message: mycode_core::AssistantMessage {
                blocks,
                usage: self.usage,
                stop_reason,
            },
        }
    }

    /// Runs one streamed content fragment through the XML tool-call filter.
    fn absorb_text(&mut self, text: &str, events: &mut Vec<StreamEvent>) {
        for piece in self.xml.feed(text) {
            match piece {
                crate::xml_tool_calls::XmlPiece::Text(text) => {
                    self.text.push_str(&text);
                    events.push(StreamEvent::TextDelta(text));
                }
                crate::xml_tool_calls::XmlPiece::ToolCall { name, arguments } => {
                    self.xml_calls += 1;
                    let id = format!("call-xml-{}", self.xml_calls);
                    events.push(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        partial_json: arguments.clone(),
                    });
                    self.tool_calls.push(ToolCallAccumulator {
                        id: Some(id),
                        name,
                        arguments,
                        pending: String::new(),
                    });
                }
            }
        }
    }
}

impl FrameReducer for CompletionsReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        if data.trim() == "[DONE]" {
            self.terminal_sent = true;
            return vec![self.assemble()];
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return vec![crate::driver::protocol_error("invalid completions frame")];
        };
        let mut events = Vec::new();
        if let Some(choice) = chunk["choices"].get(0) {
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str()
                && !text.is_empty()
            {
                if !charge_stream(&mut self.accumulated, text.len()) {
                    self.terminal_sent = true;
                    return vec![crate::driver::protocol_error(
                        "stream exceeded the output limit",
                    )];
                }
                self.absorb_text(text, &mut events);
            }
            let reasoning = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str());
            if let Some(text) = reasoning
                && !text.is_empty()
            {
                if !charge_stream(&mut self.accumulated, text.len()) {
                    self.terminal_sent = true;
                    return vec![crate::driver::protocol_error(
                        "stream exceeded the output limit",
                    )];
                }
                self.thinking.push_str(text);
                events.push(StreamEvent::ThinkingDelta(text.to_owned()));
            }
            if let Some(fragments) = delta["tool_calls"].as_array() {
                for fragment in fragments {
                    if let Err(message) = self.absorb_tool_fragment(fragment, &mut events) {
                        self.terminal_sent = true;
                        return vec![crate::driver::protocol_error(message)];
                    }
                }
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                self.stop_reason = Some(map_stop_reason(finish));
            }
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(merge_usage(self.usage, usage_from_value(usage)));
        }
        events
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("completions stream ended after terminal");
        }
        self.terminal_sent = true;
        self.assemble()
    }
}

impl CompletionsReducer {
    fn absorb_tool_fragment(
        &mut self,
        fragment: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), &'static str> {
        let index = fragment["index"].as_u64().unwrap_or_default();
        if index > MAX_STREAM_INDEX {
            return Err("tool call index exceeds the stream limit");
        }
        let index = index as usize;
        while self.tool_calls.len() <= index {
            self.tool_calls.push(ToolCallAccumulator::default());
        }
        if let Some(id) = fragment["id"].as_str()
            && self.tool_calls[index].id.is_none()
        {
            if !charge_stream(&mut self.accumulated, id.len()) {
                return Err("stream exceeded the output limit");
            }
            let call = &mut self.tool_calls[index];
            call.id = Some(id.to_owned());
            if !call.pending.is_empty() {
                let pending = std::mem::take(&mut call.pending);
                call.arguments.push_str(&pending);
                events.push(StreamEvent::ToolCallDelta {
                    id: id.to_owned(),
                    partial_json: pending,
                });
            }
        }
        if let Some(name) = fragment["function"]["name"].as_str() {
            if !charge_stream(&mut self.accumulated, name.len()) {
                return Err("stream exceeded the output limit");
            }
            self.tool_calls[index].name.push_str(name);
        }
        if let Some(arguments) = fragment["function"]["arguments"].as_str()
            && !arguments.is_empty()
        {
            if !charge_stream(&mut self.accumulated, arguments.len()) {
                return Err("stream exceeded the output limit");
            }
            let call = &mut self.tool_calls[index];
            match &call.id {
                Some(id) => {
                    let id = id.clone();
                    call.arguments.push_str(arguments);
                    events.push(StreamEvent::ToolCallDelta {
                        id,
                        partial_json: arguments.to_owned(),
                    });
                }
                None => call.pending.push_str(arguments),
            }
        }
        Ok(())
    }
}
