//! Explicit, bounded workflow rework. Admission commits only after validation.
use super::{
    AgentHost, AgentRuntime,
    runtime::{clip, error, overlaps},
    state::*,
    workflow::{AgentWorkflowArgs, check_reservations, depends_on},
};
use crate::{ctx::ToolCtx, stream::ToolStream, tool::ToolError};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) const MAX_WORKFLOW_REVISIONS: u32 = 16;
#[derive(Clone)]
pub(super) struct ReworkContext {
    pub previous_run_id: Option<String>,
    pub report_path: String,
    pub feedback: String,
    pub summary: String,
}
impl AgentRuntime {
    pub(super) async fn revise_workflow(
        &self,
        host: Arc<dyn AgentHost>,
        id: &str,
        args: &AgentWorkflowArgs,
        ctx: &ToolCtx,
        stream: &ToolStream,
    ) -> Result<(), ToolError> {
        let node_id = args
            .node_id
            .as_deref()
            .ok_or_else(|| error("revise requires producer node_id"))?;
        let feedback = args
            .feedback
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("revise requires nonblank feedback"))?;
        if feedback.chars().count() > 8000 {
            return Err(error("rework feedback exceeds 8000 characters"));
        }
        let expected = args.expected_revision.ok_or_else(|| {
            error("revise requires expected_revision from the current workflow snapshot")
        })?;
        let (mut specs, affected, previous, old_events) = {
            let state = self.state.lock();
            let w = state
                .workflows
                .get(id)
                .ok_or_else(|| error("unknown workflow in this session"))?;
            ensure_revisable(&state, w, expected, node_id, ctx)?;
            if node_id == "workflow-acceptance" || !w.specs.iter().any(|n| n.id == node_id) {
                return Err(error(
                    "revise must target an authored producer, not independent acceptance",
                ));
            }
            let affected: BTreeSet<String> = w
                .specs
                .iter()
                .filter(|n| n.id == node_id || depends_on(&w.specs, &n.id, node_id))
                .map(|n| n.id.clone())
                .collect();
            let mut previous = BTreeMap::new();
            for node in &w.snapshot.nodes {
                if affected.contains(&node.id) {
                    if node.status == AgentRunStatus::Cancelled {
                        return Err(error("cancelled nodes cannot be revived by rework"));
                    }
                    if let Some(run_id) = &node.run_id {
                        let run=state.runs.get(run_id).ok_or_else(||error("rework source run expired; retained artifacts remain available for manual recovery"))?;
                        if !matches!(
                            run.snapshot.status,
                            AgentRunStatus::Completed | AgentRunStatus::Failed
                        ) {
                            return Err(error("rework source has not settled or was cancelled"));
                        }
                    }
                    previous.insert(node.id.clone(), node.run_id.clone());
                }
            }
            let events = state
                .runs
                .values()
                .filter(|r| r.snapshot.workflow_id.as_deref() == Some(id))
                .map(|r| AgentStateEvent::Run(r.snapshot.clone()))
                .chain(std::iter::once(AgentStateEvent::Workflow(
                    w.snapshot.clone(),
                )))
                .collect::<Vec<_>>();
            (w.specs.clone(), affected, previous, events)
        };
        for spec in specs.iter_mut().filter(|spec| affected.contains(&spec.id)) {
            spec.writer = host.validate_role(&spec.role).await?;
            if spec.role == "sentinel" && spec.writer {
                return Err(error("workflow sentinel must remain read-only"));
            }
            if let Some(Some(source)) = previous.get(&spec.id)
                && let Some(path) = self.retained_worktree(source)?
            {
                host.validate_reopen(&path).await?;
            }
        }
        if ctx.cancel.is_cancelled() {
            return Err(error("rework cancelled before admission"));
        }
        let summaries: BTreeMap<String, String> = previous
            .iter()
            .map(|(node, run)| {
                let summary = format!(
                    "{}\n{}",
                    previous_summary(&old_events, run.as_deref(), "Previous producer output"),
                    previous_summary(
                        &old_events,
                        previous
                            .get("workflow-acceptance")
                            .and_then(|id| id.as_deref()),
                        "Independent review feedback"
                    )
                );
                (node.clone(), summary)
            })
            .collect();
        let report=serde_json::to_string_pretty(&serde_json::json!({"workflow_id":id,"previous_revision":expected,"producer":node_id,"feedback":feedback,"evidence":old_events})).map_err(|e|error(e.to_string()))?;
        let report = super::redact::redact_text(&host.redact_text(&report));
        let report_path = self
            .write_artifact(&format!("{id}-rework-{}", uuid::Uuid::new_v4()), &report)
            .await
            .map_err(|e| error(format!("rework evidence artifact: {e}")))?;
        let feedback = super::redact::redact_text(&host.redact_text(feedback));
        let snapshot = {
            let mut state = self.state.lock();
            super::runtime::refresh_idle_workflows(&mut state);
            let w = state
                .workflows
                .get(id)
                .ok_or_else(|| error("workflow expired during rework validation"))?;
            ensure_revisable(&state, w, expected, node_id, ctx)?;
            // Recheck source retention and every pending writer atomically with
            // the revision change. No child or scope is allocated before this.
            for source in previous.values().flatten() {
                if !state.runs.contains_key(source) {
                    return Err(error("rework source expired during validation"));
                }
            }
            for spec in specs
                .iter()
                .filter(|s| affected.contains(&s.id) && s.writer)
            {
                check_reservations(&state, spec.scope.as_ref(), Some(id))?;
                if state.runs.values().any(|r| {
                    r.writer
                        && !r.snapshot.status.is_terminal()
                        && overlaps(spec.scope.as_ref(), r.options.scope.as_ref())
                }) {
                    return Err(error("rework scope overlaps an active/settling writer"));
                }
            }
            let w = state
                .workflows
                .get_mut(id)
                .expect("validated workflow retained");
            w.snapshot.revision += 1;
            w.snapshot.status = AgentWorkflowStatus::Running;
            w.snapshot.acceptance = None;
            w.snapshot.result = None;
            w.specs = specs;
            w.cancel = ctx.cancel.child_token();
            w.driving = true;
            w.pause_before_gate = w.snapshot.name == "migration" && node_id.starts_with("apply-");
            for node in &mut w.snapshot.nodes {
                if affected.contains(&node.id) {
                    w.rework.insert(
                        node.id.clone(),
                        ReworkContext {
                            previous_run_id: previous[&node.id].clone(),
                            report_path: report_path.clone(),
                            feedback: clip(&feedback, 8000),
                            summary: summaries[&node.id].clone(),
                        },
                    );
                    node.status = AgentRunStatus::Queued;
                    node.run_id = None;
                }
            }
            w.snapshot.clone()
        };
        publish(stream, AgentStateEvent::Workflow(snapshot));
        Ok(())
    }
}
fn previous_summary(events: &[AgentStateEvent], id: Option<&str>, label: &str) -> String {
    let run = events.iter().find_map(|event| match event {
        AgentStateEvent::Run(run) if Some(run.id.as_str()) == id => Some(run),
        _ => None,
    });
    match run {
        Some(run) => format!(
            "{label} ({:?}): {}\nFull result artifact: {}",
            run.status,
            clip(
                run.result
                    .as_deref()
                    .or(run.error.as_deref())
                    .unwrap_or("no output"),
                1500
            ),
            run.artifact_path.as_deref().unwrap_or("none")
        ),
        None => format!("{label}: no prior run; inspect retained workflow evidence"),
    }
}

