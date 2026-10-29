//! Typed agent progress is consumed before the legacy free-text protocol.
use super::*;
use mycode_app::builtin::{
    AGENT_STATE_PREFIX, AgentRunSnapshot, AgentRunStatus, AgentStateEvent, AgentWorkflowStatus,
};

#[cfg(test)]
mod tests;

fn session_id(state: &WorkspaceState) -> String {
    state
        .active
        .as_ref()
        .map(|active| active.session_id.clone())
        .unwrap_or_default()
}

pub(super) fn consume(
    state: &mut WorkspaceState,
    call_id: &str,
    provider_id: &str,
    message: &str,
) -> bool {
    let Some(json) = message.strip_prefix(AGENT_STATE_PREFIX) else {
        return false;
    };
    // Never paint protocol JSON as user-facing progress, including a malformed packet.
    if let Ok(event) = serde_json::from_str::<AgentStateEvent>(json) {
        apply(state, call_id, provider_id, event);
    }
    true
}

fn apply(state: &mut WorkspaceState, ledger_id: &str, provider_id: &str, event: AgentStateEvent) {
    let session_id = session_id(state);
    match event {
        AgentStateEvent::Run(run) => {
            if !state.sending && !run.status.is_terminal() {
                return;
            }
            let accepted = state
                .subagents
                .session_mut(&session_id)
                .upsert_run(run.clone());
            if !accepted {
                return;
            }
            let row_id = state
                .live_jobs
                .iter()
                .find(|job| {
                    job.call_id == run.call_id
                        || job.provider_call_id == run.call_id
                        || (run.workflow_id.is_none()
                            && job.call_id == ledger_id
                            && provider_id == run.call_id)
                })
                .map(|job| job.call_id.clone())
                .unwrap_or_else(|| run.call_id.clone());
            // A typed snapshot replaces an earlier compatibility-only archive.
            let session = state.subagents.session_mut(&session_id);
            if row_id != run.call_id {
                session.call_ids.retain(|(call, _)| call != &row_id);
                session.call_ids.push((row_id.clone(), run.id.clone()));
            }
            session
                .runs
                .retain(|old| old.id == run.id || old.id != format!("legacy:{row_id}"));
            if run.status.is_terminal() {
                drop_live_job(state, &row_id);
                state.finished_jobs.insert(run.call_id.clone());
                state.job_elapsed.retain(|(id, _)| id != &row_id);
                state
                    .job_elapsed
                    .push((row_id, crate::view_model::agent_run_elapsed_ms(&run)));
                if state.job_elapsed.len() > 128 {
                    state.job_elapsed.remove(0);
                }
            } else if !state.finished_jobs.contains(&row_id)
                && !state.finished_jobs.contains(&run.call_id)
            {
                upsert_live_job(state, &row_id, &run.role, &run.step);
                if let Some(job) = live_job_mut(state, &row_id) {
                    job.provider_call_id = run.call_id.clone();
                    job.label = run.description;
                    job.prompt = run.prompt;
                    job.path = run.path;
                    job.started_ms = run.started_ms;
                }
            }
        }
        AgentStateEvent::Workflow(workflow) => {
            if !state.sending
                && matches!(
                    workflow.status,
                    AgentWorkflowStatus::Queued | AgentWorkflowStatus::Running
                )
            {
                return;
            }
            state
                .subagents
                .session_mut(&session_id)
                .upsert_workflow(workflow);
        }
    }
    if state.sending {
        refresh_subagent_status(state);
    }
}

