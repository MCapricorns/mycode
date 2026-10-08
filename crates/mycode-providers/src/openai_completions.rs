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
    MAX_STREAM_INDEX, ReasoningReplay, append_interruption, apply_reasoning_effort,
    assemble_blocks, assembled_stop_reason, charge_stream, join_text, join_thinking,
    map_stop_reason, merge_usage, provider_error_detail, reasoning_replay, usage_from_value,
};

/// Concatenation separator for multi-part system prompts.
const SYSTEM_JOIN: &str = "\n\n";

/// Converts one provider-neutral request into a completions body.
#[must_use]
pub(crate) fn build_body(model: &str, endpoint: &str, request: &Request) -> Value {
    let mut messages = Vec::new();
    if !request.system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": request.system_prompt.join(SYSTEM_JOIN),
        }));
    }
    for message in &request.messages {
        convert_message(model, endpoint, message, &mut messages);
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
        apply_reasoning_effort(&mut body, model, endpoint, level);
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

fn convert_message(model: &str, endpoint: &str, message: &Message, messages: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            messages.push(json!({"role": "user", "content": user_content(&user.content)}));
        }
        Message::Assistant(assistant) => {
            let text = join_text(&assistant.blocks);
            let thinking = join_thinking(&assistant.blocks);
            let replay = reasoning_replay(model, endpoint);
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
            // A thinking-only assistant becomes `{"role":"assistant"}` with
            // no content; MiniMax and generic OpenAI gateways reject that.
            // GLM, DeepSeek, and Kimi need the reasoning echoed instead,
            // including a thinking-only turn (`content: ""`).
            let echo_thinking = !thinking.is_empty() && replay == ReasoningReplay::Content;
            if text.is_empty() && tool_calls.is_empty() && !echo_thinking {
                return;
            }
            let mut wire = json!({"role": "assistant"});
            if !text.is_empty() || echo_thinking {
                wire["content"] = json!(text);
            }
            if !tool_calls.is_empty() {
                wire["tool_calls"] = json!(tool_calls);
            }
            if !thinking.is_empty() {
                match replay {
                    ReasoningReplay::Content => {
                        wire["reasoning_content"] = json!(thinking);
                    }
                    ReasoningReplay::Details => {
                        wire["reasoning_details"] = json!([{
                            "type": "reasoning.text",
                            "text": thinking,
                        }]);
                    }
                    ReasoningReplay::Omit => {}
                }
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
    /// Detail for [`StopReason::Error`], when the stream failed after bytes.
    interrupt: Option<String>,
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

    fn begin_interrupt(&mut self, detail: &str) {
        if self.interrupt.is_none() {
            self.interrupt = Some(detail.to_owned());
        }
        self.stop_reason = Some(StopReason::Error);
    }

    fn has_partial(&self) -> bool {
        !self.thinking.is_empty()
            || !self.text.is_empty()
            || self.tool_calls.iter().any(|call| {
                call.id.is_some() || !call.name.is_empty() || !call.arguments.is_empty()
            })
    }

    fn assemble(&mut self) -> StreamEvent {
        for piece in self.xml.finish() {
            if let crate::xml_tool_calls::XmlPiece::Text(text) = piece {
                self.text.push_str(&text);
            }
        }
        if let Some(detail) = self.interrupt.clone() {
            append_interruption(&mut self.text, &detail);
        }
        let calls: Vec<(&str, &str, &str)> = self
            .tool_calls
            .iter()
            .map(|call| {
                (
                    call.id.as_deref().unwrap_or_default(),
                    call.name.as_str(),
                    call.arguments.as_str(),
                )
            })
            .collect();
        let has_calls = calls
            .iter()
            .any(|(id, name, _)| !id.is_empty() && !name.is_empty());
        let blocks = assemble_blocks(&self.thinking, &self.text, calls);
        // XML-filtered calls arrive without a `tool_calls` finish reason;
        // any dispatched call set must read as tool use (length stays).
        // An interruption is not tool use: the calls were not finished.
        let stop_reason =
            assembled_stop_reason(self.interrupt.is_some(), has_calls, self.stop_reason);
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
        if data.trim().is_empty() {
            return Vec::new();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            if self.has_partial() {
                self.begin_interrupt("invalid completions frame");
                self.terminal_sent = true;
                return vec![self.assemble()];
            }
            return vec![crate::driver::protocol_error("invalid completions frame")];
        };
        let mut events = Vec::new();
        if chunk["choices"].get(0).is_none()
            && let Some(detail) = provider_error_detail(&chunk)
        {
            self.begin_interrupt(&detail);
            self.terminal_sent = true;
            return vec![self.assemble()];
        }
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
                let reason = map_stop_reason(finish);
                if reason == StopReason::Error {
                    self.begin_interrupt(finish);
                } else if self.stop_reason != Some(StopReason::Error) {
                    self.stop_reason = Some(reason);
                }
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
        if self.stop_reason.is_none() && self.has_partial() {
            self.begin_interrupt("the stream ended before a finish reason");
        }
        self.assemble()
    }

    fn interrupt(&mut self, detail: &str) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("completions stream ended after terminal");
        }
        self.begin_interrupt(detail);
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

#[cfg(test)]
mod tests {
    use mycode_core::{
        AssistantMessage, ContentBlock, Message, Request, StopReason, TextBlock, ThinkingBlock,
        UserMessage,
    };

    use super::CompletionsReducer;
    use crate::driver::FrameReducer;
    use crate::openai_completions::build_body;

    fn text_of(message: &AssistantMessage) -> String {
        message
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

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

    fn take_done(events: Vec<mycode_core::StreamEvent>) -> AssistantMessage {
        events
            .into_iter()
            .find_map(|event| match event {
                mycode_core::StreamEvent::Done { message } => Some(message),
                _ => None,
            })
            .expect("done")
    }

    #[test]
    fn glm_error_finish_keeps_thinking() {
        let mut reducer = CompletionsReducer::new();
        let deltas = reducer.feed(
            r#"{"choices":[{"delta":{"reasoning_content":"plan the page"},"finish_reason":null}]}"#,
        );
        assert!(deltas.iter().any(|event| matches!(
            event,
            mycode_core::StreamEvent::ThinkingDelta(text) if text == "plan the page"
        )));
        reducer.feed(
            r#"{"choices":[{"delta":{},"finish_reason":"error"}],"usage":{"prompt_tokens":3}}"#,
        );
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "plan the page");
        assert!(text_of(&message).contains("[error] the response was interrupted: error"));
        assert_eq!(message.stop_reason, StopReason::Error);
    }

    #[test]
    fn thinking_only_stop_is_success() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"only thought"}}]}"#);
        reducer.feed(r#"{"choices":[{"finish_reason":"stop"}]}"#);
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "only thought");
        assert!(text_of(&message).is_empty());
        assert_eq!(message.stop_reason, StopReason::Stop);
    }

    #[test]
    fn usage_only_and_blank_frames_do_not_wipe() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"kept"}}]}"#);
        assert!(reducer.feed("").is_empty());
        assert!(reducer.feed("   ").is_empty());
        assert!(
            reducer
                .feed(r#"{"choices":[],"usage":{"completion_tokens":1}}"#)
                .is_empty()
        );
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "kept");
    }

    #[test]
    fn invalid_json_after_thinking_assembles_the_partial() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
        let message = take_done(reducer.feed("not-json"));
        assert!(text_of(&message).contains("hello"));
        assert!(text_of(&message).contains("invalid completions frame"));
        assert_eq!(message.stop_reason, StopReason::Error);
    }

    #[test]
    fn eof_without_finish_reason_notes_the_partial() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"mid"}}]}"#);
        let mycode_core::StreamEvent::Done { message } = reducer.finish() else {
            panic!("done");
        };
        assert_eq!(thinking_of(&message), "mid");
        assert!(text_of(&message).contains("finish reason"));
        assert_eq!(message.stop_reason, StopReason::Error);
    }

    #[test]
    fn provider_error_object_is_a_visible_message() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(r#"{"choices":[{"delta":{"reasoning_content":"before"}}]}"#);
        let message = take_done(reducer.feed(r#"{"error":{"message":"rate limit"}}"#));
        assert_eq!(thinking_of(&message), "before");
        assert!(text_of(&message).contains("rate limit"));
    }

    #[test]
    fn glm_replays_reasoning_content_including_thinking_only() {
        let request = Request {
            messages: vec![
                std::sync::Arc::new(Message::User(UserMessage::text("go"))),
                std::sync::Arc::new(Message::Assistant(AssistantMessage {
                    blocks: vec![ContentBlock::Thinking(ThinkingBlock::new("hidden"))],
                    usage: None,
                    stop_reason: StopReason::Stop,
                })),
            ],
            ..Request::default()
        };
        let glm = build_body("glm-4.7", "https://open.bigmodel.cn/api/paas/v4", &request);
        let replayed = &glm["messages"][1];
        assert_eq!(replayed["reasoning_content"], "hidden");
        assert_eq!(replayed["content"], "");

        let openai = build_body("gpt-5", "https://api.openai.com/v1", &request);
        assert_eq!(openai["messages"].as_array().unwrap().len(), 1);

        let minimax = build_body(
            "MiniMax-M2.5",
            "https://api.minimaxi.com/v1",
            &Request {
                messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                    blocks: vec![
                        ContentBlock::Thinking(ThinkingBlock::new("why")),
                        ContentBlock::Text(TextBlock::new("answer")),
                    ],
                    usage: None,
                    stop_reason: StopReason::Stop,
                }))],
                ..Request::default()
            },
        );
        assert!(minimax["messages"][0].get("reasoning_content").is_none());
        assert_eq!(
            minimax["messages"][0]["reasoning_details"][0]["text"],
            "why"
        );
    }
}
