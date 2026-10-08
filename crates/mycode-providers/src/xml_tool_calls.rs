//! Streaming filter for XML `<tool_call>` blocks emitted as plain text.
//!
//! Some OpenAI/Anthropic-compatible endpoints (MiniMax M2, StepFun step-3.5,
//! self-hosted Qwen) never map tool calling onto native `tool_use` deltas and
//! instead stream the model's raw tool-call markup inside the text channel.
//! This parser intercepts that markup mid-stream so the reducers can surface
//! real tool calls instead of leaking XML into the visible reply.
//!
//! Payload shapes accepted inside one tool-call block:
//!
//! * JSON — `{"name": "read", "arguments": {"path": "a.rs"}}`
//!   (also `parameters`/`params` for the `arguments` key)
//! * Qwen-style XML — `<function=read><parameter=path>src/a.rs</parameter></function>`
//! * MiniMax XML — `<invoke name="read"><parameter name="path">src/a.rs</parameter></invoke>`
//!   inside `<minimax:tool_call>` or a plain `<tool_call>`
//!
//! A parameter value that is a JSON array or object is decoded, and repeated
//! parameter tags become one array. That keeps list and object arguments
//! (such as `ask_user` `choices`) in the shape the tool schema expects.
//! Plain text stays a string.

/// Cap on one buffered tool-call body; oversized bodies fall back to text.
const MAX_CALL_BODY_CHARS: usize = 32 * 1024;

const OPEN_TOOL: &str = "<tool_call>";
const CLOSE_TOOL: &str = "</tool_call>";
const OPEN_MINIMAX: &str = "<minimax:tool_call>";
const CLOSE_MINIMAX: &str = "</minimax:tool_call>";

/// Which opening tag the parser is currently inside.
#[derive(Clone, Copy)]
enum CallTag {
    Tool,
    Minimax,
}

impl CallTag {
    fn open(self) -> &'static str {
        match self {
            Self::Tool => OPEN_TOOL,
            Self::Minimax => OPEN_MINIMAX,
        }
    }

    fn close(self) -> &'static str {
        match self {
            Self::Tool => CLOSE_TOOL,
            Self::Minimax => CLOSE_MINIMAX,
        }
    }
}

/// One parsed piece of a streamed text channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XmlPiece {
    /// Visible text that is not part of a tool call.
    Text(String),
    /// One complete tool call: tool name and its JSON-encoded arguments.
    ToolCall { name: String, arguments: String },
}

/// Incremental tool-call extractor over a text stream.
#[derive(Default)]
pub(crate) struct XmlToolCallParser {
    /// Text not yet safe to classify: a possible partial tag or an open call.
    buffer: String,
    /// The opening tag currently being buffered, when one has started.
    open: Option<CallTag>,
}

impl XmlToolCallParser {
    /// Feeds one text delta and returns the pieces that are now final.
    pub(crate) fn feed(&mut self, text: &str) -> Vec<XmlPiece> {
        self.buffer.push_str(text);
        let mut pieces = Vec::new();
        loop {
            if let Some(open) = self.open {
                let close = open.close();
                let Some(end) = self.buffer.find(close) else {
                    if self.buffer.chars().count() > MAX_CALL_BODY_CHARS {
                        // A runaway body without a closing tag degrades to
                        // visible text instead of unbounded buffering.
                        let raw = std::mem::take(&mut self.buffer);
                        self.open = None;
                        pieces.push(XmlPiece::Text(format!("{}{raw}", open.open())));
                    }
                    break;
                };
                let body = self.buffer[..end].to_owned();
                self.buffer.replace_range(..end + close.len(), "");
                self.open = None;
                match parse_call_body(&body) {
                    Some(calls) => {
                        for (name, arguments) in calls {
                            pieces.push(XmlPiece::ToolCall { name, arguments });
                        }
                    }
                    // Unparseable bodies surface as text; swallowing model
                    // output silently would hide real content.
                    None => pieces.push(XmlPiece::Text(format!(
                        "{}{body}{}",
                        open.open(),
                        open.close()
                    ))),
                }
            } else {
                let Some((start, tag)) = find_open(&self.buffer) else {
                    let text = flush_safe_text(&mut self.buffer);
                    if !text.is_empty() {
                        pieces.push(XmlPiece::Text(text));
                    }
                    break;
                };
                if start > 0 {
                    pieces.push(XmlPiece::Text(self.buffer[..start].to_owned()));
                    self.buffer.replace_range(..start, "");
                }
                self.buffer.replace_range(..tag.open().len(), "");
                self.open = Some(tag);
            }
        }
        pieces
    }

