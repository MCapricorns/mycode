//! The `ask_user` tool: structured agent-to-user questions.
//!
//! The tool serializes 1..=4 typed questions through the host-supplied
//! [`AskChannel`], awaits the user's answers cancel-safely, and returns them
//! to the model as the tool result. The channel seam keeps the tool free of
//! any UI dependency; hosts answer, tests replay.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum questions per ask.
pub const MAX_QUESTIONS: usize = 4;
/// Maximum characters accepted per answer.
pub const MAX_ANSWER_CHARS: usize = 4 * 1024;

/// One question the agent asks the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AskQuestion {
    /// Question headline.
    pub question: String,
    /// Optional candidate answers; free text is always allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Whether the user may skip this question.
    #[serde(default)]
    pub optional: bool,
    /// When true, the user may select more than one choice.
    ///
    /// Accepted aliases cover the names models actually emit.
    #[serde(
        default,
        alias = "multi_select",
        alias = "allow_multiple",
        alias = "multiSelect"
    )]
    #[schemars(
        default,
        description = "When true, the user may select more than one choice. Return every selected value."
    )]
    pub multiple: bool,
}

/// Whether `choice` is one of the newline-joined selections in `answer`.
#[must_use]
pub fn ask_choice_selected(answer: &str, choice: &str) -> bool {
    answer.split('\n').any(|item| item == choice)
}

/// Toggles one choice. Single-select replaces the answer. Multi-select adds
/// or removes that line and never collapses the others.
#[must_use]
pub fn toggle_ask_choice(answer: &str, choice: &str, multiple: bool) -> String {
    if !multiple {
        return choice.to_owned();
    }
    let mut items: Vec<&str> = answer.split('\n').filter(|item| !item.is_empty()).collect();
    if let Some(index) = items.iter().position(|item| *item == choice) {
        items.remove(index);
    } else {
        items.push(choice);
    }
    items.join("\n")
}

/// One answered question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    /// The question headline this answer belongs to.
    pub question: String,
    /// The chosen or typed answer; empty means skipped.
    pub answer: String,
}

/// Host side of the ask interaction.
#[async_trait]
pub trait AskChannel: Send + Sync + 'static {
    /// Presents the questions and waits for the user.
    ///
    /// Implementations must honor `cancel`: firing it resolves the wait
    /// without leaking the pending interaction.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when the user dismissed the ask or
    /// the channel failed.
    async fn ask(
        &self,
        questions: &[AskQuestion],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<AskAnswer>, ToolError>;
}

/// The built-in `ask_user` tool.
pub struct AskTool {
    channel: Arc<dyn AskChannel>,
}

impl AskTool {
    /// Binds one answering channel.
    pub fn new(channel: Arc<dyn AskChannel>) -> Self {
        Self { channel }
    }
}

/// Wire shape of the tool arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// 1..=4 questions for the user.
    pub questions: Vec<AskQuestion>,
}

#[async_trait]
impl Tool for AskTool {
    type Args = AskArgs;
    type Output = ();

    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user 1-4 clarifying questions and wait for their answers. \
         Use when a decision needs human input before proceeding."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "ask_user: batch every clarification into one call; the turn pauses \
             until the user answers. Set multiple on a question when more than \
             one choice may be selected.",
        )
    }

    fn prepare_args(&self, args: serde_json::Value) -> serde_json::Value {
        normalize_ask_args(args)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if args.questions.is_empty() || args.questions.len() > MAX_QUESTIONS {
            return Err(ToolError::InvalidArgs(format!(
                "1..={MAX_QUESTIONS} questions required"
            )));
        }
        for question in &args.questions {
            if question.question.trim().is_empty() {
                return Err(ToolError::InvalidArgs("question text is required".into()));
            }
        }
        let answers = self.channel.ask(&args.questions, &ctx.cancel).await?;
        Ok(ToolResult::text(render_ask_answers(&answers)))
    }
}