/// Prefer projected typed results; retain compatibility with status/workflow JSON text.
pub(in crate::view_model::reduce) fn consume_result(
    state: &mut WorkspaceState,
    entry: &ConversationEntry,
) {
    if let Some(events) = &entry.agent_states {
        for event in events.iter() {
            apply(
                state,
                entry.call_id.as_deref().unwrap_or(""),
                "",
                event.clone(),
            );
        }
        return;
    }
    let text = entry
        .text
        .trim()
        .strip_prefix("failed: ")
        .unwrap_or(entry.text.trim());
    let value = serde_json::from_str::<serde_json::Value>(text).ok();
    let events = value.and_then(|value| {
        let value = value.get("agent_states").cloned().unwrap_or(value);
        if value.is_array() {
            serde_json::from_value::<Vec<AgentStateEvent>>(value).ok()
        } else {
            serde_json::from_value::<AgentStateEvent>(value)
                .ok()
                .map(|event| vec![event])
        }
    });
    if let Some(events) = events {
        for event in events {
            apply(state, entry.call_id.as_deref().unwrap_or(""), "", event);
        }
        return;
    }
    let Some(call_id) = entry.call_id.as_deref() else {
        return;
    };
    let session_id = session_id(state);
    let failed = entry.text.starts_with("failed: ");
    let session = state.subagents.session_mut(&session_id);
    // Generic outer results cannot rewrite a typed child verdict.
    if let Some(run) = session
        .runs
        .iter_mut()
        .find(|run| run.id == format!("legacy:{call_id}"))
    {
        if run.status != AgentRunStatus::Cancelled {
            run.status = if failed {
                AgentRunStatus::Failed
            } else {
                AgentRunStatus::Completed
            };
        }
        if failed {
            run.error = Some(text.to_owned());
        } else {
            run.result = Some(text.to_owned());
        }
        return;
    }
    if let Some(job) = state
        .live_jobs
        .iter()
        .find(|job| job.call_id == call_id)
        .cloned()
    {
        archive_legacy(
            state,
            &job,
            if failed {
                AgentRunStatus::Failed
            } else {
                AgentRunStatus::Completed
            },
            Some(text.to_owned()),
        );
    }
}

pub(super) fn complete_call(state: &mut WorkspaceState, call_id: &str, status: AgentRunStatus) {
    let session_id = session_id(state);
    if let Some(run) = state
        .subagents
        .session_mut(&session_id)
        .runs
        .iter_mut()
        .find(|run| run.call_id == call_id)
        && !run.status.is_terminal()
    {
        run.status = status;
        run.finished_ms = Some(crate::unix_now().as_millis() as u64);
        run.step = crate::view_model::agent_run_status_label(status).to_owned();
    }
}

pub(super) fn archive_legacy(
    state: &mut WorkspaceState,
    job: &LiveJob,
    status: AgentRunStatus,
    result: Option<String>,
) {
    let session_id = session_id(state);
    let session = state.subagents.session_mut(&session_id);
    if job.call_id.is_empty()
        || session.runs.iter().any(|run| {
            run.call_id == job.call_id
                || (!job.provider_call_id.is_empty() && run.call_id == job.provider_call_id)
        })
    {
        return;
    }
    session.upsert_run(AgentRunSnapshot {
        id: format!("legacy:{}", job.call_id),
        call_id: if job.provider_call_id.is_empty() {
            job.call_id.clone()
        } else {
            job.provider_call_id.clone()
        },
        role: job.role.clone(),
        description: job.label.clone(),
        prompt: job.prompt.clone(),
        status,
        step: job.step.clone(),
        path: job.path.clone(),
        started_ms: job.started_ms,
        finished_ms: Some(crate::unix_now().as_millis() as u64),
        result: if status == AgentRunStatus::Completed {
            result.clone()
        } else {
            None
        },
        error: if status == AgentRunStatus::Failed {
            result
        } else {
            None
        },
        artifact_path: None,
        workflow_id: None,
        node_id: None,
    });
    session
        .activity
        .retain(|(id, _)| id != &format!("legacy:{}", job.call_id));
    session
        .activity
        .push((format!("legacy:{}", job.call_id), job.log.clone()));
}

pub(in crate::view_model::reduce) fn cancelled(state: &mut WorkspaceState, call_id: &str) {
    let session_id = session_id(state);
    let provider_id = state
        .live_jobs
        .iter()
        .find(|job| job.call_id == call_id)
        .map(|job| job.provider_call_id.clone());
    if let Some(job) = state
        .live_jobs
        .iter()
        .find(|job| job.call_id == call_id)
        .cloned()
    {
        archive_legacy(state, &job, AgentRunStatus::Cancelled, None);
    }
    if let Some(run) = state
        .subagents
        .session_mut(&session_id)
        .runs
        .iter_mut()
        .find(|run| run.call_id == call_id || provider_id.as_deref() == Some(run.call_id.as_str()))
        && !run.status.is_terminal()
    {
        run.status = AgentRunStatus::Settling;
        run.step = t("Waiting for cancellation to settle", "等待停止操作完成收尾").to_owned();
    }
}