    /// Flushes at end-of-stream. Trailing text is emitted. An unterminated
    /// call is not a call, but the buffered markup is still model output, so
    /// it surfaces as text instead of disappearing from the assembled message.
    pub(crate) fn finish(&mut self) -> Vec<XmlPiece> {
        let open = self.open.take();
        let rest = std::mem::take(&mut self.buffer);
        if rest.is_empty() {
            Vec::new()
        } else if let Some(open) = open {
            vec![XmlPiece::Text(format!("{}{rest}", open.open()))]
        } else {
            vec![XmlPiece::Text(rest)]
        }
    }
}

/// The earliest `<tool_call>` or `<minimax:tool_call>` in `buffer`.
fn find_open(buffer: &str) -> Option<(usize, CallTag)> {
    let tool = buffer.find(OPEN_TOOL);
    let minimax = buffer.find(OPEN_MINIMAX);
    match (tool, minimax) {
        (Some(tool_at), Some(minimax_at)) if minimax_at < tool_at => {
            Some((minimax_at, CallTag::Minimax))
        }
        (Some(tool_at), _) => Some((tool_at, CallTag::Tool)),
        (None, Some(minimax_at)) => Some((minimax_at, CallTag::Minimax)),
        (None, None) => None,
    }
}

/// Emits text that cannot be part of a future opening tag, keeping a
/// possible partial-tag suffix buffered for the next delta. Only a suffix
/// ending at the buffer's end can still grow into an opening tag, and every
/// prefix of those tags starts with `<`, so the decision hangs on the last `<`.
fn flush_safe_text(buffer: &mut String) -> String {
    let Some(position) = buffer.rfind('<') else {
        return std::mem::take(buffer);
    };
    let tail = &buffer[position..];
    if holds_partial_open(tail) {
        let drained = buffer[..position].to_owned();
        buffer.replace_range(..position, "");
        drained
    } else {
        std::mem::take(buffer)
    }
}

/// Whether `tail` can still grow into `<tool_call>` or `<minimax:tool_call>`.
fn holds_partial_open(tail: &str) -> bool {
    (OPEN_TOOL.starts_with(tail) && tail.len() < OPEN_TOOL.len())
        || (OPEN_MINIMAX.starts_with(tail) && tail.len() < OPEN_MINIMAX.len())
}

/// Parses one tool-call body into one or more (name, JSON arguments) pairs.
fn parse_call_body(body: &str) -> Option<Vec<(String, String)>> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        let (name, arguments) = parse_json_body(trimmed)?;
        return Some(vec![(name, arguments)]);
    }
    if trimmed.contains("<function") || trimmed.contains("<invoke") {
        return parse_xml_calls(trimmed);
    }
    None
}

/// Parses `{"name": "...", "arguments": ...}` (and the `parameters` alias).
fn parse_json_body(body: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let name = value
        .get("name")
        .or_else(|| value.get("tool"))
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    let arguments = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .or_else(|| value.get("params"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Some((name, unwrap_json_container(arguments).to_string()))
}

/// Parses every `<function>` / `<invoke>` in one tool-call body.
fn parse_xml_calls(body: &str) -> Option<Vec<(String, String)>> {
    let mut calls = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let rest = &body[cursor..];
        let invoke = rest.find("<invoke");
        let function = rest.find("<function");
        let next = match (invoke, function) {
            (Some(invoke_at), Some(function_at)) if invoke_at <= function_at => invoke_at,
            (Some(_), Some(function_at)) => function_at,
            (Some(invoke_at), None) => invoke_at,
            (None, Some(function_at)) => function_at,
            (None, None) => break,
        };
        let (name, arguments, consumed) = parse_one_call(body, cursor + next)?;
        if name.is_empty() {
            return None;
        }
        calls.push((name, arguments));
        cursor = consumed;
    }
    if calls.is_empty() { None } else { Some(calls) }
}

/// Parses one `<function…>` or `<invoke…>` starting at `start`.
///
/// Returns the tool name, JSON arguments, and the index just past the close tag.
fn parse_one_call(body: &str, start: usize) -> Option<(String, String, usize)> {
    let header_end = body[start..].find('>')? + start;
    let header = &body[start + 1..header_end];
    let (kind, name) = if let Some(rest) = tag_rest(header, "function") {
        ("function", function_name(rest)?)
    } else {
        let rest = tag_rest(header, "invoke")?;
        ("invoke", attr_value(rest, "name")?)
    };
    let close = format!("</{kind}>");
    let content_start = header_end + 1;
    let close_at = body[content_start..].find(&close)? + content_start;
    let arguments = parameters_object(&body[content_start..close_at])?;
    Some((
        name,
        serde_json::Value::Object(arguments).to_string(),
        close_at + close.len(),
    ))
}

/// The header remainder after `name`, when `name` is the tag and not a prefix
/// of a longer tag (`function` must not match `functionality`).
fn tag_rest<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let rest = header.strip_prefix(name)?;
    if rest.is_empty() || rest.starts_with(['=', ' ', '\t', '\n', '\r', '/', '>']) {
        Some(rest)
    } else {
        None
    }
}