/// Renders one answer per question. A multi-select answer keeps each
/// selected line under the same number.
pub fn render_ask_answers(answers: &[AskAnswer]) -> String {
    let mut rendered = String::new();
    for (index, answer) in answers.iter().enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        let number = index + 1;
        if answer.answer.is_empty() {
            rendered.push_str(&format!("{number}. (skipped)"));
            continue;
        }
        let bounded: String = answer.answer.chars().take(MAX_ANSWER_CHARS).collect();
        let mut lines = bounded.lines();
        if let Some(first) = lines.next() {
            rendered.push_str(&format!("{number}. {first}"));
            for line in lines {
                rendered.push('\n');
                rendered.push_str(line);
            }
        }
    }
    rendered
}

/// Fails the ask when the user dismisses it.
pub fn user_dismissed() -> ToolError {
    ToolError::Execution("the user dismissed the questions".into())
}

/// Turns XML tool-call argument shapes into the `ask_user` schema.
///
/// Native JSON calls already match the schema and stay equivalent. XML
/// parsers (and some gateways) leave `questions` / `choices` as strings and
/// spell the multi-select flag as `multi_select` or the text `true`. Those
/// forms fail schema validation before the ask card can show its chips.
pub(crate) fn normalize_ask_args(args: serde_json::Value) -> serde_json::Value {
    let value = unwrap_json_string(args);
    if let serde_json::Value::Array(items) = value {
        return questions_object(normalize_questions(serde_json::Value::Array(items)));
    }
    let serde_json::Value::Object(mut map) = value else {
        return value;
    };
    if let Some(questions) = map.remove("questions") {
        let mut items = question_items(unwrap_json_string(questions));
        attach_siblings(&mut map, &mut items);
        return questions_object(normalize_questions(serde_json::Value::Array(items)));
    }
    if map.contains_key("question") {
        let mut item = serde_json::Map::new();
        if let Some(question) = map.remove("question") {
            item.insert("question".to_owned(), question);
        }
        lift_question_fields(&mut map, &mut item);
        return questions_object(normalize_questions(serde_json::Value::Array(vec![
            serde_json::Value::Object(item),
        ])));
    }
    serde_json::Value::Object(map)
}

fn questions_object(questions: serde_json::Value) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("questions".to_owned(), questions);
    serde_json::Value::Object(map)
}

fn question_items(value: serde_json::Value) -> Vec<serde_json::Value> {
    match value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => vec![serde_json::Value::Object(map)],
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            vec![serde_json::json!({ "question": text.trim() })]
        }
        _ => Vec::new(),
    }
}

fn attach_siblings(
    map: &mut serde_json::Map<String, serde_json::Value>,
    items: &mut [serde_json::Value],
) {
    let choices = take_first(map, &["choices", "options", "choice"]);
    let optional = take_first(map, &["optional"]);
    let multiple = take_first(map, &["multiple", "multi_select", "allow_multiple"]);
    if choices.is_none() && optional.is_none() && multiple.is_none() {
        return;
    }
    for item in items.iter_mut() {
        let serde_json::Value::Object(question) = item else {
            continue;
        };
        if let Some(choices) = &choices {
            question
                .entry("choices".to_owned())
                .or_insert_with(|| choices.clone());
        }
        if let Some(optional) = &optional {
            question
                .entry("optional".to_owned())
                .or_insert_with(|| optional.clone());
        }
        if let Some(multiple) = &multiple {
            question
                .entry("multiple".to_owned())
                .or_insert_with(|| multiple.clone());
        }
    }
}

fn lift_question_fields(
    map: &mut serde_json::Map<String, serde_json::Value>,
    item: &mut serde_json::Map<String, serde_json::Value>,
) {
    for key in [
        "choices",
        "options",
        "choice",
        "optional",
        "multiple",
        "multi_select",
        "allow_multiple",
    ] {
        if let Some(value) = map.remove(key) {
            item.entry(key.to_owned()).or_insert(value);
        }
    }
}

