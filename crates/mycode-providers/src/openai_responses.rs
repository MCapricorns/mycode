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
    MAX_STREAM_INDEX, SYSTEM_JOIN, append_interruption, apply_responses_thinking, assemble_blocks,
    assembled_stop_reason, charge_stream, join_text, merge_usage, provider_error_detail,
    usage_from_value,
};

/// Converts one provider-neutral request into a Responses body.
#[must_use]
pub(crate) fn build_body(model: &str, endpoint: &str, request: &Request) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        convert_message(message, &mut input);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "input": input,
        "stream": true,
        // The ChatGPT Codex backend rejects stored responses outright
        // ("Store must be set to false"); every caller replays the full
        // input each turn, so server-side state is never depended on.
        "store": false,
    });
    if !request.system_prompt.is_empty() {
        body["instructions"] = json!(request.system_prompt.join(SYSTEM_JOIN));
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(limit) = request.max_output_tokens.filter(|tokens| *tokens > 0) {
        body["max_output_tokens"] = json!(limit);
    }
    if let Some(level) = request.reasoning {
        apply_responses_thinking(&mut body, model, endpoint, level);
    } else if let Some(token) = request.reasoning_token.as_deref() {
        body["reasoning"] = json!({ "effort": token });
    }
    crate::cache::apply_prompt_cache_key(&mut body, endpoint, request.prompt_cache_key.as_deref());
    body
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": crate::wire_common::tool_parameters(tool),
    })
}

/// Converts one message for the Responses API. Image blocks are dropped:
/// the only Responses preset is the Codex backend, whose tool surface this
/// adapter targets, and image input is not wired through it.
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
    }
}

#[derive(Default)]
struct FunctionCallAccumulator {
    id: String,
    item_id: String,
    output_index: Option<u64>,
    name: String,
    arguments: String,
}

/// Accumulates Responses SSE events.
#[derive(Default)]
pub(crate) struct ResponsesReducer {
    thinking: String,
    text: String,
    function_calls: Vec<FunctionCallAccumulator>,
    usage: Option<Usage>,
    /// Set once `response.completed` or `response.incomplete` arrives.
    completed: bool,
    /// Detail for [`StopReason::Error`] when the stream fails after bytes.
    interrupt: Option<String>,
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

    fn begin_interrupt(&mut self, detail: &str) {
        if self.interrupt.is_none() {
            self.interrupt = Some(detail.to_owned());
        }
    }

    fn has_partial(&self) -> bool {
        !self.thinking.is_empty()
            || !self.text.is_empty()
            || self.function_calls.iter().any(|call| {
                !call.id.is_empty() || !call.name.is_empty() || !call.arguments.is_empty()
            })
    }

    fn assemble(&mut self) -> StreamEvent {
        self.terminal_sent = true;
        if let Some(detail) = self.interrupt.as_deref() {
            append_interruption(&mut self.text, detail);
        }
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
        let has_calls = self
            .function_calls
            .iter()
            .any(|call| !call.id.is_empty() && !call.name.is_empty());
        let recorded = if self.interrupt.is_some() {
            Some(StopReason::Error)
        } else if self.length_limited {
            Some(StopReason::Length)
        } else {
            None
        };
        let stop_reason = assembled_stop_reason(self.interrupt.is_some(), has_calls, recorded);
        StreamEvent::Done {
            message: AssistantMessage {
                blocks,
                usage: self.usage,
                stop_reason,
            },
        }
    }
}

impl ResponsesReducer {
    /// Resolves the target function call for an event by `call_id`, `item_id`,
    /// or `output_index`, falling back to the last call.
    fn call_index_for(&self, event: &Value, item: Option<&Value>) -> Option<usize> {
        let call_id = item
            .and_then(|item| item["call_id"].as_str())
            .or_else(|| event["call_id"].as_str())
            .filter(|id| !id.is_empty());
        if let Some(call_id) = call_id
            && let Some(pos) = self
                .function_calls
                .iter()
                .rposition(|call| call.id == call_id)
        {
            return Some(pos);
        }
        let item_id = item
            .and_then(|item| item["id"].as_str())
            .or_else(|| event["item_id"].as_str())
            .filter(|id| !id.is_empty());
        if let Some(item_id) = item_id
            && let Some(pos) = self
                .function_calls
                .iter()
                .rposition(|call| call.item_id == item_id)
        {
            return Some(pos);
        }
        if let Some(output_index) = event["output_index"].as_u64()
            && let Some(pos) = self
                .function_calls
                .iter()
                .rposition(|call| call.output_index == Some(output_index))
        {
            return Some(pos);
        }
        self.function_calls.len().checked_sub(1)
    }

