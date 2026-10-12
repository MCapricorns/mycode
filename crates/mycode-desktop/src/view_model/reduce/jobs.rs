//! Live tool and subagent progress: the `agent|role|phase|detail` protocol,
//! the running scout/artisan rows, and the status lines they feed.
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
    parent: Option<String>,
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
            parent_call_id: parent,
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
    let capped = crate::view_model::capped_artisan_count(&state.live_jobs);
    let running = state.live_jobs.len();
    let status = if capped > 0 {
        crate::view_model::capped_artisan_status(capped)
    } else if running > 1 {
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
        capped: false,
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
            "capped" => {
                let warning = role == "artisan";
                let step = if warning {
                    cap_step(detail)
                } else {
                    "queued".to_owned()
                };
                upsert_live_job(state, call_id, role, &step);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.capped = warning;
                }
            }
            "path" => {
                upsert_live_job(state, call_id, role, "starting");
                if let Some(job) = live_job_mut(state, call_id) {
                    job.path = detail.to_owned();
                    job.capped = false;
                }
            }
            "done" => drop_live_job(state, call_id),
            "tool" => {
                upsert_live_job(
                    state,
                    call_id,
                    role,
                    &format!("{} {detail}", t("running", "正在运行")),
                );
                if let Some(job) = live_job_mut(state, call_id) {
                    job.capped = false;
                }
            }
            "step" => {
                upsert_live_job(state, call_id, role, detail);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.capped = false;
                }
            }
            other => {
                upsert_live_job(state, call_id, role, other);
                if let Some(job) = live_job_mut(state, call_id) {
                    job.capped = false;
                }
            }
        }
        return;
    }
    upsert_live_job(state, call_id, "", message);
}

fn cap_step(detail: &str) -> String {
    let cap = detail.trim();
    if cap.is_empty() {
        t(
            "queued past the concurrency cap",
            "已超过并发上限，正在排队",
        )
        .to_owned()
    } else {
        format!(
            "{} ({cap})",
            t(
                "queued past the concurrency cap",
                "已超过并发上限，正在排队",
            )
        )
    }
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

    #[test]
    fn an_artisan_past_the_cap_warns_until_it_starts() {
        use crate::view_model::{
            ActiveConversation, DesktopAction, WorkspaceState, capped_artisan_count,
            capped_artisan_status,
        };

        let mut state = WorkspaceState {
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        super::super::reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: "c1".to_owned(),
                name: String::new(),
                message: "agent|scout|queued|map tokio".to_owned(),
            },
        );
        super::super::reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: "c2".to_owned(),
                name: String::new(),
                message: "agent|artisan|capped|4".to_owned(),
            },
        );
        super::super::reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: "c3".to_owned(),
                name: String::new(),
                message: "agent|scout|capped|4".to_owned(),
            },
        );
        let scout = state
            .live_jobs
            .iter()
            .find(|job| job.call_id == "c1")
            .expect("scout");
        assert_eq!(scout.role, "scout");
        assert!(!scout.capped);
        assert_eq!(scout.label, "map tokio");
        assert_eq!(scout.title(), "scout · map tokio");
        let artisan = state
            .live_jobs
            .iter()
            .find(|job| job.call_id == "c2")
            .expect("artisan");
        assert!(artisan.capped);
        assert_eq!(artisan.role, "artisan");
        assert!(artisan.step.contains("(4)"), "{}", artisan.step);
        let ignored = state
            .live_jobs
            .iter()
            .find(|job| job.call_id == "c3")
            .expect("scout cap line");
        assert!(!ignored.capped);
        assert_eq!(capped_artisan_count(&state.live_jobs), 1);
        let status = state
            .active
            .as_ref()
            .and_then(|conversation| conversation.streaming.as_ref())
            .map(|streaming| streaming.status.clone())
            .unwrap_or_default();
        assert_eq!(status, capped_artisan_status(1));

        super::super::reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: "c2".to_owned(),
                name: String::new(),
                message: "agent|artisan|path|/tmp/wt".to_owned(),
            },
        );
        let artisan = state
            .live_jobs
            .iter()
            .find(|job| job.call_id == "c2")
            .expect("artisan still running");
        assert!(!artisan.capped);
        assert_eq!(artisan.path, "/tmp/wt");
        assert_eq!(capped_artisan_count(&state.live_jobs), 0);

        super::super::reduce(
            &mut state,
            DesktopAction::SubagentDismissed("c1".to_owned()),
        );
        assert!(state.live_jobs.iter().all(|job| job.call_id != "c1"));
    }
}