fn normalize_questions(value: serde_json::Value) -> serde_json::Value {
    let items = question_items(unwrap_json_string(value));
    serde_json::Value::Array(items.into_iter().filter_map(normalize_question).collect())
}

fn normalize_question(value: serde_json::Value) -> Option<serde_json::Value> {
    let value = unwrap_json_string(value);
    let mut map = match value {
        serde_json::Value::Object(map) => map,
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            let mut map = serde_json::Map::new();
            map.insert("question".to_owned(), serde_json::Value::String(text));
            map
        }
        _ => return None,
    };
    let question = take_first(&mut map, &["question", "text", "prompt"])?;
    let question = json_string(question);
    if question.trim().is_empty() {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert(
        "question".to_owned(),
        serde_json::Value::String(question.trim().to_owned()),
    );
    if let Some(choices) = take_first(&mut map, &["choices", "options", "choice"]) {
        let choices = coerce_choices(unwrap_json_string(choices));
        if let serde_json::Value::Array(items) = &choices
            && !items.is_empty()
        {
            out.insert("choices".to_owned(), choices);
        }
    }
    if let Some(flag) = take_first(&mut map, &["multiple", "multi_select", "allow_multiple"])
        && let Some(flag) = coerce_bool(&flag)
    {
        out.insert("multiple".to_owned(), serde_json::Value::Bool(flag));
    }
    if let Some(flag) = take_first(&mut map, &["optional"])
        && let Some(flag) = coerce_bool(&flag)
    {
        out.insert("optional".to_owned(), serde_json::Value::Bool(flag));
    }
    Some(serde_json::Value::Object(out))
}

fn coerce_choices(value: serde_json::Value) -> serde_json::Value {
    match unwrap_json_string(value) {
        serde_json::Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                push_choice(&mut out, unwrap_json_string(item));
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::String(text) => choices_from_text(&text),
        other => {
            let mut out = Vec::new();
            push_choice(&mut out, other);
            serde_json::Value::Array(out)
        }
    }
}

fn choices_from_text(text: &str) -> serde_json::Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Array(Vec::new());
    }
    if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return coerce_choices(parsed);
    }
    if trimmed.contains('\n') {
        let items = trimmed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::Value::String(line.to_owned()))
            .collect();
        return serde_json::Value::Array(items);
    }
    // A comma stays inside one label. Lists are JSON, repeated tags, or lines.
    serde_json::Value::Array(vec![serde_json::Value::String(trimmed.to_owned())])
}

fn push_choice(out: &mut Vec<serde_json::Value>, value: serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                push_choice(out, unwrap_json_string(item));
            }
        }
        serde_json::Value::Object(map) => {
            // MiniMax XML decodes a choice list as one field, e.g.
            // `{"item":["chips","cookies"]}`. Any single-field object is that
            // wrapper: an array value is the list, a string value is one chip.
            if map.len() == 1 {
                if let Some(inner) = map.into_values().next() {
                    push_choice(out, unwrap_json_string(inner));
                }
                return;
            }
            if let Some(text) = ["text", "label", "value", "name"]
                .into_iter()
                .find_map(|key| map.get(key).cloned())
            {
                push_choice(out, text);
            }
        }
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            if trimmed.contains('\n') {
                for line in trimmed.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        out.push(serde_json::Value::String(line.to_owned()));
                    }
                }
            } else {
                out.push(serde_json::Value::String(trimmed.to_owned()));
            }
        }
        serde_json::Value::Number(number) => {
            out.push(serde_json::Value::String(number.to_string()));
        }
        serde_json::Value::Bool(flag) => out.push(serde_json::Value::String(flag.to_string())),
        serde_json::Value::Null => {}
    }
}

fn unwrap_json_string(value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(text) = &value else {
        return value;
    };
    let trimmed = text.trim();
    if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return parsed;
    }
    value
}

