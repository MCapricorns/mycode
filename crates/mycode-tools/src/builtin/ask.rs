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
             until the user answers.",
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
        let mut rendered = String::new();
        for (index, answer) in answers.iter().enumerate() {
            if index > 0 {
                rendered.push('\n');
            }
            if answer.answer.is_empty() {
                rendered.push_str(&format!("{}. (skipped)", index + 1));
            } else {
                let bounded: String = answer.answer.chars().take(MAX_ANSWER_CHARS).collect();
                rendered.push_str(&format!("{}. {}", index + 1, bounded));
            }
        }
        Ok(ToolResult::text(rendered))
    }
}

/// Fails the ask when the user dismisses it.
pub fn user_dismissed() -> ToolError {
    ToolError::Execution("the user dismissed the questions".into())
}