fn function_name(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if let Some(rest) = rest.strip_prefix('=') {
        let name = rest.trim().trim_matches(['"', '\'']);
        if name.is_empty() {
            None
        } else {
            Some(name.to_owned())
        }
    } else {
        attr_value(rest, "name")
    }
}

fn attr_value(header: &str, attr: &str) -> Option<String> {
    let key = format!("{attr}=");
    let pos = header.find(&key)?;
    let after = header[pos + key.len()..].trim_start();
    if let Some(rest) = after.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].trim().to_owned())
    } else if let Some(rest) = after.strip_prefix('\'') {
        let end = rest.find('\'')?;
        Some(rest[..end].trim().to_owned())
    } else {
        let end = after
            .find(|ch: char| ch.is_whitespace() || ch == '/' || ch == '>')
            .unwrap_or(after.len());
        let name = after[..end].trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_owned())
        }
    }
}

/// Collects `<parameter=key>` and `<parameter name="key">` pairs.
///
/// A repeated key becomes a JSON array so a choices list written as several
/// tags is not collapsed to the last value.
fn parameters_object(region: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut map = serde_json::Map::new();
    let mut cursor = 0;
    while let Some(offset) = region[cursor..].find("<parameter") {
        let absolute = cursor + offset;
        let header_end = region[absolute..].find('>')? + absolute;
        let header = &region[absolute + "<parameter".len()..header_end];
        let key = parameter_key(header)?;
        let value_start = header_end + 1;
        let value_end = region[value_start..].find("</parameter>")? + value_start;
        let value = coerce_xml_value(&region[value_start..value_end]);
        insert_param(&mut map, key, value);
        cursor = value_end + "</parameter>".len();
    }
    Some(map)
}

fn parameter_key(header: &str) -> Option<String> {
    let header = header.trim();
    if let Some(rest) = header.strip_prefix('=') {
        let key = rest.trim().trim_matches(['"', '\'']);
        if key.is_empty() {
            None
        } else {
            Some(key.to_owned())
        }
    } else {
        attr_value(header, "name")
    }
}

fn insert_param(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: String,
    value: serde_json::Value,
) {
    match map.remove(&key) {
        None => {
            map.insert(key, value);
        }
        Some(serde_json::Value::Array(mut items)) => {
            items.push(value);
            map.insert(key, serde_json::Value::Array(items));
        }
        Some(existing) => {
            map.insert(key, serde_json::Value::Array(vec![existing, value]));
        }
    }
}

/// Decodes a parameter body. JSON arrays and objects keep their types; a
/// sequence of child elements becomes an array or an object; other text
/// stays a string.
fn coerce_xml_value(raw: &str) -> serde_json::Value {
    coerce_xml_value_at(raw, 0)
}

fn coerce_xml_value_at(raw: &str, depth: usize) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::Value::String(String::new());
    }
    if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return value;
    }
    if depth < 8
        && trimmed.starts_with('<')
        && !trimmed.starts_with("</")
        && let Some(value) = coerce_xml_fragment(trimmed, depth)
    {
        return value;
    }
    serde_json::Value::String(trimmed.to_owned())
}

/// A JSON string that itself holds an array or object becomes that value.
fn unwrap_json_container(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(text) = &value else {
        return value;
    };
    let trimmed = text.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return parsed;
    }
    value
}

fn coerce_xml_fragment(text: &str, depth: usize) -> Option<serde_json::Value> {
    let items = scan_elements(text, depth)?;
    Some(elements_to_value(items))
}

fn scan_elements(text: &str, depth: usize) -> Option<Vec<(String, serde_json::Value)>> {
    let mut items = Vec::new();
    let mut rest = text.trim_start();
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let (name, value, consumed) = parse_element(rest, depth)?;
        items.push((name, value));
        rest = rest[consumed..].trim_start();
    }
    Some(items)
}