    /// Sets the full argument string on a call whose deltas never arrived.
    /// A call that already streamed arguments keeps them.
    fn fill_arguments(&mut self, index: Option<usize>, full: &str) -> Vec<StreamEvent> {
        let Some(call) = index.and_then(|index| self.function_calls.get_mut(index)) else {
            return Vec::new();
        };
        if !call.arguments.is_empty() || full.is_empty() {
            return Vec::new();
        }
        if !charge_stream(&mut self.accumulated, full.len()) {
            self.terminal_sent = true;
            return vec![crate::driver::protocol_error(
                "stream exceeded the output limit",
            )];
        }
        full.clone_into(&mut call.arguments);
        if call.id.is_empty() {
            return Vec::new();
        }
        vec![StreamEvent::ToolCallDelta {
            id: call.id.clone(),
            partial_json: full.to_owned(),
        }]
    }
}

impl FrameReducer for ResponsesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        if data.trim().is_empty() {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            if self.has_partial() {
                self.begin_interrupt("invalid responses frame");
                return vec![self.assemble()];
            }
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
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
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
                    let item_id = item["id"].as_str().unwrap_or_default();
                    let output_index = event["output_index"].as_u64();
                    let name = item["name"].as_str().unwrap_or_default();
                    if !charge_stream(&mut self.accumulated, id.len() + item_id.len() + name.len())
                    {
                        self.terminal_sent = true;
                        return vec![crate::driver::protocol_error(
                            "stream exceeded the output limit",
                        )];
                    }
                    self.function_calls.push(FunctionCallAccumulator {
                        id: id.to_owned(),
                        item_id: item_id.to_owned(),
                        output_index,
                        name: name.to_owned(),
                        arguments: String::new(),
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
                let index = self.call_index_for(&event, None);
                if let Some(call) = index.and_then(|index| self.function_calls.get_mut(index)) {
                    call.arguments.push_str(part);
                    if !part.is_empty() && !call.id.is_empty() {
                        return vec![StreamEvent::ToolCallDelta {
                            id: call.id.clone(),
                            partial_json: part.to_owned(),
                        }];
                    }
                }
            }
            // Gateways that skip the argument deltas still send the whole
            // string when the item closes; without this the call ran with `{}`.
            "response.function_call_arguments.done" => {
                let full = event["arguments"].as_str().unwrap_or_default();
                let index = self.call_index_for(&event, None);
                return self.fill_arguments(index, full);
            }
            "response.output_item.done" => {
                let item = &event["item"];
                if item["type"].as_str() == Some("function_call") {
                    let index = self.call_index_for(&event, Some(item));
                    if let Some(call) = index.and_then(|index| self.function_calls.get_mut(index)) {
                        if call.id.is_empty()
                            && let Some(id) = item["call_id"].as_str().filter(|id| !id.is_empty())
                        {
                            id.clone_into(&mut call.id);
                        }
                        if call.name.is_empty()
                            && let Some(name) = item["name"].as_str().filter(|n| !n.is_empty())
                        {
                            name.clone_into(&mut call.name);
                        }
                    }
                    return self
                        .fill_arguments(index, item["arguments"].as_str().unwrap_or_default());
                }
            }
            "response.completed" | "response.incomplete" => {
                self.completed = true;
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
                let detail = provider_error_detail(&event)
                    .or_else(|| provider_error_detail(&event["response"]))
                    .unwrap_or_else(|| "responses stream failed".to_owned());
                self.begin_interrupt(&detail);
                return vec![self.assemble()];
            }
            _ => {}
        }
        Vec::new()
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("responses stream ended after terminal");
        }
        if !self.completed && self.has_partial() {
            self.begin_interrupt("the stream ended before response.completed");
        }
        self.assemble()
    }

    fn interrupt(&mut self, detail: &str) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("responses stream ended after terminal");
        }
        self.begin_interrupt(detail);
        self.assemble()
    }
}

