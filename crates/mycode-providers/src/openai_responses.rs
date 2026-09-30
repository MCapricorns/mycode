//! OpenAI Responses wire protocol adapter.
//!
//! Targets the current Responses streaming shape: `response.output_text.delta`
//! for text, `response.function_call_arguments.delta` for tool arguments, and
//! `response.completed` for usage. Reasoning summaries stream as thinking
//! deltas when the endpoint provides them.

use serde_json::{Value, json};

use mycode_core::{AssistantMessage, ContentBlock, Message, StopReason, ToolSpec, Usage};
use mycode_core::{Request, StreamEvent};

use crate::driver::FrameReducer;
use crate::wire_common::{
    MAX_STREAM_INDEX, apply_reasoning_effort, assemble_blocks, charge_stream, join_text,
    merge_usage, usage_from_value,
};

/// Converts one provider-neutral request into a Responses body.
#[must_use]
pub(crate) fn build_body(model: &str, request: &Request) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        convert_message(message, &mut input);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "input": input,
        "stream": true,
    });
    if !request.system_prompt.is_empty() {
        body["instructions"] = json!(request.system_prompt.join("\n\n"));
    }
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
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.params_schema,
    })
}

fn convert_message(message: &Message, input: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            let text = join_text(&user.content);
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}],
            }));
        }
        Message::Assistant(assistant) => {
            for block in &assistant.blocks {
                match block {
                    ContentBlock::Text(text) => {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text.text}],
                        }));
                    }
                    // The Responses API owns its reasoning items; replaying
                    // signed thinking from other protocols is not possible.
                    ContentBlock::Thinking(_) => {}
                    ContentBlock::ToolCall(call) => {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        }));
                    }
                    ContentBlock::Image(_) => {}
                }
            }
        }
        Message::ToolResult(result) => {
            let output = join_text(&result.content);
            input.push(json!({
                "type": "function_call_output",
                "call_id": result.tool_call_id,
                "output": output,
            }));
        }
        Message::Custom(_) => {}
    }
}

#[derive(Default)]
struct FunctionCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    text_emitted: bool,
}

/// Accumulates Responses SSE events.
#[derive(Default)]
pub(crate) struct ResponsesReducer {
    thinking: String,
    text: String,
    function_calls: Vec<FunctionCallAccumulator>,
    usage: Option<Usage>,
    terminal_sent: bool,
    /// Bytes retained across text, thinking, and tool-argument fragments.
    accumulated: usize,
    /// The provider cut the response off at the output-token limit.
    length_limited: bool,
}

impl ResponsesReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn assemble(&mut self) -> StreamEvent {
        self.terminal_sent = true;
        let blocks = assemble_blocks(
            &self.thinking,
            &self.text,
            self.function_calls.iter().map(|call| {
                (
                    call.id.as_str(),
                    call.name.as_str(),
                    call.arguments.as_str(),
                )
            }),
        );
        let stop_reason = if self.length_limited {
            StopReason::Length
        } else if !self.function_calls.is_empty() {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        };
        StreamEvent::Done {
            message: AssistantMessage {
                blocks,
                usage: self.usage,
                stop_reason,
            },
        }
    }
}

impl FrameReducer for ResponsesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return vec![crate::driver::protocol_error("invalid responses frame")];
        };
        match event["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                let part = event["delta"].as_str().unwrap_or_default();
                if !part.is_empty() {
                    if !charge_stream(&mut self.accumulated, part.len()) {
                        self.terminal_sent = true;
                        return vec![crate::driver::protocol_error(
                            "stream exceeded the output limit",
                        )];
                    }
                    self.text.push_str(part);
                    return vec![StreamEvent::TextDelta(part.to_owned())];
                }
            }
            "response.reasoning_summary_text.delta" => {
                let part = event["delta"].as_str().unwrap_or_default();
                if !part.is_empty() {
                    if !charge_stream(&mut self.accumulated, part.len()) {
                        self.terminal_sent = true;
                        return vec![crate::driver::protocol_error(
                            "stream exceeded the output limit",
                        )];
                    }
                    self.thinking.push_str(part);
                    return vec![StreamEvent::ThinkingDelta(part.to_owned())];
                }
            }
            "response.output_item.added" => {
                let item = &event["item"];
                if item["type"].as_str() == Some("function_call") {
                    if self.function_calls.len() as u64 > MAX_STREAM_INDEX {
                        self.terminal_sent = true;
                        return vec![crate::driver::protocol_error(
                            "tool call index exceeds the stream limit",
                        )];
                    }
                    let id = item["call_id"].as_str().unwrap_or_default();
                    let name = item["name"].as_str().unwrap_or_default();
                    if !charge_stream(&mut self.accumulated, id.len() + name.len()) {
                        self.terminal_sent = true;
                        return vec![crate::driver::protocol_error(
                            "stream exceeded the output limit",
                        )];
                    }
                    self.function_calls.push(FunctionCallAccumulator {
                        id: id.to_owned(),
                        name: name.to_owned(),
                        arguments: String::new(),
                        text_emitted: false,
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let part = event["delta"].as_str().unwrap_or_default();
                if !part.is_empty() && !charge_stream(&mut self.accumulated, part.len()) {
                    self.terminal_sent = true;
                    return vec![crate::driver::protocol_error(
                        "stream exceeded the output limit",
                    )];
                }
                if let Some(call) = self.function_calls.last_mut() {
                    call.arguments.push_str(part);
                    if !part.is_empty() && !call.id.is_empty() {
                        call.text_emitted = true;
                        return vec![StreamEvent::ToolCallDelta {
                            id: call.id.clone(),
                            partial_json: part.to_owned(),
                        }];
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                let response = &event["response"];
                let usage = &response["usage"];
                self.usage = Some(merge_usage(self.usage, usage_from_value(usage)));
                if event["type"].as_str() == Some("response.incomplete") {
                    let reason = response["incomplete_details"]["reason"].as_str();
                    if matches!(reason, Some("max_output_tokens" | "max_tokens" | "length")) {
                        self.length_limited = true;
                    }
                }
                return vec![self.assemble()];
            }
            "response.failed" | "error" => {
                self.terminal_sent = true;
                return vec![StreamEvent::Error(
                    mycode_core::ProviderError::with_message(
                        mycode_core::ProviderErrorKind::Rejected,
                        event["error"]["message"]
                            .as_str()
                            .or_else(|| event["response"]["error"]["message"].as_str())
                            .unwrap_or("responses stream failed"),
                    ),
                )];
            }
            _ => {}
        }
        Vec::new()
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("responses stream ended after terminal");
        }
        self.assemble()
    }
}