fn ensure_revisable(
    state: &super::runtime::RuntimeState,
    w: &super::workflow::WorkflowRecord,
    expected: u32,
    node_id: &str,
    ctx: &ToolCtx,
) -> Result<(), ToolError> {
    if w.snapshot.revision != expected {
        return Err(error(
            "stale or duplicate expected_revision; query the current workflow before revising",
        ));
    }
    if w.snapshot.revision >= MAX_WORKFLOW_REVISIONS {
        return Err(error(
            "workflow reached its explicit rework limit (16 revisions); retain evidence for a new bounded task",
        ));
    }
    if ctx.cancel.is_cancelled()
        || w.cancel.is_cancelled()
        || w.snapshot.status == AgentWorkflowStatus::Cancelled
    {
        return Err(error("cancelled workflows cannot be revived by revise"));
    }
    if w.driving
        || !matches!(
            w.snapshot.status,
            AgentWorkflowStatus::Failed
                | AgentWorkflowStatus::Completed
                | AgentWorkflowStatus::WaitingIntegration
        )
        || state.runs.values().any(|r| {
            r.snapshot.workflow_id.as_deref() == Some(&w.snapshot.id)
                && !r.snapshot.status.is_terminal()
        })
    {
        return Err(error(
            "workflow is running or settling; revise must wait for all runs and handoffs to settle",
        ));
    }
    if w.snapshot.name == "migration"
        && w.snapshot.status == AgentWorkflowStatus::WaitingIntegration
        && node_id == "gate"
    {
        return Err(error(
            "gate rework cannot bypass manual integration; call resume/accept after integrating the writer worktrees",
        ));
    }
    if w.cwd != ctx.cwd {
        return Err(error("rework must use the original parent cwd"));
    }
    Ok(())
}
