//! The `task` tool: delegate one scoped unit of work to a subagent.
//!
//! The tool serializes the delegation through the host-supplied
//! [`TaskHost`], forwards short progress lines onto its own tool stream,
//! and returns the subagent's final answer as the tool result. The channel
//! seam keeps the tool free of provider and runtime dependencies; hosts run
//! the nested agent, tests replay canned answers.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum characters accepted for the delegated prompt.
pub const MAX_TASK_PROMPT_CHARS: usize = 16_000;
/// Maximum characters of the subagent answer kept in the tool result.
pub const MAX_TASK_ANSWER_CHARS: usize = 24_000;

/// One delegated unit of work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRequest {
    /// Role name from the catalog (`scout`, `artisan`, …).
    pub agent: String,
    /// The full task brief handed to the subagent.
    pub prompt: String,
    /// Short human-facing label for progress lines.
    pub description: String,
    /// Isolation override: `shared`, `worktree`, or absent (role default).
    pub isolation: Option<String>,
}

/// Host side of the delegation.
#[async_trait]
pub trait TaskHost: Send + Sync + 'static {
    /// Runs one subagent to completion and returns its final answer.
    ///
    /// Implementations must honor `cancel` and report short status lines
    /// through `progress`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when the subagent failed, timed out,
    /// or was cancelled.
    async fn run_subagent(
        &self,
        request: SubagentRequest,
        progress: &ToolStream,
        cancel: &tokio_util::sync::CancellationToken,
        call_id: &str,
    ) -> Result<String, ToolError>;
}

/// The built-in `task` tool.
pub struct TaskTool {
    host: Arc<dyn TaskHost>,
}

impl TaskTool {
    /// Binds one delegation host.
    pub fn new(host: Arc<dyn TaskHost>) -> Self {
        Self { host }
    }
}

/// Wire shape of the tool arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskArgs {
    /// Catalog role to invoke: `scout`, `artisan`, `steward`, `sentinel`,
    /// or a user/project role from `agents/<name>.md`.
    pub agent: String,
    /// Complete, self-contained instructions for the subagent. Include the
    /// goal, relevant paths, and the expected form of the answer.
    pub prompt: String,
    /// One-line label shown while the subagent runs.
    #[serde(default)]
    pub description: Option<String>,
    /// Isolation override: `shared` uses the session checkout; `worktree`
    /// creates a detached lease. Omit to use the role default. Worktree
    /// applies only to write-capable roles.
    #[serde(default)]
    pub isolation: Option<String>,
}

#[async_trait]
impl Tool for TaskTool {
    type Args = TaskArgs;
    type Output = ();

    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate one scoped unit of work to a named subagent role. \
         Independent `task` calls in the same response run at the same time, \
         and they overlap `search_tool` / `use_tool` emitted in that same response. \
         `scout` is read-only reconnaissance; `artisan` makes the primary \
         change; `steward` does residual cleanup; `sentinel` reviews a \
         finished diff. Custom roles from agents/*.md are also valid. \
         The child has no parent conversation and cannot ask the user."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "task: dispatch a listed role with a self-contained brief. \
             Independent task calls in one response run together, and they \
             overlap search_tool / use_tool from that same response.",
        )
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let agent = args.agent.trim().to_owned();
        if agent.is_empty() {
            return Err(ToolError::InvalidArgs("agent is required".into()));
        }
        if let Some(isolation) = args.isolation.as_deref()
            && !matches!(isolation, "shared" | "worktree")
        {
            return Err(ToolError::InvalidArgs(
                "isolation must be shared or worktree".into(),
            ));
        }
        let prompt = args.prompt.trim().to_owned();
        if prompt.is_empty() {
            return Err(ToolError::InvalidArgs("prompt is required".into()));
        }
        if prompt.chars().count() > MAX_TASK_PROMPT_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "prompt exceeds {MAX_TASK_PROMPT_CHARS} characters"
            )));
        }
        let description = args.description.unwrap_or_else(|| brief_label(&prompt));
        let request = SubagentRequest {
            agent,
            prompt,
            description,
            isolation: args.isolation,
        };
        let answer = self
            .host
            .run_subagent(request, out, &ctx.cancel, &ctx.call_id)
            .await?;
        let answer = answer
            .chars()
            .take(MAX_TASK_ANSWER_CHARS)
            .collect::<String>();
        Ok(ToolResult::text(answer))
    }
}

/// Short progress label derived from a prompt's first line.
fn brief_label(prompt: &str) -> String {
    let first = prompt.lines().next().unwrap_or("").trim();
    let chars: Vec<char> = first.chars().collect();
    if chars.len() <= 60 {
        first.to_owned()
    } else {
        chars[..60].iter().collect()
    }
}