fn take_first(
    map: &mut serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<serde_json::Value> {
    let mut found = None;
    for key in keys {
        if let Some(value) = map.remove(*key)
            && found.is_none()
        {
            found = Some(value);
        }
    }
    found
}

fn json_string(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text,
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn coerce_bool(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(flag) => Some(*flag),
        serde_json::Value::Number(number) => number.as_i64().map(|number| number != 0),
        serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AskAnswer, AskArgs, AskQuestion, AskTool, ask_choice_selected, normalize_ask_args,
        render_ask_answers, toggle_ask_choice,
    };
    use crate::tool::{ToolDyn, normalize_tool_args, validate_args};

    struct Unused;

    #[async_trait::async_trait]
    impl super::AskChannel for Unused {
        async fn ask(
            &self,
            _questions: &[AskQuestion],
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Vec<AskAnswer>, crate::tool::ToolError> {
            Err(crate::tool::ToolError::Execution("unused".into()))
        }
    }

    #[test]
    fn multiple_aliases_round_trip_into_the_rendered_answer() {
        for key in ["multiple", "multi_select", "allow_multiple", "multiSelect"] {
            let parsed: AskQuestion = serde_json::from_value(json!({
                "question": "which",
                "choices": ["red", "blue", "green"],
                key: true
            }))
            .unwrap_or_else(|error| panic!("{key}: {error}"));
            assert!(parsed.multiple, "{key}");
        }
        let single: AskQuestion = serde_json::from_value(json!({
            "question": "which",
            "choices": ["red"]
        }))
        .expect("optional multiple");
        assert!(!single.multiple);

        let mut picked = String::new();
        picked = toggle_ask_choice(&picked, "red", true);
        picked = toggle_ask_choice(&picked, "blue", true);
        assert!(ask_choice_selected(&picked, "red"));
        assert!(ask_choice_selected(&picked, "blue"));
        assert!(!ask_choice_selected(&picked, "green"));
        picked = toggle_ask_choice(&picked, "red", false);
        assert_eq!(picked, "red");

        let rendered = render_ask_answers(&[AskAnswer {
            question: "which".into(),
            answer: "red\nblue".into(),
        }]);
        assert!(rendered.contains("red"), "{rendered}");
        assert!(rendered.contains("blue"), "{rendered}");
    }

    fn accept_ask(args: serde_json::Value) -> Vec<AskQuestion> {
        let args = normalize_ask_args(normalize_tool_args(args));
        validate_args::<AskArgs>(&args).unwrap_or_else(|error| panic!("{error}: {args}"));
        serde_json::from_value::<AskArgs>(args)
            .expect("schema-valid ask args deserialize")
            .questions
    }

    #[test]
    fn xml_shapes_match_the_json_multi_select_path() {
        let json_path = accept_ask(json!({
            "questions": [{
                "question": "Which?",
                "choices": ["red", "blue", "green"],
                "multiple": true
            }]
        }));
        let shapes = [
            json!({
                "questions": "[{\"question\":\"Which?\",\"choices\":[\"red\",\"blue\",\"green\"],\"multiple\":true}]"
            }),
            json!({
                "questions": [{
                    "question": "Which?",
                    "choices": "[\"red\", \"blue\", \"green\"]",
                    "multiple": "true"
                }]
            }),
            json!({
                "questions": [{
                    "question": "Which?",
                    "choices": ["red", "blue", "green"],
                    "multi_select": true
                }]
            }),
            json!({
                "questions": [{
                    "question": "Which?",
                    "choices": "red\nblue\ngreen",
                    "allow_multiple": "yes"
                }]
            }),
            json!({
                "question": "Which?",
                "choices": ["red", "blue", "green"],
                "multiSelect": true
            }),
            json!({
                "questions": {
                    "question": "Which?",
                    "choices": ["red", "blue", "green"],
                    "multiple": "true"
                }
            }),
        ];
        for shape in shapes {
            let parsed = accept_ask(shape.clone());
            assert_eq!(parsed, json_path, "{shape}");
        }

        let mut picked = String::new();
        picked = toggle_ask_choice(&picked, "red", json_path[0].multiple);
        picked = toggle_ask_choice(&picked, "blue", json_path[0].multiple);
        assert_eq!(picked, "red\nblue");
        let rendered = render_ask_answers(&[AskAnswer {
            question: json_path[0].question.clone(),
            answer: picked,
        }]);
        assert!(rendered.contains("red"), "{rendered}");
        assert!(rendered.contains("blue"), "{rendered}");
    }

    #[test]
    fn xml_single_select_keeps_choice_chips() {
        let raw = json!({
            "questions": [{
                "question": "Which?",
                "choices": "red"
            }]
        });
        assert!(
            validate_args::<AskArgs>(&raw).is_err(),
            "a string choice list must not pass the schema unchanged"
        );
        let parsed = accept_ask(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].choices, vec!["red".to_owned()]);
        assert!(!parsed[0].multiple);

        let flat = accept_ask(json!({
            "question": "Which?",
            "choices": "red\nblue"
        }));
        assert_eq!(flat[0].choices, vec!["red".to_owned(), "blue".to_owned()]);
        assert!(!flat[0].multiple);
        assert_eq!(toggle_ask_choice("", "red", flat[0].multiple), "red");

        let labeled = accept_ask(json!({
            "question": "Which?",
            "choices": "Yes, proceed"
        }));
        assert_eq!(labeled[0].choices, vec!["Yes, proceed".to_owned()]);
        assert!(!labeled[0].multiple);
    }

    #[test]
    fn wrapped_choice_objects_become_string_chips() {
        let snacks = ["chips", "cookies", "fruit", "candy"];
        let json_path = accept_ask(json!({
            "questions": [{
                "question": "Pick snacks",
                "choices": snacks,
                "multiple": true
            }]
        }));
        let raw = json!({
            "questions": [{
                "question": "Pick snacks",
                "choices": {"item": snacks},
                "multiple": "true"
            }]
        });
        assert!(
            validate_args::<AskArgs>(&raw).is_err(),
            "a wrapped choice object must not pass the schema unchanged"
        );
        let shapes = [
            raw,
            json!({
                "questions": [{
                    "question": "Pick snacks",
                    "choices": {"choice": snacks},
                    "multiple": "true"
                }]
            }),
            json!({
                "questions": [{
                    "question": "Pick snacks",
                    "choices": {"snacks": snacks},
                    "multiple": true
                }]
            }),
            json!({
                "question": "Pick snacks",
                "choices": "{\"item\":[\"chips\",\"cookies\",\"fruit\",\"candy\"]}",
                "multiple": "true"
            }),
        ];
        for shape in shapes {
            let parsed = accept_ask(shape.clone());
            assert_eq!(parsed, json_path, "{shape}");
            assert_eq!(parsed[0].choices, snacks);
            assert!(parsed[0].multiple, "{shape}");
        }
        let mut picked = String::new();
        for choice in &json_path[0].choices {
            picked = toggle_ask_choice(&picked, choice, true);
        }
        assert_eq!(picked, "chips\ncookies\nfruit\ncandy");
        let rendered = render_ask_answers(&[AskAnswer {
            question: json_path[0].question.clone(),
            answer: picked,
        }]);
        for snack in snacks {
            assert!(rendered.contains(snack), "{rendered}");
        }
    }

    #[test]
    fn schema_advertises_multiple_as_optional() {
        let tool = AskTool::new(std::sync::Arc::new(Unused));
        let schema = ToolDyn::spec(&tool).params_schema;
        let text = schema.to_string();
        assert!(text.contains("multiple"), "{text}");
        let question = schema
            .pointer("/$defs/AskQuestion/required")
            .or_else(|| schema.pointer("/definitions/AskQuestion/required"))
            .and_then(|value| value.as_array())
            .expect("AskQuestion required");
        assert!(
            question.iter().all(|item| item != "multiple"),
            "{question:?}"
        );
    }
}
