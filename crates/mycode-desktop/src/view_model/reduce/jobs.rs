//! Live tool and subagent progress: the `agent|role|phase|detail` protocol,
//! the inspector-panel job cards, and the status lines they feed.
//!
//! Finished jobs drop out of the list: the panel and the status line only
//! ever describe work that is still running.

use crate::i18n::t;
use crate::view_model::{ConversationEntry, EntryKind, LiveJob, WorkspaceState};

use mycode_tools::builtin::AGENT_PROGRESS_PREFIX;

use super::streaming::{set_streaming_status, tool_call_label};

/// The `ToolStarted` transition: one tool call (or nested subagent) began.
pub(super) fn tool_started(
    state: &mut WorkspaceState,
    call_id: String,
    name: String,
    target: String,
) {
    let label = tool_call_label(&name, &target);
    set_streaming_status(state, &format!("{} {label}", t("Running", "正在执行")));
    if name == AGENT_PROGRESS_PREFIX {
        upsert_live_job(state, &call_id, "", "starting");
    }
    if let Some(conversation) = state.active.as_mut() {
        conversation.entries.push(ConversationEntry {
            event_id: format!("call-{call_id}"),
            kind: EntryKind::ToolCall,
            text: label.into(),
            call_id: Some(call_id),
            thinking: String::new(),
        });
    }
}

/// The `ToolProgress` transition: incremental tool or subagent progress for
/// the live status line.
pub(super) fn tool_progress(
    state: &mut WorkspaceState,
    call_id: String,
    name: String,
    message: String,
) {
    apply_live_job_progress(state, &call_id, &name, &message);
    let running = state.live_jobs.len();
    let status = if running > 1 {
        format!("{running} {}", t("subagents running", "个子代理运行中"))
    } else if is_agent_event(&name, &message) {
        live_job_status(state, &call_id)
    } else if name.is_empty() {
        message
    } else {
        format!("{name}: {message}")
    };
    set_streaming_status(state, &status);
}

fn is_agent_event(name: &str, message: &str) -> bool {
    name == AGENT_PROGRESS_PREFIX
        || message
            .split_once('|')
            .is_some_and(|(prefix, _)| prefix == AGENT_PROGRESS_PREFIX)
}

fn parse_agent_progress(message: &str) -> Option<(&str, &str, &str)> {
    let mut parts = message.splitn(4, '|');
    if parts.next()? != AGENT_PROGRESS_PREFIX {
        return None;
    }
    Some((parts.next()?, parts.next()?, parts.next().unwrap_or("")))
}

fn upsert_live_job(state: &mut WorkspaceState, call_id: &str, role: &str, step: &str) {
    if let Some(job) = state
        .live_jobs
        .iter_mut()
        .find(|job| !call_id.is_empty() && job.call_id == call_id)
    {
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            push_job_step(job, step);
        }
        return;
    }
    // A progress line may arrive before its `ToolStarted` claimed the call:
    // attach to the newest job that has no call id yet, claiming or filling
    // it as needed.
    if let Some(job) = state
        .live_jobs
        .iter_mut()
        .rev()
        .find(|job| job.call_id.is_empty())
    {
        if !call_id.is_empty() {
            job.call_id = call_id.to_owned();
        }
        if !role.is_empty() {
            job.role = role.to_owned();
        }
        if !step.is_empty() {
            push_job_step(job, step);
        }
        return;
    }
    let step = if step.is_empty() {
        "starting".to_owned()
    } else {
        step.to_owned()
    };
    state.live_jobs.push(LiveJob {
        call_id: call_id.to_owned(),
        role: role.to_owned(),
        label: String::new(),
        prompt: String::new(),
        path: String::new(),
        log: vec![step.clone()],
        step,
    });
}

fn push_job_step(job: &mut LiveJob, step: &str) {
    job.step = step.to_owned();
    if job.log.last().is_none_or(|last| last != step) {
        job.log.push(step.to_owned());
        if job.log.len() > 48 {
            job.log.remove(0);
        }
    }
}

fn apply_live_job_progress(state: &mut WorkspaceState, call_id: &str, name: &str, message: &str) {
    if !is_agent_event(name, message) {
        return;
    }
    if let Some((role, phase, detail)) = parse_agent_progress(message) {
        match phase {
            "queued" => {
                upsert_live_job(state, call_id, role, "queued");
                if let Some(job) = live_job_mut(state, call_id) {
                    job.label = detail.to_owned();
                }
            }
            "prompt" => {
                upsert_live_job(state, call_id, role, "starting");
                if let Some(job) = live_job_mut(state, call_id) {
                    job.prompt = detail.chars().take(8_000).collect();
                }
            }
            "path" => {
                upsert_live_job(state, call_id, role, "starting");
                if let Some(job) = live_job_mut(state, call_id) {
                    job.path = detail.to_owned();
                }
            }
            "done" => drop_live_job(state, call_id),
            "tool" => upsert_live_job(
                state,
                call_id,
                role,
                &format!("{} {detail}", t("running", "正在运行")),
            ),
            "step" => upsert_live_job(state, call_id, role, detail),
            other => upsert_live_job(state, call_id, role, other),
        }
        return;
    }
    upsert_live_job(state, call_id, "", message);
}

fn live_job_mut<'a>(state: &'a mut WorkspaceState, call_id: &str) -> Option<&'a mut LiveJob> {
    if call_id.is_empty() {
        return state.live_jobs.last_mut();
    }
    state
        .live_jobs
        .iter_mut()
        .find(|job| job.call_id == call_id)
}

fn live_job_status(state: &WorkspaceState, call_id: &str) -> String {
    let job = if call_id.is_empty() {
        state.live_jobs.last()
    } else {
        state.live_jobs.iter().find(|job| job.call_id == call_id)
    };
    let Some(job) = job else {
        return t("subagent", "子代理").to_owned();
    };
    let who = if job.role.is_empty() {
        t("subagent", "子代理")
    } else {
        job.role.as_str()
    };
    let detail = if matches!(job.step.as_str(), "queued" | "starting") && !job.label.is_empty() {
        job.label.as_str()
    } else {
        job.step.as_str()
    };
    format!("{who} · {detail}")
}

/// Removes one finished job and any detail window still showing it.
pub(super) fn drop_live_job(state: &mut WorkspaceState, call_id: &str) {
    if call_id.is_empty() {
        state.live_jobs.pop();
    } else {
        state.live_jobs.retain(|job| job.call_id != call_id);
    }
    if !call_id.is_empty() && state.subagent_window.as_deref() == Some(call_id) {
        state.subagent_window = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{is_agent_event, parse_agent_progress};
    use mycode_tools::builtin::AGENT_PROGRESS_PREFIX;

    #[test]
    fn progress_protocol_is_agent_only() {
        assert_eq!(AGENT_PROGRESS_PREFIX, "agent");
        assert_eq!(
            parse_agent_progress("agent|scout|queued|look"),
            Some(("scout", "queued", "look"))
        );
        assert!(parse_agent_progress("task|scout|queued|look").is_none());
        assert!(is_agent_event("agent", "starting"));
        assert!(!is_agent_event("task", "task|scout|queued|look"));
    }
}
