//! The `agent` tool: delegate one scoped unit of work to a subagent.
//!
//! The tool serializes the delegation through the host-supplied
//! [`AgentHost`], forwards short progress lines onto its own tool stream,
//! and returns the subagent's final answer as the tool result. The channel
//! seam keeps the tool free of provider and runtime dependencies; hosts run
//! the nested agent, tests replay canned answers. There is no `task` alias.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum characters accepted for the delegated prompt.
pub const MAX_AGENT_PROMPT_CHARS: usize = 16_000;
/// Maximum characters of the subagent answer kept in the tool result.
pub const MAX_AGENT_ANSWER_CHARS: usize = 24_000;
/// First field of live progress lines: `agent|role|phase|detail`.
///
/// Readers do not accept the previous `task|` prefix.
pub const AGENT_PROGRESS_PREFIX: &str = "agent";

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
pub trait AgentHost: Send + Sync + 'static {
    /// Runs one subagent to completion and returns its final answer.
    ///
    /// Implementations must honor `cancel` and report short status lines
    /// through `progress`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when the subagent failed or was
    /// cancelled. A nested run has no wall-clock timeout.
    async fn run_subagent(
        &self,
        request: SubagentRequest,
        progress: &ToolStream,
        cancel: &tokio_util::sync::CancellationToken,
        call_id: &str,
    ) -> Result<String, ToolError>;
}

/// The built-in `agent` tool.
pub struct AgentTool {
    host: Arc<dyn AgentHost>,
}

impl AgentTool {
    /// Binds one delegation host.
    pub fn new(host: Arc<dyn AgentHost>) -> Self {
        Self { host }
    }
}

/// Wire shape of the tool arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AgentArgs {
    /// Catalog role to invoke: `scout`, `artisan`, or a user/project role
    /// from `agents/<name>.md`.
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
impl Tool for AgentTool {
    type Args = AgentArgs;
    type Output = ();

    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Delegate one scoped unit to a listed role. Use `agent` only when \
         the work can run independently in parallel, the brief has clear \
         boundaries, and it will cut cost or improve completion quality. \
         `scout` returns a read-only map and stops. `artisan` implements a \
         bounded change you integrate, one at a time unless the briefs are \
         independent. Skip trivial edits and vague briefs. Independent calls \
         overlap `search_tool` / `use_tool`. Custom roles from agents/*.md \
         are valid. The child cannot ask the user."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "agent: only for independent parallel work with a clear brief that \
             cuts cost or improves quality. One scout (read-only map) or one \
             artisan (bounded change you integrate). Skip trivial edits.",
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
        if prompt.chars().count() > MAX_AGENT_PROMPT_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "prompt exceeds {MAX_AGENT_PROMPT_CHARS} characters"
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
            .take(MAX_AGENT_ANSWER_CHARS)
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{AgentHost, AgentTool, SubagentRequest};
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolDyn, ToolError};

    struct EchoHost;

    #[async_trait]
    impl AgentHost for EchoHost {
        async fn run_subagent(
            &self,
            request: SubagentRequest,
            _progress: &ToolStream,
            _cancel: &tokio_util::sync::CancellationToken,
            _call_id: &str,
        ) -> Result<String, ToolError> {
            Ok(request.prompt)
        }
    }

    #[test]
    fn tool_name_is_agent_with_no_task_alias() {
        let tool = AgentTool::new(Arc::new(EchoHost));
        assert_eq!(tool.name(), "agent");
        let spec = ToolDyn::spec(&tool);
        assert_eq!(spec.name, "agent");
        let schema = spec.params_schema.to_string();
        assert!(!schema.contains("\"task\""));
        let snippet = tool.prompt_snippet().expect("snippet");
        assert!(snippet.starts_with("agent:"));
        assert!(snippet.contains("scout"));
        assert!(snippet.contains("artisan"));
        assert!(!snippet.contains("task"));
        assert!(!snippet.contains("steward"));
        assert!(!snippet.contains("sentinel"));
        assert!(!snippet.contains("exec"));
        assert!(spec.description.contains("`scout`"));
        assert!(spec.description.contains("`artisan`"));
        assert!(!spec.description.contains("`task`"));
        assert!(!spec.description.contains("steward"));
        assert!(!spec.description.contains("sentinel"));
        assert!(!spec.description.contains("`exec`"));
    }
}
