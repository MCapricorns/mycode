//! Query, stop, steer and explicitly summarized continuation tools.
use super::{
    AgentHost, AgentRuntime,
    runtime::{RunOptions, clip, error},
    state::*,
};
use crate::tools::ctx::ToolCtx;
use crate::tools::stream::ToolStream;
use crate::tools::tool::{Tool, ToolError, ToolResult};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentStatusArgs {
    /// Stable run id or actual host call_id; omit to list this session.
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
        "Report retained agent runs of this session: results, errors, and full-output artifact \
         paths. Pass a run id or call_id, or omit `id` to list recent runs."
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
        "Cancel a run by id or call_id. A stopping run keeps its scope until the child and its \
         worktree handoff settle; stopped runs cannot resume. Use agent_status to see the final \
         state."
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
        "Steer a running agent with `message`; it reads the message before its next step. For a \
         finished run, start and wait for a new run that continues from the retained summary and \
         artifacts; a writer continues in its retained worktree. Cancelled, failed, and settling \
         runs cannot be continued."
    }
    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
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
        let prompt = format!(
            "Continue a completed task as a NEW run using summarized context only. Original role: {}. Previous worktree/path: {}. Full previous result artifact: {}. Previous edits have not been automatically integrated; inspect actual files and retained worktree before relying on them.\n\nOriginal brief:\n{}\n\nRetained result summary:\n{}\n\nNew instruction:\n{}",
            snapshot.role,
            snapshot.path,
            snapshot.artifact_path.as_deref().unwrap_or("none"),
            clip(&request.prompt, 3000),
            clip(snapshot.result.as_deref().unwrap_or(""), 4000),
            message
        );
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
