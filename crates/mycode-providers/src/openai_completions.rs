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
    merge_usage, provider_error_detail, record_provider_stop, tool_parameters, usage_from_value,
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
            let replay = crate::family::reasoning_replay(model, endpoint);
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
    /// Wire `index` → position in `tool_calls`. Native calls get their own
    /// slots, so they never land on a call the XML filter already added.
    tool_slots: std::collections::HashMap<u64, usize>,
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
                    // A retained detail item is stream state and counts
                    // toward the accumulation ceiling like any other
                    // fragment, so distinct-id frames cannot grow the
                    // array without bound.
                    if !charge_stream(&mut self.accumulated, item.to_string().len()) {
                        return Err("stream exceeded the output limit");
                    }
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
        // A missing index means a gateway that streams one call at a time.
        let wire_index = fragment["index"].as_u64().unwrap_or_default();
        if wire_index > MAX_STREAM_INDEX {
            return Err("tool call index exceeds the stream limit");
        }
        let fresh_id = fragment["id"].as_str().filter(|id| !id.is_empty());
        let slot = self.tool_slots.get(&wire_index).copied();
        // A different id at an index that already holds a call is a new
        // call. Some servers stream every parallel call at index 0 (or with
        // no index), each under its own id; merging them glued two argument
        // objects into one invalid call.
        let reuse =
            slot.filter(
                |position| match (self.tool_calls[*position].id.as_deref(), fresh_id) {
                    (Some(known), Some(id)) => known == id,
                    _ => true,
                },
            );
        let index = match reuse {
            Some(position) => position,
            None => {
                if self.tool_calls.len() > MAX_STREAM_INDEX as usize {
                    return Err("tool call index exceeds the stream limit");
                }
                self.tool_calls.push(ToolCallAccumulator::default());
                let position = self.tool_calls.len() - 1;
                self.tool_slots.insert(wire_index, position);
                position
            }
        };
        if let Some(id) = fresh_id
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
        if let Some(name) = fragment["function"]["name"].as_str()
            && !name.is_empty()
        {
            // Gateways that resend the whole name on every fragment would
            // otherwise build `readread…`; a name that grows from the
            // current one replaces it, and only a new piece is appended.
            if name != self.tool_calls[index].name.as_str() {
                if !charge_stream(&mut self.accumulated, name.len()) {
                    return Err("stream exceeded the output limit");
                }
                let call = &mut self.tool_calls[index];
                if !call.name.is_empty() && name.starts_with(call.name.as_str()) {
                    call.name = name.to_owned();
                } else {
                    call.name.push_str(name);
                }
            }
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
mod tests;
