//! The `agent` tool: delegate one scoped unit of work to a subagent.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tools::ctx::ToolCtx;
use crate::tools::stream::ToolStream;
use crate::tools::tool::{Tool, ToolError, ToolResult};

pub(crate) mod activity;
pub use activity::{AgentActivity, inherit_agent_activity, settle_agent_activity};
mod control;
mod redact;
mod runtime;
mod scope;
mod state;
pub use control::{AgentMessageTool, AgentStatusTool, AgentStopTool};
pub use runtime::AgentRuntime;
pub use scope::{AgentScope, AgentSymbolScope};
pub use state::*;
#[cfg(test)]
mod runtime_tests;

/// Maximum characters accepted for the delegated prompt.
pub const MAX_AGENT_PROMPT_CHARS: usize = 16_000;
/// Maximum characters of the subagent answer kept in the tool result.
pub const MAX_AGENT_ANSWER_CHARS: usize = 24_000;
/// First field of live progress lines: `agent|role|phase|detail`.
pub const AGENT_PROGRESS_PREFIX: &str = "agent";
/// `step` detail a subagent reports after compacting its own history.
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
pub trait AgentHost: Send + Sync + 'static {
    /// Resolve an enabled role before allocating a run; return its write capability.
    fn validate_role<'a>(
        &'a self,
        role: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ToolError>> + Send + 'a>> {
        Box::pin(async move { Ok(!matches!(role, "scout" | "sentinel")) })
    }
    /// Validate a retained worktree without allocating or modifying a checkout.
    fn validate_reopen<'a>(
        &'a self,
        _path: &'a std::path::Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), ToolError>> + Send + 'a>> {
        Box::pin(async {
            Err(ToolError::Execution(
                "this host cannot safely reopen retained worktrees".into(),
            ))
        })
    }
    /// Run a new summarized task in the exact retained worktree. Never fall back to HEAD.
    fn run_subagent_reopened<'a>(
        &'a self,
        _request: SubagentRequest,
        _progress: &'a ToolStream,
        _cancel: &'a tokio_util::sync::CancellationToken,
        _call_id: &'a str,
        _path: &'a std::path::Path,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async {
            Err(ToolError::Execution(
                "this host cannot safely reopen retained worktrees".into(),
            ))
        })
    }
    /// Runs with the write capability used to admit this run and reserve its scope.
    fn run_subagent_validated<'a>(
        &'a self,
        request: SubagentRequest,
        progress: &'a ToolStream,
        cancel: &'a tokio_util::sync::CancellationToken,
        call_id: &'a str,
        _writer: bool,
        retained: Option<&'a std::path::Path>,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        match retained {
            Some(path) => self.run_subagent_reopened(request, progress, cancel, call_id, path),
            None => self.run_subagent(request, progress, cancel, call_id),
        }
    }
    /// Queue steering using the host's real child identity.
    fn steer_subagent(&self, _call_id: &str, _message: &str) -> Result<(), ToolError> {
        Err(ToolError::Execution(
            "this host does not support live steering".into(),
        ))
    }
    /// Remove known credentials before retaining output or publishing snapshots.
    fn redact_text(&self, text: &str) -> String {
        redact::redact_text(text)
    }
    /// Runs one subagent to completion and returns its final answer.
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
    runtime: Arc<AgentRuntime>,
}

impl AgentTool {
    /// Binds one delegation host.
    pub fn new(host: Arc<dyn AgentHost>) -> Self {
        Self {
            host,
            runtime: Arc::new(AgentRuntime::new(
                std::env::temp_dir().join(format!("mycode-agent-{}", uuid::Uuid::new_v4())),
            )),
        }
    }
    /// Reuse the session registry across turns and control tools.
    pub fn with_runtime(mut self, runtime: Arc<AgentRuntime>) -> Self {
        self.runtime = runtime;
        self
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
    /// Stable coordination phase. Duplicate retained phases are rejected.
    #[serde(default)]
    pub phase_id: Option<String>,
    /// Exact path/symbol ownership claims; coordination only, not a sandbox.
    #[serde(default)]
    pub scope: Option<AgentScope>,
}

impl Tool for AgentTool {
    type Args = AgentArgs;
    type Output = ();

    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Delegate one self-contained unit of work to a role listed in the          system prompt's <dispatch> section and get back its answer. Only          those roles are enabled. Calls are serialized and wait for their own result;          call() inside run_code is also synchronous. The brief must stand alone: include          the goal, paths, constraints, supplied evidence and done condition. The child          cannot see this conversation or ask the user."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "agent: hand one self-contained brief to a role from <dispatch>              (research, implementation, or review) and wait for its answer. Calls are serialized;              a CodeMode yield is progress, not completion.",
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
            .map(|description| description.chars().take(240).collect())
            .unwrap_or_else(|| brief_label(&prompt));
        let request = SubagentRequest {
            agent,
            prompt,
            description,
            isolation: args.isolation,
        };
        let run = self
            .runtime
            .run(
                self.host.clone(),
                request,
                runtime::RunOptions {
                    phase_id: args.phase_id,
                    scope: args.scope,
                    ..runtime::RunOptions::default()
                },
                out,
                ctx,
            )
            .await?;
        let result = if run.status == AgentRunStatus::Completed {
            ToolResult::text(run.result.clone().unwrap_or_default())
        } else {
            ToolResult::error(
                run.error
                    .clone()
                    .unwrap_or_else(|| "subagent failed".into()),
            )
        };
        Ok(result.with_details(serde_json::json!({"agent_states": [AgentStateEvent::Run(run)]})))
    }
}

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
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use super::{AgentHost, AgentTool, SubagentRequest};
    use crate::tools::stream::ToolStream;
    use crate::tools::tool::{Tool, ToolDyn, ToolError};

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
        assert!(!Tool::parallel_safe(&tool));
        let spec = ToolDyn::spec(&tool);
        assert_eq!(spec.name, "agent");
        assert!(!spec.params_schema.to_string().contains("\"task\""));
    }
}
