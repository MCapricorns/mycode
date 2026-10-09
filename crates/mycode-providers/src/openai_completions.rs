//! OpenAI Chat Completions wire protocol adapter.
//!
//! Covers every OpenAI-compatible endpoint (OpenAI, DeepSeek, Kimi/Moonshot,
//! Z.AI gateways, custom `…/v1` bases). Vendor differences are data; this
//! adapter owns the wire shape and the stream reduction.

use serde_json::{Value, json};

use mycode_core::{ContentBlock, Message, StopReason, ToolSpec, Usage};
use mycode_core::{Request, StreamEvent};

use crate::driver::FrameReducer;
use crate::wire_common::{
    MAX_STREAM_INDEX, ReasoningReplay, SYSTEM_JOIN, append_interruption, apply_reasoning_effort,
    assemble_blocks_with_replay, assembled_stop_reason, charge_stream, join_text, join_thinking,
    merge_usage, provider_error_detail, reasoning_replay, record_provider_stop, tool_parameters,
    usage_from_value,
};

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
    if let Some(limit) = request.max_output_tokens.filter(|tokens| *tokens > 0) {
        body["max_tokens"] = json!(limit);
    }
    if let Some(level) = request.reasoning {
        apply_reasoning_effort(&mut body, model, endpoint, level);
    } else if let Some(token) = request.reasoning_token.as_deref() {
        body["reasoning_effort"] = json!(token);
    }
    if crate::cache::explicit_chat_cache(model, endpoint) {
        crate::cache::apply_chat_cache_breakpoints(&mut body);
    }
    crate::cache::apply_prompt_cache_key(&mut body, endpoint, request.prompt_cache_key.as_deref());
    body
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool_parameters(tool),
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
            // no content; generic OpenAI gateways reject that. GLM, DeepSeek,
            // and Kimi need `reasoning_content`, including a thinking-only
            // turn (`content: ""`). MiniMax needs `reasoning_details` (or
            // `reasoning_content` when that is what the turn stored).
            let echo = reasoning_echo(replay, &assistant.blocks, &thinking);
            if text.is_empty() && tool_calls.is_empty() && echo.is_none() {
                return;
            }
            let mut wire = json!({"role": "assistant"});
            if !text.is_empty() || echo.is_some() {
                wire["content"] = json!(text);
            }
            if !tool_calls.is_empty() {
                wire["tool_calls"] = json!(tool_calls);
            }
            match echo {
                Some(ReasoningEcho::Content(value)) => {
                    wire["reasoning_content"] = json!(value);
                }
                Some(ReasoningEcho::Details(details)) => {
                    wire["reasoning_details"] = details;
                }
                None => {}
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

/// Reasoning attached to one replayed assistant message.
enum ReasoningEcho {
    Content(String),
    Details(Value),
}

fn reasoning_echo(
    replay: ReasoningReplay,
    blocks: &[ContentBlock],
    thinking: &str,
) -> Option<ReasoningEcho> {
    match replay {
        ReasoningReplay::Omit => None,
        ReasoningReplay::Content => {
            (!thinking.is_empty()).then(|| ReasoningEcho::Content(thinking.to_owned()))
        }
        ReasoningReplay::MiniMax => minimax_echo(blocks, thinking),
    }
}

/// MiniMax wants the original `reasoning_details` array unchanged. A turn
/// that arrived as `reasoning_content` is echoed that way. `<think>` text
/// with no vendor blob becomes one `reasoning.text` item.
fn minimax_echo(blocks: &[ContentBlock], thinking: &str) -> Option<ReasoningEcho> {
    let mut details = Vec::new();
    let mut content = false;
    for block in blocks {
        let ContentBlock::Thinking(thinking) = block else {
            continue;
        };
        let Some(raw) = thinking.replay.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if let Some(array) = value.get("reasoning_details").and_then(Value::as_array) {
            details.extend(array.iter().cloned());
        } else if value.get("reasoning_content").and_then(Value::as_bool) == Some(true) {
            content = true;
        }
    }
    if !details.is_empty() {
        return Some(ReasoningEcho::Details(Value::Array(details)));
    }
    if thinking.is_empty() {
        return None;
    }
    if content {
        return Some(ReasoningEcho::Content(thinking.to_owned()));
    }
    Some(ReasoningEcho::Details(json!([{
        "type": "reasoning.text",
        "text": thinking,
    }])))
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

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

enum ThinkPiece {
    Thinking(String),
    Text(String),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ThinkMode {
    /// Still deciding whether content starts with `<think>`.
    #[default]
    Lead,
    Inside,
    /// The wrapper is closed, or the content never had one.
    Body,
}

#[derive(Default)]
struct ThinkSplitter {
    hold: String,
    mode: ThinkMode,
    /// A reasoning field already owns the thought, so the wrapper is discarded.
    suppress: bool,
}

impl ThinkSplitter {
    fn is_holding(&self) -> bool {
        !self.hold.is_empty()
    }

    fn push(&mut self, chunk: &str, suppress: bool) -> Vec<ThinkPiece> {
        self.suppress = suppress;
        self.hold.push_str(chunk);
        let mut out = Vec::new();
        loop {
            match self.mode {
                ThinkMode::Lead => {
                    let trimmed = self.hold.trim_start_matches([' ', '\n', '\r', '\t']);
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some(rest) = trimmed.strip_prefix(THINK_OPEN) {
                        self.hold = rest.to_owned();
                        self.mode = ThinkMode::Inside;
                        continue;
                    }
                    if THINK_OPEN.starts_with(trimmed) {
                        break;
                    }
                    out.push(ThinkPiece::Text(std::mem::take(&mut self.hold)));
                    self.mode = ThinkMode::Body;
                    break;
                }
                ThinkMode::Inside => {
                    if let Some(index) = self.hold.find(THINK_CLOSE) {
                        let thinking = self.hold[..index].to_owned();
                        let mut after = self.hold[index + THINK_CLOSE.len()..].to_owned();
                        trim_one_newline(&mut after);
                        self.hold = after;
                        self.mode = ThinkMode::Body;
                        if !thinking.is_empty() && !self.suppress {
                            out.push(ThinkPiece::Thinking(thinking));
                        }
                        continue;
                    }
                    let keep = partial_tag_suffix(&self.hold, THINK_CLOSE);
                    let emit_len = self.hold.len() - keep;
                    if emit_len > 0 {
                        let thinking = self.hold[..emit_len].to_owned();
                        self.hold.drain(..emit_len);
                        if !thinking.is_empty() && !self.suppress {
                            out.push(ThinkPiece::Thinking(thinking));
                        }
                    }
                    break;
                }
                ThinkMode::Body => {
                    if !self.hold.is_empty() {
                        out.push(ThinkPiece::Text(std::mem::take(&mut self.hold)));
                    }
                    break;
                }
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<ThinkPiece> {
        if self.hold.is_empty() {
            return Vec::new();
        }
        match self.mode {
            ThinkMode::Lead | ThinkMode::Body => {
                vec![ThinkPiece::Text(std::mem::take(&mut self.hold))]
            }
            ThinkMode::Inside => {
                let thinking = std::mem::take(&mut self.hold);
                if thinking.is_empty() || self.suppress {
                    Vec::new()
                } else {
                    vec![ThinkPiece::Thinking(thinking)]
                }
            }
        }
    }
}

fn trim_one_newline(text: &mut String) {
    if let Some(rest) = text.strip_prefix("\r\n") {
        *text = rest.to_owned();
    } else if text.starts_with(['\n', '\r']) {
        text.remove(0);
    }
}

/// Byte length of the longest suffix of `text` that is a prefix of `tag`.
fn partial_tag_suffix(text: &str, tag: &str) -> usize {
    let bytes = text.as_bytes();
    let tag = tag.as_bytes();
    let max = tag.len().saturating_sub(1).min(bytes.len());
    for len in (1..=max).rev() {
        if bytes[bytes.len() - len..] == tag[..len] {
            return len;
        }
    }
    0
}

fn detail_item_text(item: &Value) -> String {
    item.get("text")
        .or_else(|| item.get("reasoning"))
        .or_else(|| item.get("summary"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn detail_text(items: &[Value]) -> String {
    let mut joined = String::new();
    for item in items {
        joined.push_str(&detail_item_text(item));
    }
    joined
}

fn same_detail(left: &Value, right: &Value) -> bool {
    if let (Some(id), Some(other)) = (
        left.get("id").and_then(Value::as_str),
        right.get("id").and_then(Value::as_str),
    ) {
        return id == other;
    }
    if let (Some(index), Some(other)) = (
        left.get("index").and_then(Value::as_u64),
        right.get("index").and_then(Value::as_u64),
    ) {
        return index == other;
    }
    false
}

fn merge_detail(existing: &mut Value, incoming: &Value) {
    let prev = detail_item_text(existing);
    let next = detail_item_text(incoming);
    let text = if next.starts_with(&prev) {
        next
    } else if prev.starts_with(&next) {
        prev
    } else {
        format!("{prev}{next}")
    };
    let Some(src) = incoming.as_object() else {
        return;
    };
    let Some(dest) = existing.as_object_mut() else {
        return;
    };
    for (key, value) in src {
        if key != "text" {
            dest.insert(key.clone(), value.clone());
        }
    }
    dest.insert("text".to_owned(), json!(text));
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
    /// Thinking taken from `reasoning_details`.
    detail_thinking: String,
    /// Original `reasoning_details` items, merged across stream chunks.
    reasoning_details: Vec<Value>,
    /// Thinking taken from `reasoning_content` or `reasoning`.
    content_thinking: String,
    /// The turn's reasoning arrived as `reasoning_content`, not details.
    from_reasoning_content: bool,
    /// Thinking parsed out of a leading `<think>` wrapper.
    inline_thinking: String,
    text: String,
    /// Splits a leading `<think>` wrapper out of `delta.content`.
    think: ThinkSplitter,
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
        !self.detail_thinking.is_empty()
            || !self.content_thinking.is_empty()
            || !self.inline_thinking.is_empty()
            || !self.reasoning_details.is_empty()
            || self.think.is_holding()
            || !self.text.is_empty()
            || self.tool_calls.iter().any(|call| {
                call.id.is_some() || !call.name.is_empty() || !call.arguments.is_empty()
            })
    }

    fn assemble(&mut self) -> StreamEvent {
        self.finish_think();
        for piece in self.xml.finish() {
            if let crate::xml_tool_calls::XmlPiece::Text(text) = piece {
                self.text.push_str(&text);
            }
        }
        if let Some(detail) = self.interrupt.as_deref() {
            append_interruption(&mut self.text, detail);
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
        let (thinking, replay) = self.thinking_replay();
        let blocks = assemble_blocks_with_replay(&thinking, &self.text, calls, replay.as_deref());
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

    /// Pulls a held `<think>` tail into thinking or text before the terminal
    /// message is built. Events are not emitted; the committed block is the
    /// record the transcript keeps.
    fn finish_think(&mut self) {
        let mut ignored = Vec::new();
        for piece in self.think.finish() {
            self.apply_think_piece(piece, &mut ignored);
        }
    }

    fn apply_think_piece(&mut self, piece: ThinkPiece, events: &mut Vec<StreamEvent>) {
        match piece {
            ThinkPiece::Text(text) => self.absorb_text(&text, events),
            ThinkPiece::Thinking(text) => {
                self.inline_thinking.push_str(&text);
                events.push(StreamEvent::ThinkingDelta(text));
            }
        }
    }

    fn absorb_content(&mut self, text: &str, events: &mut Vec<StreamEvent>) {
        let suppress = !self.reasoning_details.is_empty() || self.from_reasoning_content;
        for piece in self.think.push(text, suppress) {
            self.apply_think_piece(piece, events);
        }
    }

    fn thinking_replay(&self) -> (String, Option<String>) {
        if !self.reasoning_details.is_empty() {
            let text = detail_text(&self.reasoning_details);
            let text = if text.is_empty() {
                self.detail_thinking.clone()
            } else {
                text
            };
            let replay = json!({"reasoning_details": self.reasoning_details}).to_string();
            return (text, Some(replay));
        }
        if self.from_reasoning_content {
            return (
                self.content_thinking.clone(),
                Some(json!({"reasoning_content": true}).to_string()),
            );
        }
        (self.inline_thinking.clone(), None)
    }

    fn absorb_reasoning_content(
        &mut self,
        text: &str,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), &'static str> {
        // Details already carry the thought. Appending `reasoning_content`
        // would show it a second time.
        if text.is_empty() || !self.reasoning_details.is_empty() {
            return Ok(());
        }
        if !charge_stream(&mut self.accumulated, text.len()) {
            return Err("stream exceeded the output limit");
        }
        self.from_reasoning_content = true;
        self.content_thinking.push_str(text);
        events.push(StreamEvent::ThinkingDelta(text.to_owned()));
        Ok(())
    }

    fn absorb_reasoning_details(
        &mut self,
        value: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), &'static str> {
        let incoming = match value {
            Value::Array(items) => items.clone(),
            Value::Object(_) => vec![value.clone()],
            _ => return Ok(()),
        };
        if incoming.is_empty() {
            return Ok(());
        }
        let before = detail_text(&self.reasoning_details);
        let incoming_text = detail_text(&incoming);
        if !self.reasoning_details.is_empty() && incoming_text.starts_with(&before) {
            if incoming_text.len() > before.len() {
                self.reasoning_details = incoming;
            } else if incoming.len() == self.reasoning_details.len() {
                for (dest, src) in self.reasoning_details.iter_mut().zip(incoming) {
                    merge_detail(dest, &src);
                }
            }
        } else if !self.reasoning_details.is_empty() && before.starts_with(&incoming_text) {
            if incoming_text == before && incoming.len() == self.reasoning_details.len() {
                for (dest, src) in self.reasoning_details.iter_mut().zip(incoming) {
                    merge_detail(dest, &src);
                }
            }
        } else {
            for item in incoming {
                if let Some(position) = self
                    .reasoning_details
                    .iter()
                    .position(|existing| same_detail(existing, &item))
                {
                    merge_detail(&mut self.reasoning_details[position], &item);
                } else {
                    self.reasoning_details.push(item);
                }
            }
        }
        let after = detail_text(&self.reasoning_details);
        if after.starts_with(&self.detail_thinking) {
            let delta = after[self.detail_thinking.len()..].to_owned();
            if !delta.is_empty() {
                if !charge_stream(&mut self.accumulated, delta.len()) {
                    return Err("stream exceeded the output limit");
                }
                self.detail_thinking.push_str(&delta);
                events.push(StreamEvent::ThinkingDelta(delta));
            }
        } else if !after.is_empty() {
            self.detail_thinking = after;
        }
        Ok(())
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
            // Reasoning fields first, so a leading `<think>` in the same
            // chunk is dropped instead of shown twice.
            if let Some(details) = delta
                .get("reasoning_details")
                .filter(|value| !value.is_null())
                && let Err(message) = self.absorb_reasoning_details(details, &mut events)
            {
                self.terminal_sent = true;
                return vec![crate::driver::protocol_error(message)];
            }
            if let Some(message) = choice.get("message")
                && let Some(details) = message
                    .get("reasoning_details")
                    .filter(|value| !value.is_null())
                && let Err(message) = self.absorb_reasoning_details(details, &mut events)
            {
                self.terminal_sent = true;
                return vec![crate::driver::protocol_error(message)];
            }
            let reasoning = [
                delta["reasoning_content"].as_str(),
                delta["reasoning"].as_str(),
                choice["message"]["reasoning_content"].as_str(),
            ]
            .into_iter()
            .flatten()
            .find(|text| !text.is_empty());
            if let Some(text) = reasoning
                && let Err(message) = self.absorb_reasoning_content(text, &mut events)
            {
                self.terminal_sent = true;
                return vec![crate::driver::protocol_error(message)];
            }
            if let Some(text) = delta["content"].as_str().filter(|text| !text.is_empty()) {
                if !charge_stream(&mut self.accumulated, text.len()) {
                    self.terminal_sent = true;
                    return vec![crate::driver::protocol_error(
                        "stream exceeded the output limit",
                    )];
                }
                self.absorb_content(text, &mut events);
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
                record_provider_stop(&mut self.stop_reason, &mut self.interrupt, finish);
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
    fn minimax_xml_ask_user_arrives_as_a_typed_tool_call() {
        let mut reducer = CompletionsReducer::new();
        let xml = concat!(
            "<minimax:tool_call>",
            "<invoke name=\"ask_user\">",
            "<parameter name=\"questions\">",
            r#"[{"question":"Which?","choices":["red","blue"],"multiple":true}]"#,
            "</parameter>",
            "</invoke>",
            "</minimax:tool_call>",
        );
        let chunk = serde_json::json!({
            "choices": [{"delta": {"content": xml}, "finish_reason": "stop"}]
        });
        reducer.feed(&chunk.to_string());
        let message = take_done(reducer.feed("[DONE]"));
        let call = message.blocks.iter().find_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        });
        let call = call.expect("xml ask_user becomes a tool call");
        assert_eq!(call.name, "ask_user");
        assert_eq!(
            call.arguments["questions"][0]["choices"],
            serde_json::json!(["red", "blue"])
        );
        assert_eq!(call.arguments["questions"][0]["multiple"], true);
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert!(text_of(&message).is_empty(), "{}", text_of(&message));
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
        assert_eq!(minimax["messages"][0]["content"], "answer");
        assert!(minimax["messages"][0].get("reasoning_content").is_none());
        assert_eq!(
            minimax["messages"][0]["reasoning_details"],
            serde_json::json!([{ "type": "reasoning.text", "text": "why" }])
        );

        let minimax_thinking_only = build_body(
            "MiniMax-M3",
            "https://api.minimaxi.com/v1",
            &Request {
                messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                    blocks: vec![ContentBlock::Thinking(ThinkingBlock::new("why"))],
                    usage: None,
                    stop_reason: StopReason::Stop,
                }))],
                ..Request::default()
            },
        );
        assert_eq!(minimax_thinking_only["messages"][0]["content"], "");
        assert_eq!(
            minimax_thinking_only["messages"][0]["reasoning_details"][0]["text"],
            "why"
        );
    }

    #[test]
    fn minimax_replays_reasoning_details_verbatim_and_stays_stable() {
        let details = serde_json::json!([
            {
                "type": "reasoning.text",
                "id": "reasoning-text-1",
                "format": "MiniMax-response-v1",
                "index": 0,
                "text": "plan the edit exactly"
            }
        ]);
        let replay = serde_json::json!({"reasoning_details": details}).to_string();
        let request = Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(
                        ThinkingBlock::new("plan the edit exactly").with_replay(replay),
                    ),
                    ContentBlock::Text(TextBlock::new("visible answer")),
                ],
                usage: None,
                stop_reason: StopReason::Stop,
            }))],
            reasoning: Some(mycode_core::ReasoningLevel::Max),
            ..Request::default()
        };
        let endpoint = "https://api.minimaxi.com/v1/chat/completions";
        let first = build_body("MiniMax-M3", endpoint, &request);
        let second = build_body("MiniMax-M3", endpoint, &request);
        assert_eq!(first["thinking"]["type"], "adaptive");
        assert_eq!(first["reasoning_split"], true);
        assert!(first.get("reasoning_effort").is_none());
        assert_eq!(first["messages"][0]["content"], "visible answer");
        assert_eq!(first["messages"][0]["reasoning_details"], details);
        assert!(first["messages"][0].get("reasoning_content").is_none());
        assert_eq!(first, second);
    }

    #[test]
    fn minimax_reasoning_content_replays_as_content_not_details() {
        let request = Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(
                        ThinkingBlock::new("hidden").with_replay(r#"{"reasoning_content":true}"#),
                    ),
                    ContentBlock::Text(TextBlock::new("answer")),
                ],
                usage: None,
                stop_reason: StopReason::Stop,
            }))],
            ..Request::default()
        };
        let body = build_body("MiniMax-M3", "https://api.minimax.io/v1", &request);
        assert_eq!(body["messages"][0]["reasoning_content"], "hidden");
        assert!(body["messages"][0].get("reasoning_details").is_none());
        assert_eq!(body["messages"][0]["content"], "answer");
    }

    #[test]
    fn leading_think_tag_becomes_a_thinking_block() {
        let mut reducer = CompletionsReducer::new();
        let deltas = reducer
            .feed(r#"{"choices":[{"delta":{"content":"<think>plan the page</think>\nhello"}}]}"#);
        assert!(deltas.iter().any(|event| matches!(
            event,
            mycode_core::StreamEvent::ThinkingDelta(text) if text == "plan the page"
        )));
        assert!(deltas.iter().any(|event| matches!(
            event,
            mycode_core::StreamEvent::TextDelta(text) if text == "hello"
        )));
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "plan the page");
        assert_eq!(text_of(&message), "hello");
        let replay = message.blocks.iter().find_map(|block| match block {
            ContentBlock::Thinking(thinking) => thinking.replay.clone(),
            _ => None,
        });
        assert!(replay.is_none(), "a parsed tag has no vendor blob");
    }

    #[test]
    fn think_tag_split_across_chunks_stays_out_of_the_reply() {
        let mut reducer = CompletionsReducer::new();
        assert!(
            reducer
                .feed(r#"{"choices":[{"delta":{"content":"<thi"}}]}"#)
                .iter()
                .all(|event| !matches!(event, mycode_core::StreamEvent::TextDelta(_)))
        );
        reducer.feed(r#"{"choices":[{"delta":{"content":"nk>plan</thi"}}]}"#);
        reducer.feed(r#"{"choices":[{"delta":{"content":"nk>\nanswer"}}]}"#);
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "plan");
        assert_eq!(text_of(&message), "answer");
        assert!(!text_of(&message).contains("<think>"));
    }

    #[test]
    fn plain_text_is_not_held_for_a_think_tag() {
        let mut reducer = CompletionsReducer::new();
        let deltas = reducer.feed(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
        assert!(deltas.iter().any(|event| matches!(
            event,
            mycode_core::StreamEvent::TextDelta(text) if text == "hello"
        )));
    }

    #[test]
    fn minimax_reasoning_details_map_to_a_thinking_block() {
        let mut reducer = CompletionsReducer::new();
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {
                    "reasoning_details": [{
                        "type": "reasoning.text",
                        "id": "reasoning-text-1",
                        "format": "MiniMax-response-v1",
                        "index": 0,
                        "text": "plan the edit exactly"
                    }],
                    "content": "<think>duplicate</think>\nvisible answer"
                },
                "finish_reason": "stop"
            }]
        });
        let deltas = reducer.feed(&chunk.to_string());
        assert!(deltas.iter().any(|event| matches!(
            event,
            mycode_core::StreamEvent::ThinkingDelta(text) if text == "plan the edit exactly"
        )));
        assert!(deltas.iter().all(|event| !matches!(
            event,
            mycode_core::StreamEvent::ThinkingDelta(text) if text.contains("duplicate")
        )));
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "plan the edit exactly");
        assert_eq!(text_of(&message), "visible answer");
        let replay = message.blocks.iter().find_map(|block| match block {
            ContentBlock::Thinking(thinking) => thinking.replay.clone(),
            _ => None,
        });
        let replay = replay.expect("details blob");
        let request = Request {
            messages: vec![std::sync::Arc::new(Message::Assistant(message))],
            ..Request::default()
        };
        let body = build_body("MiniMax-M3", "https://api.minimax.io/v1", &request);
        let echoed = &body["messages"][0]["reasoning_details"];
        assert_eq!(echoed[0]["id"], "reasoning-text-1");
        assert_eq!(echoed[0]["format"], "MiniMax-response-v1");
        assert_eq!(echoed[0]["text"], "plan the edit exactly");
        assert_eq!(body["messages"][0]["content"], "visible answer");
        let again = build_body("MiniMax-M3", "https://api.minimax.io/v1", &request);
        assert_eq!(again["messages"][0]["reasoning_details"], *echoed);
        assert!(replay.contains("reasoning-text-1"));
    }

    #[test]
    fn reasoning_details_fragments_with_the_same_id_concatenate() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(
            r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","id":"reasoning-text-1","index":0,"text":"plan"}]}}]}"#,
        );
        reducer.feed(
            r#"{"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","id":"reasoning-text-1","index":0,"text":" the edit"}]}}]}"#,
        );
        reducer.feed(r#"{"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}"#);
        let message = take_done(reducer.feed("[DONE]"));
        assert_eq!(thinking_of(&message), "plan the edit");
        assert_eq!(text_of(&message), "answer");
    }
}
