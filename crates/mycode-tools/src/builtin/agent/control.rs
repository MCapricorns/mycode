//! Query, stop, steer and explicitly summarized continuation tools.
use super::{
    AgentHost, AgentRuntime,
    runtime::{RunOptions, clip, error},
    state::*,
};
use crate::{
    ctx::ToolCtx,
    stream::ToolStream,
    tool::{Tool, ToolError, ToolResult},
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentStatusArgs {
    /// Stable run/workflow id or actual host call_id; omit to list this session.
    pub id: Option<String>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentStopArgs {
    pub id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageArgs {
    pub id: String,
    pub message: String,
    /// Only summary mode is supported for completed runs. Full-session mode errors.
    #[serde(default = "summary_mode")]
    pub mode: String,
}
fn summary_mode() -> String {
    "summary".into()
}
pub struct AgentStatusTool {
    runtime: Arc<AgentRuntime>,
}
pub struct AgentStopTool {
    runtime: Arc<AgentRuntime>,
}
pub struct AgentMessageTool {
    host: Arc<dyn AgentHost>,
    runtime: Arc<AgentRuntime>,
}
impl AgentStatusTool {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self { runtime }
    }
}
impl AgentStopTool {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self { runtime }
    }
}
impl AgentMessageTool {
    pub fn new(host: Arc<dyn AgentHost>, runtime: Arc<AgentRuntime>) -> Self {
        Self { host, runtime }
    }
}
pub(super) fn snapshot_result(
    runtime: &AgentRuntime,
    id: Option<&str>,
    out: &ToolStream,
) -> Result<ToolResult, ToolError> {
    let events = runtime.snapshots(id)?;
    for e in &events {
        publish(out, e.clone());
    }
    let text = serde_json::to_string(&events).map_err(|e| error(e.to_string()))?;
    Ok(ToolResult::text(text).with_details(serde_json::json!({"agent_states":events})))
}
impl Tool for AgentStatusTool {
    type Args = AgentStatusArgs;
    type Output = ();
    fn name(&self) -> &str {
        "agent_status"
    }
    fn description(&self) -> &str {
        "Query retained agent runs and workflows in this session, including completed results, errors and full-output artifact paths. Takes stable id or actual host call_id; omit id to list bounded retained records."
    }
    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        snapshot_result(&self.runtime, args.id.as_deref(), out)
    }
}
impl Tool for AgentStopTool {
    type Args = AgentStopArgs;
    type Output = ();
    fn name(&self) -> &str {
        "agent_stop"
    }
    fn description(&self) -> &str {
        "Request cancellation of a run or workflow by stable id (or run call_id). Stopping runs retain scope until the child and worktree handoff settle. Stopped runs cannot resume. Use agent_status to observe the terminal state."
    }
    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        self.runtime.stop(&args.id, out)?;
        snapshot_result(&self.runtime, Some(&args.id), out)
    }
}
impl Tool for AgentMessageTool {
    type Args = AgentMessageArgs;
    type Output = ();
    fn name(&self) -> &str {
        "agent_message"
    }
    fn description(&self) -> &str {
        "Steer a running agent at its next loop boundary. For a completed standalone run, start and synchronously await a NEW run with explicit retained summary context and artifact references. Writer continuation safely reopens the exact retained worktree and preserves dirty/untracked/ignored files; missing or unsupported retained worktrees fail without a HEAD fallback. This does not resume the original session or automatically integrate worktrees. Only mode=summary is supported. Cancelled, failed, settling and workflow-owned runs cannot be continued; workflow producers use explicit agent_workflow revise."
    }
    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if args.mode != "summary" {
            return Err(error(
                "full-session continuation is unsupported; only explicit summary mode is available",
            ));
        }
        let message = args.message.trim();
        if message.is_empty() || message.chars().count() > 8000 {
            return Err(error("message must contain 1-8000 characters"));
        }
        let (snapshot, request, options, active_host) = {
            let state = self.runtime.state.lock();
            let id = super::runtime::resolve_run_id(&state, &args.id)?;
            let r = state.runs.get(&id).expect("resolved run retained");
            if !r.snapshot.status.is_terminal() && r.cancel.is_cancelled() {
                return Err(error(
                    "run cancellation is settling; steering is no longer accepted",
                ));
            }
            (
                r.snapshot.clone(),
                r.request.clone(),
                r.options.clone(),
                r.host.clone(),
            )
        };
        if matches!(
            snapshot.status,
            AgentRunStatus::Queued | AgentRunStatus::Running
        ) {
            let host = active_host.ok_or_else(|| error("run already settled"))?;
            host.steer_subagent(&snapshot.call_id, message)?;
            return Ok(ToolResult::text(
                "Steering queued for the child's next loop boundary; delivery does not cancel its in-flight response.",
            ));
        }
        if snapshot.status != AgentRunStatus::Completed {
            return Err(error(
                "only completed runs can continue; stopped/failed/settling runs cannot resume",
            ));
        }
        if snapshot.workflow_id.is_some() {
            return Err(error(
                "workflow nodes cannot continue independently; use the workflow's explicit control action",
            ));
        }
        let prompt=format!("Continue a completed task as a NEW run using summarized context only. Original role: {}. Previous worktree/path: {}. Full previous result artifact: {}. Previous edits have not been automatically integrated; inspect actual files and retained worktree before relying on them.

Original brief:
{}

Retained result summary:
{}

New instruction:
{}",snapshot.role,snapshot.path,snapshot.artifact_path.as_deref().unwrap_or("none"),clip(&request.prompt,3000),clip(snapshot.result.as_deref().unwrap_or(""),4000),message);
        let run = self
            .runtime
            .run(
                self.host.clone(),
                super::SubagentRequest {
                    prompt,
                    description: format!("Continue {}", snapshot.description),
                    ..request
                },
                RunOptions {
                    scope: options.scope,
                    previous_run_id: Some(snapshot.id.clone()),
                    ..RunOptions::default()
                },
                out,
                ctx,
            )
            .await?;
        let mut result = if run.status == AgentRunStatus::Completed {
            ToolResult::text(run.result.clone().unwrap_or_default())
        } else {
            ToolResult::error(
                run.error
                    .clone()
                    .unwrap_or_else(|| "continuation failed".into()),
            )
        };
        result.details = Some(serde_json::json!({"agent_states":[AgentStateEvent::Run(run)]}));
        Ok(result)
    }
}