fn parse_element(text: &str, depth: usize) -> Option<(String, serde_json::Value, usize)> {
    if !text.starts_with('<') || text.starts_with("</") {
        return None;
    }
    let name_end = text[1..].find(|ch: char| !is_name_char(ch))? + 1;
    if name_end == 1 {
        return None;
    }
    let name = text[1..name_end].to_owned();
    let header_end = text[name_end..].find('>')? + name_end;
    let header = &text[name_end..header_end];
    if header.trim_end().ends_with('/') {
        return Some((
            name,
            serde_json::Value::String(String::new()),
            header_end + 1,
        ));
    }
    let close = format!("</{name}>");
    let content_start = header_end + 1;
    let close_at = text[content_start..].find(&close)? + content_start;
    let inner = coerce_xml_value_at(&text[content_start..close_at], depth + 1);
    Some((name, inner, close_at + close.len()))
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' || ch == '-'
}

fn elements_to_value(items: Vec<(String, serde_json::Value)>) -> serde_json::Value {
    let same_name = items.iter().all(|(name, _)| name == &items[0].0);
    if same_name && is_list_tag(&items[0].0) {
        return serde_json::Value::Array(items.into_iter().map(|(_, value)| value).collect());
    }
    if items.iter().any(|(name, _)| name == "question") {
        return group_questions(items);
    }
    if same_name {
        return serde_json::Value::Array(items.into_iter().map(|(_, value)| value).collect());
    }
    let mut map = serde_json::Map::new();
    for (name, value) in items {
        insert_param(&mut map, name, value);
    }
    serde_json::Value::Object(map)
}

fn is_list_tag(name: &str) -> bool {
    matches!(
        name,
        "choice" | "choices" | "item" | "option" | "value" | "string"
    )
}

/// Groups child elements into one question object, or an array when several
/// `<question>` tags appear.
fn group_questions(items: Vec<(String, serde_json::Value)>) -> serde_json::Value {
    let mut groups: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
    for (name, value) in items {
        if name == "question" {
            if let Some(last) = groups.last_mut()
                && !last.contains_key("question")
            {
                last.insert("question".to_owned(), stringish(value));
            } else {
                let mut map = serde_json::Map::new();
                map.insert("question".to_owned(), stringish(value));
                groups.push(map);
            }
            continue;
        }
        if groups.is_empty() {
            groups.push(serde_json::Map::new());
        }
        if let Some(last) = groups.last_mut() {
            insert_param(last, name, value);
        }
    }
    match groups.len() {
        0 => serde_json::Value::Object(serde_json::Map::new()),
        1 => serde_json::Value::Object(groups.pop().expect("one question group")),
        _ => serde_json::Value::Array(groups.into_iter().map(serde_json::Value::Object).collect()),
    }
}

