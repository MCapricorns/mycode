//! Session-owned lifecycle state. Synchronous callers await a settling driver.
use super::{AgentHost, AgentScope, MAX_AGENT_ANSWER_CHARS, SubagentRequest, state::*};
use crate::{
    stream::{ToolStream, ToolStreamItem},
    tool::ToolError,
};
use futures_util::FutureExt;
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

pub const MAX_RETAINED_AGENT_RUNS: usize = 128;
pub(super) const MAX_WORKFLOWS: usize = 32;

#[derive(Clone, Default)]
pub(super) struct RunOptions {
    pub phase_id: Option<String>,
    pub scope: Option<AgentScope>,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub previous_run_id: Option<String>,
}
pub(super) struct RunRecord {
    pub snapshot: AgentRunSnapshot,
    pub request: SubagentRequest,
    pub options: RunOptions,
    pub writer: bool,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub host: Option<Arc<dyn AgentHost>>,
}
#[derive(Default)]
pub(super) struct RuntimeState {
    pub runs: BTreeMap<String, RunRecord>,
    pub workflows: BTreeMap<String, super::workflow::WorkflowRecord>,
}
/// One session's bounded run/workflow registry, reused across turns.
/// Artifact files contain sanitized result text only, never host configuration.
pub struct AgentRuntime {
    pub(super) state: Mutex<RuntimeState>,
    pub(super) artifacts: PathBuf,
    slots: std::sync::OnceLock<Arc<tokio::sync::Semaphore>>,
}
impl AgentRuntime {
    pub fn new(artifact_dir: PathBuf) -> Self {
        Self {
            state: Mutex::new(RuntimeState::default()),
            artifacts: artifact_dir,
            slots: std::sync::OnceLock::new(),
        }
    }
    /// The session-wide host concurrency budget, also shared by yielded cells.
    pub fn slots(&self, limit: usize) -> Arc<tokio::sync::Semaphore> {
        self.slots
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(limit.max(1))))
            .clone()
    }

    pub(super) fn snapshots(&self, id: Option<&str>) -> Result<Vec<AgentStateEvent>, ToolError> {
        let mut state = self.state.lock();
        refresh_idle_workflows(&mut state);
        let mut events = Vec::new();
        for r in state.runs.values() {
            if id.is_none_or(|id| {
                id == r.snapshot.id
                    || id == r.snapshot.call_id
                    || r.snapshot.workflow_id.as_deref() == Some(id)
            }) {
                events.push(AgentStateEvent::Run(r.snapshot.clone()));
            }
        }
        for w in state.workflows.values() {
            if id.is_none_or(|id| id == w.snapshot.id) {
                events.push(AgentStateEvent::Workflow(w.snapshot.clone()));
            }
        }
        if id.is_some() && events.is_empty() {
            return Err(error("unknown or expired run/workflow in this session"));
        }
        Ok(events)
    }
    pub(super) fn update_run(
        &self,
        id: &str,
        stream: &ToolStream,
        update: impl FnOnce(&mut AgentRunSnapshot),
    ) {
        let (snapshot, workflow) = {
            let mut state = self.state.lock();
            let Some(run) = state.runs.get_mut(id) else {
                return;
            };
            update(&mut run.snapshot);
            let snapshot = run.snapshot.clone();
            let workflow = snapshot
                .workflow_id
                .as_ref()
                .and_then(|wid| state.workflows.get_mut(wid))
                .map(|w| {
                    if let Some(node) = w
                        .snapshot
                        .nodes
                        .iter_mut()
                        .find(|n| Some(&n.id) == snapshot.node_id.as_ref())
                    {
                        node.status = snapshot.status;
                    }
                    w.snapshot.clone()
                });
            (snapshot, workflow)
        };
        publish(stream, AgentStateEvent::Run(snapshot));
        if let Some(workflow) = workflow {
            publish(stream, AgentStateEvent::Workflow(workflow));
        }
    }
    pub(super) fn stop(
        &self,
        id: &str,
        stream: &ToolStream,
    ) -> Result<Vec<AgentStateEvent>, ToolError> {
        {
            let mut state = self.state.lock();
            if let Some(workflow) = state.workflows.get_mut(id) {
                workflow.cancel.cancel();
                // An idle handoff has no driver to settle it.
                if workflow.snapshot.status == AgentWorkflowStatus::WaitingIntegration {
                    workflow.snapshot.status = AgentWorkflowStatus::Cancelled;
                    for node in &mut workflow.snapshot.nodes {
                        if !node.status.is_terminal() {
                            node.status = AgentRunStatus::Cancelled;
                        }
                    }
                }
                publish(stream, AgentStateEvent::Workflow(workflow.snapshot.clone()));
            } else {
                let run_id = resolve_run_id(&state, id)?;
                let run = state.runs.get_mut(&run_id).expect("resolved run retained");
                if !run.snapshot.status.is_terminal() {
                    run.cancel.cancel();
                    run.snapshot.status = AgentRunStatus::Settling;
                    run.snapshot.step = "Stopping; waiting for child and worktree handoff".into();
                    publish(stream, AgentStateEvent::Run(run.snapshot.clone()));
                }
            }
        }
        self.snapshots(Some(id))
    }
    pub(super) async fn run(
        self: &Arc<Self>,
        host: Arc<dyn AgentHost>,
        request: SubagentRequest,
        mut options: RunOptions,
        progress: &ToolStream,
        ctx: &crate::ctx::ToolCtx,
    ) -> Result<AgentRunSnapshot, ToolError> {
        let (cancel, call_id, cwd) = (&ctx.cancel, ctx.call_id.as_str(), ctx.cwd.as_path());
        super::scope::validate_phase(options.phase_id.as_deref())?;
        if request.prompt.trim().is_empty()
            || request.prompt.chars().count() > super::MAX_AGENT_PROMPT_CHARS
        {
            return Err(error("run prompt must contain 1-16000 characters"));
        }
        let writer = host.validate_role(&request.agent).await?;
        let reopen_path = if let Some(source_id) = &options.previous_run_id {
            {
                let state = self.state.lock();
                let source = state
                    .runs
                    .get(source_id)
                    .ok_or_else(|| error("previous run expired; no rework was started"))?;
                if !matches!(
                    source.snapshot.status,
                    AgentRunStatus::Completed | AgentRunStatus::Failed
                ) {
                    return Err(error("previous run must be settled and not cancelled"));
                }
                if source.cwd != cwd || source.request.agent != request.agent {
                    return Err(error("rework must retain the original parent cwd and role"));
                }
            }
            if let Some(path) = self.retained_worktree(source_id)? {
                let path = tokio::fs::canonicalize(path).await.map_err(|e| {
                    error(format!(
                        "retained worktree is unavailable; no HEAD fallback: {e}"
                    ))
                })?;
                host.validate_reopen(&path).await?;
                Some(path)
            } else {
                None
            }
        } else {
            None
        };
        if cancel.is_cancelled() {
            return Err(error("agent cancelled before admission"));
        }
        options.scope = options
            .scope
            .as_ref()
            .map(|s| s.normalize(cwd))
            .transpose()?;
        let id = format!("run-{}", uuid::Uuid::new_v4());
        let call_id = if call_id.is_empty() {
            id.clone()
        } else {
            call_id.to_owned()
        };
        let child_cancel = cancel.child_token();
        let snapshot = AgentRunSnapshot {
            id: id.clone(),
            call_id: call_id.clone(),
            role: request.agent.clone(),
            description: clip(&sanitize_output(host.as_ref(), &request.description), 240),
            prompt: sanitize_output(host.as_ref(), &request.prompt),
            status: AgentRunStatus::Queued,
            step: "queued".into(),
            path: reopen_path.as_deref().unwrap_or(cwd).display().to_string(),
            started_ms: now_ms(),
            finished_ms: None,
            result: None,
            error: None,
            artifact_path: None,
            workflow_id: options.workflow_id.clone(),
            node_id: options.node_id.clone(),
        };
        {
            let mut state = self.state.lock();
            refresh_idle_workflows(&mut state);
            if options
                .previous_run_id
                .as_ref()
                .is_some_and(|id| !state.runs.contains_key(id))
            {
                return Err(error("previous run expired during rework validation"));
            }
            for run in state.runs.values() {
                if !run.snapshot.status.is_terminal()
                    && reopen_path
                        .as_ref()
                        .is_some_and(|path| path == &PathBuf::from(&run.snapshot.path))
                {
                    return Err(error(
                        "retained worktree is already owned by an active/settling run",
                    ));
                }
                if options.phase_id.is_some() && options.phase_id == run.options.phase_id {
                    return Err(error(
                        "duplicate phase_id; query or continue the existing run",
                    ));
                }
                if !run.snapshot.status.is_terminal() && run.snapshot.call_id == call_id {
                    return Err(error("duplicate active call_id"));
                }
                if writer
                    && run.writer
                    && !run.snapshot.status.is_terminal()
                    && overlaps(options.scope.as_ref(), run.options.scope.as_ref())
                {
                    return Err(error(
                        "scope overlaps an active writer (including queued/settling runs)",
                    ));
                }
            }
            if writer {
                super::workflow::check_reservations(
                    &state,
                    options.scope.as_ref(),
                    options.workflow_id.as_deref(),
                )?;
            }
            if state.runs.len() >= MAX_RETAINED_AGENT_RUNS {
                let oldest = state
                    .runs
                    .iter()
                    .filter(|(_, r)| {
                        r.snapshot.status.is_terminal()
                            && options.previous_run_id.as_deref() != Some(r.snapshot.id.as_str())
                            && r.options.workflow_id.as_ref().is_none_or(|wid| {
                                state.workflows.get(wid).is_none_or(|w| w.is_terminal())
                            })
                    })
                    .min_by_key(|(_, r)| r.snapshot.finished_ms)
                    .map(|(id, _)| id.clone());
                if let Some(oldest) = oldest {
                    state.runs.remove(&oldest);
                } else {
                    return Err(error(
                        "retained run capacity reached; active workflows must settle first",
                    ));
                }
            }
            if let (Some(wid), Some(nid)) = (&options.workflow_id, &options.node_id)
                && let Some(w) = state.workflows.get_mut(wid)
            {
                if let Some(n) = w.snapshot.nodes.iter_mut().find(|n| &n.id == nid) {
                    n.run_id = Some(id.clone());
                }
                publish(progress, AgentStateEvent::Workflow(w.snapshot.clone()));
            }
            state.runs.insert(
                id.clone(),
                RunRecord {
                    snapshot: snapshot.clone(),
                    request: request.clone(),
                    options,
                    writer,
                    cwd: cwd.to_owned(),
                    cancel: child_cancel.clone(),
                    host: Some(host.clone()),
                },
            );
        }
        publish(progress, AgentStateEvent::Run(snapshot));
        let guard = child_cancel.clone().drop_guard();
        let runtime = self.clone();
        let progress = progress.clone();
        let run_id = id.clone();
        // The owned driver survives a dropped outer script/call long enough to
        // await host cancellation and retain recovery artifacts before release.
        let task = tokio::spawn(async move {
            let (sink, mut rx) = ToolStream::channel();
            let activity = Arc::new(super::AgentActivity::default());
            let host_work = match reopen_path.as_deref() {
                Some(path) => {
                    host.run_subagent_reopened(request, &sink, &child_cancel, &call_id, path)
                }
                None => host.run_subagent(request, &sink, &child_cancel, &call_id),
            };
            let work = std::panic::AssertUnwindSafe(activity.scope(host_work));
            let work = work.catch_unwind();
            tokio::pin!(work);
            let mut stopping = false;
            let mut panicked = false;
            runtime.update_run(&run_id, &progress, |r| {
                r.status = AgentRunStatus::Running;
                r.step = "starting".into();
            });
            let result = loop {
                tokio::select! {
                    result = &mut work => match result { Ok(result) => break result, Err(_) => { panicked = true; break Err(error("subagent host panicked")); } },
                    () = child_cancel.cancelled(), if !stopping => {
                        stopping = true;
                        runtime.update_run(&run_id, &progress, |r| { r.status = AgentRunStatus::Settling; r.step = "Waiting for cancellation to settle".into(); });
                    }
                    item = rx.recv() => { if let Some(ToolStreamItem::Progress(p)) = item { runtime.forward(&run_id, &progress, &sanitize_output(host.as_ref(), &p.message)); } }
                }
            };
            while let Some(item) = rx.try_recv() {
                if let ToolStreamItem::Progress(p) = item {
                    runtime.forward(
                        &run_id,
                        &progress,
                        &sanitize_output(host.as_ref(), &p.message),
                    );
                }
            }
            runtime.update_run(&run_id, &progress, |r| r.status = AgentRunStatus::Settling);
            activity.settled().await;
            let host_cancelled = result.as_ref().err().is_some_and(|e| {
                let message = e.to_string();
                message.contains(crate::tool::INTERRUPTED_NOTE)
                    || (message.starts_with("subagent ") && message.ends_with(" cancelled"))
            });
            let (answer, failed) = match result {
                Ok(text) => (sanitize_output(host.as_ref(), &text), false),
                Err(err) => (sanitize_output(host.as_ref(), &err.to_string()), true),
            };
            let artifact = runtime.write_artifact(&run_id, &answer).await;
            let cancelled = child_cancel.is_cancelled() || host_cancelled;
            let snapshot = {
                let mut state = runtime.state.lock();
                let r = state.runs.get_mut(&run_id).expect("active run retained");
                r.snapshot.status = if cancelled {
                    AgentRunStatus::Cancelled
                } else if failed || artifact.is_err() {
                    AgentRunStatus::Failed
                } else {
                    AgentRunStatus::Completed
                };
                r.snapshot.finished_ms = Some(now_ms());
                r.snapshot.step = format!("{:?}", r.snapshot.status).to_lowercase();
                if failed || cancelled {
                    r.snapshot.error = Some(clip(&answer, MAX_AGENT_ANSWER_CHARS));
                } else {
                    r.snapshot.result = Some(clip(&answer, MAX_AGENT_ANSWER_CHARS));
                }
                match artifact {
                    Ok(path) => r.snapshot.artifact_path = Some(path),
                    Err(err) => {
                        r.snapshot.error = Some(format!(
                            "result artifact failed: {err}\n{}",
                            r.snapshot.error.as_deref().unwrap_or("")
                        ))
                    }
                }
                r.host = None;
                r.snapshot.clone()
            };
            publish(&progress, AgentStateEvent::Run(snapshot.clone()));
            (snapshot, panicked)
        });
        let result = task
            .await
            .map_err(|e| error(format!("run driver failed: {e}")));
        guard.disarm();
        let (snapshot, panicked) = result?;
        if panicked && snapshot.workflow_id.is_none() {
            // Preserve the legacy host-panic behavior after retaining the
            // failure and settling all native side effects.
            std::panic::resume_unwind(Box::new("subagent host panicked"));
        }
        Ok(snapshot)
    }
    pub(super) fn retained_worktree(&self, id: &str) -> Result<Option<PathBuf>, ToolError> {
        let state = self.state.lock();
        let run = state
            .runs
            .get(id)
            .ok_or_else(|| error("previous run expired"))?;
        Ok((run.writer
            && (run.request.isolation.as_deref() == Some("worktree")
                || run.snapshot.path != run.cwd.display().to_string()))
        .then(|| PathBuf::from(&run.snapshot.path)))
    }

    fn forward(&self, id: &str, progress: &ToolStream, message: &str) {
        progress.progress(message);
        if let Some(rest) = message.strip_prefix("agent|") {
            let mut parts = rest.splitn(3, '|');
            let _role = parts.next();
            let phase = parts.next().unwrap_or("");
            let detail = parts.next().unwrap_or("");
            self.update_run(id, progress, |r| match phase {
                "path" => r.path = detail.into(),
                "tool" | "step" => r.step = clip(detail, 1000),
                "settling" => {
                    r.status = AgentRunStatus::Settling;
                    r.step = clip(detail, 1000);
                }
                "queued" if r.status != AgentRunStatus::Settling => {
                    r.status = AgentRunStatus::Queued
                }
                "goal" if r.status != AgentRunStatus::Settling => {
                    r.status = AgentRunStatus::Running
                }
                _ => {}
            });
        }
    }
    pub(super) async fn write_artifact(
        &self,
        id: &str,
        text: &str,
    ) -> Result<String, std::io::Error> {
        tokio::fs::create_dir_all(&self.artifacts).await?;
        let path = self.artifacts.join(format!("{id}.txt"));
        tokio::fs::write(&path, text).await?;
        Ok(path.display().to_string())
    }
}
pub(super) fn resolve_run_id(state: &RuntimeState, id: &str) -> Result<String, ToolError> {
    if state.runs.contains_key(id) {
        return Ok(id.into());
    }
    let mut matches = state.runs.values().filter(|r| r.snapshot.call_id == id);
    if let Some(active) = matches.clone().find(|r| !r.snapshot.status.is_terminal()) {
        return Ok(active.snapshot.id.clone());
    }
    let first = matches
        .next()
        .ok_or_else(|| error("unknown or expired run in this session"))?;
    if matches.next().is_some() {
        return Err(error(
            "call_id matches multiple completed runs; use the stable run id",
        ));
    }
    Ok(first.snapshot.id.clone())
}

pub(super) fn refresh_idle_workflows(state: &mut RuntimeState) {
    for w in state.workflows.values_mut() {
        w.refresh_cancellation();
    }
}

pub(super) fn overlaps(a: Option<&AgentScope>, b: Option<&AgentScope>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.overlaps(b),
        _ => false,
    }
}
fn sanitize_output(host: &dyn AgentHost, text: &str) -> String {
    super::redact::redact_text(&host.redact_text(text))
}
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
pub(super) fn error(message: impl Into<String>) -> ToolError {
    ToolError::Execution(message.into())
}
pub(super) fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!(
            "{}
[truncated; read artifact_path for full output]",
            &text[..end]
        ),
        None => text.into(),
    }
}