#[cfg(test)]
mod tests {
    use mycode_core::{ContentBlock, StopReason};

    use super::ResponsesReducer;
    use crate::driver::FrameReducer;

    #[test]
    fn codex_bodies_are_never_stored_server_side() {
        let body = super::build_body(
            "gpt-5.6-sol",
            "https://chatgpt.com/backend-api/codex/responses",
            &mycode_core::Request::new(),
        );
        assert_eq!(body["store"], serde_json::json!(false));
    }

    #[test]
    fn arguments_sent_only_when_the_item_closes_are_kept() {
        let mut reducer = ResponsesReducer::new();
        reducer.feed(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"read"}}"#,
        );
        reducer.feed(
            r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\":\"a.rs\"}"}}"#,
        );
        reducer.feed(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_2","name":"grep"}}"#,
        );
        reducer
            .feed(r#"{"type":"response.function_call_arguments.delta","delta":"{\"pattern\":"}"#);
        reducer.feed(r#"{"type":"response.function_call_arguments.delta","delta":"\"x\"}"}"#);
        // A done event never replaces arguments that already streamed.
        reducer.feed(
            r#"{"type":"response.function_call_arguments.done","arguments":"{\"pattern\":\"other\"}"}"#,
        );
        let events = reducer.feed(r#"{"type":"response.completed","response":{"usage":{}}}"#);
        let mycode_core::StreamEvent::Done { message } = events.into_iter().next().unwrap() else {
            panic!("done");
        };
        let calls: Vec<_> = message
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.arguments.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(calls[0]["path"], "a.rs");
        assert_eq!(calls[1]["pattern"], "x");
        assert_eq!(message.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn parallel_function_calls_route_arguments_by_item_id_and_output_index() {
        let mut reducer = ResponsesReducer::new();
        reducer.feed(
            r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"read"}}"#,
        );
        reducer.feed(
            r#"{"type":"response.output_item.added","output_index":1,"item":{"id":"fc_2","type":"function_call","call_id":"call_2","name":"grep"}}"#,
        );
        // Deltas and done events for fc_1 arrive after fc_2 was added.
        reducer.feed(
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"path\":"}"#,
        );
        reducer.feed(
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"\"a.rs\"}"}"#,
        );
        reducer.feed(
            r#"{"type":"response.function_call_arguments.done","item_id":"fc_2","output_index":1,"arguments":"{\"pattern\":\"x\"}"}"#,
        );
        let events = reducer.feed(r#"{"type":"response.completed","response":{"usage":{}}}"#);
        let mycode_core::StreamEvent::Done { message } = events.into_iter().next().unwrap() else {
            panic!("done");
        };
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
        assert_eq!(calls[0].0, "call_1");
        assert_eq!(calls[0].2["path"], "a.rs");
        assert_eq!(calls[1].0, "call_2");
        assert_eq!(calls[1].2["pattern"], "x");
    }

    #[test]
    fn failed_event_keeps_reasoning_summary() {
        let mut reducer = ResponsesReducer::new();
        reducer.feed(r#"{"type":"response.reasoning_summary_text.delta","delta":"consider"}"#);
        let events = reducer
            .feed(r#"{"type":"response.failed","response":{"error":{"message":"server busy"}}}"#);
        let mycode_core::StreamEvent::Done { message } = events.into_iter().next().unwrap() else {
            panic!("done");
        };
        let thinking = message
            .blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::Thinking(thinking) => Some(thinking.text.as_str()),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(thinking, "consider");
        assert!(message.text().contains("server busy"));
        assert_eq!(message.stop_reason, StopReason::Error);
    }

    #[test]
    fn eof_before_completed_keeps_text() {
        let mut reducer = ResponsesReducer::new();
        reducer.feed(r#"{"type":"response.output_text.delta","delta":"partial answer"}"#);
        let mycode_core::StreamEvent::Done { message } = reducer.finish() else {
            panic!("done");
        };
        assert!(message.text().contains("partial answer"));
        assert!(message.text().contains("response.completed"));
        assert_eq!(message.stop_reason, StopReason::Error);
    }
}