fn stringish(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(text.trim().to_owned()),
        serde_json::Value::Number(number) => serde_json::Value::String(number.to_string()),
        serde_json::Value::Bool(flag) => serde_json::Value::String(flag.to_string()),
        serde_json::Value::Null => serde_json::Value::String(String::new()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{XmlPiece, XmlToolCallParser};

    fn parse(text: &str) -> Vec<XmlPiece> {
        let mut parser = XmlToolCallParser::default();
        let mut pieces = parser.feed(text);
        pieces.extend(parser.finish());
        pieces
    }

    fn arguments(piece: &XmlPiece) -> serde_json::Value {
        let XmlPiece::ToolCall { arguments, .. } = piece else {
            panic!("expected a tool call, got {piece:?}");
        };
        serde_json::from_str(arguments).unwrap_or_else(|error| panic!("{error}: {arguments}"))
    }

    #[test]
    fn function_xml_keeps_plain_string_arguments() {
        let pieces = parse(
            "<tool_call><function=read><parameter=path>src/a.rs</parameter></function></tool_call>",
        );
        assert_eq!(pieces.len(), 1);
        let XmlPiece::ToolCall { name, .. } = &pieces[0] else {
            panic!("tool call");
        };
        assert_eq!(name, "read");
        assert_eq!(arguments(&pieces[0])["path"], "src/a.rs");
    }

    #[test]
    fn function_xml_decodes_json_choice_lists() {
        let pieces = parse(
            r#"<tool_call>
<function=ask_user>
<parameter=questions>[{"question":"Which?","choices":["red","blue"],"multiple":true}]</parameter>
</function>
</tool_call>"#,
        );
        let args = arguments(&pieces[0]);
        assert!(args["questions"].is_array(), "{args}");
        assert_eq!(args["questions"][0]["choices"], json!(["red", "blue"]));
        assert_eq!(args["questions"][0]["multiple"], true);
    }

    #[test]
    fn minimax_invoke_decodes_ask_user_choices_and_multiple() {
        let xml = concat!(
            "Say which.\n",
            "<minimax:tool_call>",
            "<invoke name=\"ask_user\">",
            "<parameter name=\"questions\">",
            r#"[{"question":"Which?","choices":["red","blue"],"multi_select":true}]"#,
            "</parameter>",
            "</invoke>",
            "</minimax:tool_call>",
        );
        let mut parser = XmlToolCallParser::default();
        let mut pieces = Vec::new();
        for chunk in xml
            .char_indices()
            .step_by(7)
            .map(|(index, _)| index)
            .chain(std::iter::once(xml.len()))
            .collect::<Vec<_>>()
            .windows(2)
        {
            pieces.extend(parser.feed(&xml[chunk[0]..chunk[1]]));
        }
        pieces.extend(parser.finish());
        let visible: String = pieces
            .iter()
            .filter_map(|piece| match piece {
                XmlPiece::Text(text) => Some(text.as_str()),
                XmlPiece::ToolCall { .. } => None,
            })
            .collect();
        assert!(visible.contains("Say which."), "{pieces:?}");
        let call = pieces
            .iter()
            .find(|piece| matches!(piece, XmlPiece::ToolCall { .. }))
            .expect("call");
        let XmlPiece::ToolCall { name, .. } = call else {
            unreachable!();
        };
        assert_eq!(name, "ask_user");
        let args = arguments(call);
        assert_eq!(args["questions"][0]["choices"], json!(["red", "blue"]));
        assert_eq!(args["questions"][0]["multi_select"], true);
    }

    #[test]
    fn repeated_and_nested_choice_tags_become_arrays() {
        let repeated = parse(
            r#"<tool_call>
<function=ask_user>
<parameter=question>Which?</parameter>
<parameter=choices>red</parameter>
<parameter=choices>blue</parameter>
<parameter name="multiple">true</parameter>
</function>
</tool_call>"#,
        );
        let args = arguments(&repeated[0]);
        assert_eq!(args["question"], "Which?");
        assert_eq!(args["choices"], json!(["red", "blue"]));
        assert_eq!(args["multiple"], "true");

        let nested = parse(
            r#"<minimax:tool_call>
<invoke name="ask_user">
<parameter name="questions">
<question>Which?</question>
<choices>red</choices>
<choices>blue</choices>
<multiple>true</multiple>
</parameter>
</invoke>
</minimax:tool_call>"#,
        );
        let args = arguments(&nested[0]);
        assert_eq!(args["questions"]["question"], "Which?");
        assert_eq!(args["questions"]["choices"], json!(["red", "blue"]));
        assert_eq!(args["questions"]["multiple"], "true");
    }

    #[test]
    fn single_choice_text_stays_a_string_until_ask_normalization() {
        let pieces = parse(
            r#"<tool_call><function=ask_user><parameter=question>Which?</parameter><parameter=choices>red</parameter></function></tool_call>"#,
        );
        let args = arguments(&pieces[0]);
        assert_eq!(args["choices"], "red");
        assert!(args.get("multiple").is_none());
    }

    #[test]
    fn two_invokes_become_two_calls() {
        let pieces = parse(
            r#"<minimax:tool_call>
<invoke name="read"><parameter name="path">a.rs</parameter></invoke>
<invoke name="read"><parameter name="path">b.rs</parameter></invoke>
</minimax:tool_call>"#,
        );
        assert_eq!(pieces.len(), 2);
        assert_eq!(arguments(&pieces[0])["path"], "a.rs");
        assert_eq!(arguments(&pieces[1])["path"], "b.rs");
    }

    #[test]
    fn json_tool_call_body_still_parses() {
        let pieces = parse(
            r#"<tool_call>{"name":"ask_user","arguments":{"questions":[{"question":"Which?","choices":["red"],"multiple":false}]}}</tool_call>"#,
        );
        let args = arguments(&pieces[0]);
        assert_eq!(args["questions"][0]["choices"], json!(["red"]));
        assert_eq!(args["questions"][0]["multiple"], false);
    }
}
