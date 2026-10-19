//! The `agent` tool: delegate one scoped unit of work to a subagent.
//!
//! The tool serializes the delegation through the host-supplied
//! [`AgentHost`], forwards short progress lines onto its own tool stream,
//! and returns the subagent's final answer as the tool result. The channel
//! seam keeps the tool free of provider and runtime dependencies; hosts run
//! the nested agent, tests replay canned answers. There is no `task` alias.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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
/// `step` detail a subagent reports after compacting its own history; the
/// frontend localizes it like the other step tokens.
pub const AGENT_COMPACTED_STEP: &str = "compacted";

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
///
/// The method returns a boxed future so the trait stays object-safe for
/// `dyn AgentHost` dispatch; implementers write `Box::pin(async move { … })`.
pub trait AgentHost: Send + Sync + 'static {
    /// Runs one subagent to completion and returns its final answer.
    ///
    /// Implementations must honor `cancel` and report short status lines
    /// through `progress`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when the subagent failed or was
    /// cancelled.
    fn run_subagent<'a>(
        &'a self,
        request: SubagentRequest,
        progress: &'a ToolStream,
        cancel: &'a tokio_util::sync::CancellationToken,
        call_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>>;
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
    /// Role to invoke: one of the enabled roles the system prompt's
    /// `<dispatch>` section lists.
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

impl Tool for AgentTool {
    type Args = AgentArgs;
    type Output = ();

    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Delegate one self-contained unit of work to a role listed in the \
         system prompt's <dispatch> section and get back its answer. Only \
         those roles are enabled. Independent calls in one response run \
         together, and beside the other calls in that response. The brief \
         must stand alone: the child cannot see this conversation or ask the \
         user."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "agent: hand one self-contained brief to a role from <dispatch> \
             (research or a bounded change) and get its answer. Several \
             independent calls in one response run together.",
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
        let description = args
            .description
            .as_deref()
            .map(str::trim)
            .filter(|desc| !desc.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| brief_label(&prompt));
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
        let mut answer = answer;
        if let Some((cut, _)) = answer.char_indices().nth(MAX_AGENT_ANSWER_CHARS) {
            answer.truncate(cut);
            answer.push_str("\n[answer truncated: the subagent replied with more text]");
        }
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

    use std::future::Future;
    use std::pin::Pin;

    use super::{AgentHost, AgentTool, SubagentRequest};
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolDyn, ToolError};

    struct EchoHost;

    impl AgentHost for EchoHost {
        fn run_subagent<'a>(
            &'a self,
            request: SubagentRequest,
            _progress: &'a ToolStream,
            _cancel: &'a tokio_util::sync::CancellationToken,
            _call_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
            Box::pin(async move { Ok(request.prompt) })
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
        // Role names live in the dispatch section, which lists only the
        // enabled roles; the tool copy must not advertise a disabled one.
        assert!(snippet.contains("<dispatch>"));
        assert!(!snippet.contains("scout"));
        assert!(!snippet.contains("artisan"));
        assert!(!snippet.contains("task"));
        assert!(!snippet.contains("steward"));
        assert!(!snippet.contains("sentinel"));
        assert!(!snippet.contains("exec"));
        assert!(spec.description.contains("<dispatch>"));
        assert!(!spec.description.contains("scout"));
        assert!(!spec.description.contains("artisan"));
        assert!(!spec.description.contains("`task`"));
        assert!(!spec.description.contains("steward"));
        assert!(!spec.description.contains("sentinel"));
        assert!(!spec.description.contains("`exec`"));
    }
}
