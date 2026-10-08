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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        AskAnswer, AskQuestion, AskTool, ask_choice_selected, render_ask_answers, toggle_ask_choice,
    };
    use crate::tool::ToolDyn;

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
