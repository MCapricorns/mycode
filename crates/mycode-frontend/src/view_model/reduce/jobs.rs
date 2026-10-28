//! Live tool and subagent progress: the `agent|role|phase|detail` protocol,
//! the running scout/artisan rows, and the status lines they feed.
//!
//! Finished jobs drop out of the list: the panel and the status line only
//! ever describe work that is still running.

use crate::i18n::t;
use crate::view_model::{ConversationEntry, EntryKind, LiveJob, WorkspaceState};

use mycode_app::builtin::AGENT_PROGRESS_PREFIX;

use super::streaming::{set_streaming_status, tool_call_label};

mod code_mode;

fn job_finished(state: &WorkspaceState, call_id: &str) -> bool {
    state.finished_jobs.contains(call_id)
        || state.active.as_ref().is_some_and(|conversation| {
            conversation.entries.iter().any(|entry| {
                entry.kind == EntryKind::ToolResult && entry.call_id.as_deref() == Some(call_id)
            })
        })
}

/// Event-id prefix of a placeholder row for an `agent` call the model is
/// still writing. The row carries no call id, so nothing pairs with it.
pub const PREPARING_ROW_PREFIX: &str = "prep-";

/// Whether `entry` is a placeholder for a call that has not started yet.
#[must_use]
pub fn is_preparing_row(entry: &ConversationEntry) -> bool {
    entry.event_id.starts_with(PREPARING_ROW_PREFIX)
}

/// The `ToolPreparing` transition: add the call's placeholder row, or
/// relabel it once its role and brief are readable.
pub(super) fn tool_preparing(
    state: &mut WorkspaceState,
    provider_call_id: &str,
    name: &str,
    target: &str,
) {
    if state.active.is_none() {
        return;
    }
    let event_id = format!("{PREPARING_ROW_PREFIX}{provider_call_id}");
    let label = tool_call_label(name, target);
    match state
        .preparing_calls
        .iter_mut()
        .find(|entry| entry.event_id == event_id)
    {
        Some(row) => row.text = label.into(),
        None => state.preparing_calls.push(ConversationEntry {
            event_id,
            kind: EntryKind::ToolCall,
            text: label.into(),
            call_id: None,
            thinking: String::new(),
        }),
    }
    let count = state.preparing_calls.len();
    let status = if crate::i18n::is_chinese() {
        format!("正在准备 {count} 个子代理")
    } else if count == 1 {
        "Preparing 1 subagent".to_owned()
    } else {
        format!("Preparing {count} subagents")
    };
    set_streaming_status(state, &status);
}

/// Removes every placeholder row; the turn ended before those calls ran.
pub(super) fn drop_preparing_rows(state: &mut WorkspaceState) {
    state.preparing_calls.clear();
}

/// The `ToolStarted` transition: one tool call (or nested subagent) began.
pub(super) fn tool_started(
    state: &mut WorkspaceState,
    call_id: String,
    provider_call_id: String,
    name: String,
    target: String,
) {
    // A reopened snapshot may already contain this result while its live
    // start event is still in the UI queue. It must not restart a finished job.
    if job_finished(state, &call_id) {
        return;
    }
    let label = tool_call_label(&name, &target);
    // The started call replaces its placeholder, if it had one, and joins
    // the transcript after the step that issued it.
    let placeholder = format!("{PREPARING_ROW_PREFIX}{provider_call_id}");
    state
        .preparing_calls
        .retain(|entry| entry.event_id != placeholder);
    if name == AGENT_PROGRESS_PREFIX {
        // The call's target is `{role} {short goal}` — durable text the
        // transcript row carries too — so the job names itself even when
        // the one-shot `queued`/`prompt` lines never arrive.
        let (role, goal) = agent_target_parts(&target);
        upsert_live_job(state, &call_id, role, "starting");
        if let Some(job) = live_job_mut(state, &call_id)
            && job.label.is_empty()
        {
            job.label = goal.to_owned();
        }
        claim_provider_call_id(state, &call_id, provider_call_id);
        refresh_subagent_status(state);
    } else {
        set_streaming_status(state, &format!("{} {label}", t("Running", "正在执行")));
    }
    if let Some(conversation) = state.active.as_mut()
        && !conversation.entries.iter().any(|entry| {
            entry.kind == EntryKind::ToolCall && entry.call_id.as_deref() == Some(&call_id)
        })
    {
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
    provider_call_id: String,
    name: String,
    message: String,
) {
    if code_mode::apply_progress(state, &message) {
        return;
    }
    let agent_event = is_agent_event(&name, &message);
    if agent_event && job_finished(state, &call_id) {
        return;
    }
    apply_live_job_progress(state, &call_id, &name, &message);
    claim_provider_call_id(state, &call_id, provider_call_id);
    if agent_event || !state.live_jobs.is_empty() {
        refresh_subagent_status(state);
        return;
    }
    let status = if name.is_empty() {
        message
    } else {
        format!("{name}: {message}")
    };
    set_streaming_status(state, &status);
}

/// Points the status line at the running subagents, or back at the parent
/// turn once the last one has finished. What each child is doing stays on
/// its transcript row and in the inspector.
pub(super) fn refresh_subagent_status(state: &mut WorkspaceState) {
    let status = crate::view_model::subagent_wait_status(&state.live_jobs)
        .unwrap_or_else(|| t("Waiting for the next step", "等待下一步").to_owned());
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

/// The `agent` call's target is `{role} {short goal}` (`tool_target` joins
/// them with a single space); a target holding only the role yields an
/// empty goal.
fn agent_target_parts(target: &str) -> (&str, &str) {
    match target.trim().split_once(' ') {
        Some((role, goal)) => (role, goal.trim()),
        None => (target.trim(), ""),
    }
}

/// A job recreated from a later `tool`/`step` line — a reopened session
/// replays the call row but never re-sends `queued` — still finds its role
/// and goal on the transcript row the ledger holds for the same call.
fn backfill_job_from_transcript(state: &mut WorkspaceState, call_id: &str) {
    if call_id.is_empty()
        || !state
            .live_jobs
            .iter()
            .any(|job| job.call_id == call_id && (job.role.is_empty() || job.label.is_empty()))
    {
        return;
    }
    let row_text = state
        .active
        .as_ref()
        .and_then(|active| {
            active.entries.iter().find(|entry| {
                entry.kind == EntryKind::ToolCall && entry.call_id.as_deref() == Some(call_id)
            })
        })
        .map(|entry| entry.text.to_string());
    // Rows render as `agent  {role} {goal}` (tool_label's name + target).
    let Some((role, goal)) = row_text
        .as_deref()
        .and_then(|text| text.strip_prefix(AGENT_PROGRESS_PREFIX))
        .map(str::trim)
        .map(agent_target_parts)
    else {
        return;
    };
    if let Some(job) = live_job_mut(state, call_id) {
        if job.role.is_empty() {
            job.role = role.to_owned();
        }
        if job.label.is_empty() {
            job.label = goal.to_owned();
        }
    }
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
        backfill_job_from_transcript(state, call_id);
        return;
    }
    let step = if step.is_empty() {
        "starting".to_owned()
    } else {
        step.to_owned()
    };
    state.live_jobs.push(LiveJob {
        call_id: call_id.to_owned(),
        provider_call_id: String::new(),
        role: role.to_owned(),
        label: String::new(),
        prompt: String::new(),
        path: String::new(),
        log: vec![step.clone()],
        step,
        capped: false,
        started_ms: crate::unix_now().as_millis() as u64,
    });
    backfill_job_from_transcript(state, call_id);
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
            // `goal`/`task` are the run-live re-arms of `queued`/`prompt`:
            // they only refill the fields so a replay never pushes a stale
            // "queued" step over the job's real progress.
            "goal" => {
                upsert_live_job(state, call_id, role, "");
                if let Some(job) = live_job_mut(state, call_id) {
                    job.label = detail.to_owned();
                }
            }
            "task" => {
                upsert_live_job(state, call_id, role, "");
                if let Some(job) = live_job_mut(state, call_id) {
                    job.prompt = detail.chars().take(8_000).collect();
                }
            }
            "done" => {
                drop_live_job(state, call_id);
            }
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

/// Records the provider id the core's cancel map uses on the job keyed by
/// `call_id`. A progress line may create the job before (or, after a session
/// switch, instead of) its `ToolStarted`, so either event can fill it.
fn claim_provider_call_id(state: &mut WorkspaceState, call_id: &str, provider_call_id: String) {
    if call_id.is_empty() || provider_call_id.is_empty() {
        return;
    }
    if let Some(job) = live_job_mut(state, call_id)
        && job.provider_call_id.is_empty()
    {
        job.provider_call_id = provider_call_id;
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

/// Removes one finished job and any detail window still showing it.
/// Its clock stops here: the final runtime is stored for the transcript
/// row that stays behind. Returns whether a job left the list.
pub(super) fn drop_live_job(state: &mut WorkspaceState, call_id: &str) -> bool {
    if !call_id.is_empty() {
        state.finished_jobs.insert(call_id.to_owned());
    }
    let index = if call_id.is_empty() {
        state.live_jobs.len().checked_sub(1)
    } else {
        state
            .live_jobs
            .iter()
            .position(|job| job.call_id == call_id)
    };
    let Some(index) = index else {
        return false;
    };
    let job = state.live_jobs.remove(index);
    record_job_elapsed(state, &job);
    if state.subagent_window.as_deref() == Some(job.call_id.as_str()) && !job.call_id.is_empty() {
        state.subagent_window = None;
    }
    true
}

/// Stops every running clock at once: a turn end or a session switch
/// retires all jobs, recording each final runtime for its transcript row.
pub(super) fn retire_live_jobs(state: &mut WorkspaceState) {
    let jobs = std::mem::take(&mut state.live_jobs);
    for job in &jobs {
        record_job_elapsed(state, job);
    }
    state.subagent_window = None;
}

/// Stores one job's final runtime keyed by its call id. Jobs that never
/// claimed a call id leave nothing a transcript row could look up.
fn record_job_elapsed(state: &mut WorkspaceState, job: &LiveJob) {
    if job.call_id.is_empty() || job.started_ms == 0 {
        return;
    }
    state.job_elapsed.retain(|(id, _)| id != &job.call_id);
    state
        .job_elapsed
        .push((job.call_id.clone(), job.elapsed_ms()));
    const MAX_JOB_ELAPSED: usize = 128;
    if state.job_elapsed.len() > MAX_JOB_ELAPSED {
        state.job_elapsed.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_agent_event, parse_agent_progress};

    #[test]
    fn progress_protocol_is_agent_only() {
        assert_eq!(
            parse_agent_progress("agent|scout|queued|look"),
            Some(("scout", "queued", "look"))
        );
        assert!(parse_agent_progress("task|scout|queued|look").is_none());
        assert!(is_agent_event("agent", "starting"));
        assert!(!is_agent_event("task", "task|scout|queued|look"));
    }

    #[test]
    fn a_running_agent_leaves_its_step_off_the_status_line() {
        use crate::view_model::{
            ActiveConversation, ConversationEntry, EntryKind, FrontendAction, WorkspaceState,
        };

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        let status = |state: &WorkspaceState| {
            state
                .active
                .as_ref()
                .and_then(|conversation| conversation.streaming.as_ref())
                .map(|streaming| streaming.status.clone())
                .unwrap_or_default()
        };
        super::super::reduce(
            &mut state,
            FrontendAction::ToolStarted {
                call_id: "c1".to_owned(),
                provider_call_id: "call_1".to_owned(),
                name: "agent".to_owned(),
                target: "scout map tokio".to_owned(),
            },
        );
        // The clock started with the call.
        assert!(state.live_jobs[0].started_ms > 0);
        let waiting = status(&state);
        assert!(!waiting.contains("tokio"), "{waiting}");
        super::super::reduce(
            &mut state,
            FrontendAction::ToolProgress {
                call_id: "c1".to_owned(),
                provider_call_id: String::new(),
                name: "agent".to_owned(),
                message: "agent|scout|tool|read src/lib.rs".to_owned(),
            },
        );
        assert_eq!(status(&state), waiting);
        super::super::reduce(
            &mut state,
            FrontendAction::ToolResultAppended(ConversationEntry {
                event_id: "res-c1".to_owned(),
                kind: EntryKind::ToolResult,
                text: "done".into(),
                call_id: Some("c1".to_owned()),
                thinking: String::new(),
            }),
        );
        assert!(state.live_jobs.is_empty());
        assert_ne!(status(&state), waiting);
        // Its row keeps the runtime the job accumulated before it left.
        assert!(state.job_elapsed("c1").is_some());
    }

    #[test]
    fn a_finished_turn_parks_every_running_clock() {
        use crate::view_model::{
            ActiveConversation, ConversationEntry, EntryKind, FrontendAction, WorkspaceState,
        };

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        for call_id in ["c1", "c2"] {
            super::super::reduce(
                &mut state,
                FrontendAction::ToolStarted {
                    call_id: call_id.to_owned(),
                    provider_call_id: String::new(),
                    name: "agent".to_owned(),
                    target: "scout".to_owned(),
                },
            );
        }
        super::super::reduce(
            &mut state,
            FrontendAction::ChatDone {
                head: "evt-2".to_owned(),
                entry: ConversationEntry {
                    event_id: "evt-2".to_owned(),
                    kind: EntryKind::AssistantMessage,
                    text: "done".into(),
                    call_id: None,
                    thinking: String::new(),
                },
            },
        );
        assert!(state.live_jobs.is_empty());
        assert!(state.job_elapsed("c1").is_some());
        assert!(state.job_elapsed("c2").is_some());
    }

    #[test]
    fn an_artisan_past_the_cap_warns_until_it_starts() {
        use crate::view_model::{
            ActiveConversation, FrontendAction, WorkspaceState, capped_artisan_count,
            capped_artisan_status,
        };

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        super::super::reduce(
            &mut state,
            FrontendAction::ToolProgress {
                call_id: "c1".to_owned(),
                provider_call_id: String::new(),
                name: String::new(),
                message: "agent|scout|queued|map tokio".to_owned(),
            },
        );
        super::super::reduce(
            &mut state,
            FrontendAction::ToolProgress {
                call_id: "c2".to_owned(),
                provider_call_id: String::new(),
                name: String::new(),
                message: "agent|artisan|capped|4".to_owned(),
            },
        );
        super::super::reduce(
            &mut state,
            FrontendAction::ToolProgress {
                call_id: "c3".to_owned(),
                provider_call_id: String::new(),
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
            FrontendAction::ToolProgress {
                call_id: "c2".to_owned(),
                provider_call_id: String::new(),
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
            FrontendAction::SubagentDismissed("c1".to_owned()),
        );
        assert!(state.live_jobs.iter().all(|job| job.call_id != "c1"));
        // A confirmed cancel stops its clock before the tool result lands.
        assert!(state.job_elapsed("c1").is_some());
    }

    #[test]
    fn a_streaming_agent_call_shows_a_placeholder_until_it_starts() {
        use crate::view_model::{
            ActiveConversation, ConversationEntry, EntryKind, FrontendAction, WorkspaceState,
        };

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        let ids = |rows: &[ConversationEntry]| -> Vec<String> {
            rows.iter().map(|entry| entry.event_id.clone()).collect()
        };
        let entries = |state: &WorkspaceState| ids(&state.active.as_ref().unwrap().entries);
        let prepare = |state: &mut WorkspaceState, id: &str, target: &str| {
            super::super::reduce(
                state,
                FrontendAction::ToolPreparing {
                    provider_call_id: id.to_owned(),
                    name: "agent".to_owned(),
                    target: target.to_owned(),
                },
            );
        };
        prepare(&mut state, "p1", "");
        prepare(&mut state, "p1", "scout map the crate");
        prepare(&mut state, "p2", "artisan");
        let labels: Vec<String> = state
            .preparing_calls
            .iter()
            .map(|entry| entry.text.to_string())
            .collect();
        assert_eq!(
            labels,
            ["agent  scout map the crate", "agent  artisan"],
            "one row per call, relabelled in place"
        );
        assert!(
            entries(&state).is_empty(),
            "placeholders stay out of the transcript"
        );
        // The step that issued the calls commits before they start.
        super::super::reduce(
            &mut state,
            FrontendAction::AssistantStepCommitted(ConversationEntry {
                event_id: "evt-step".to_owned(),
                kind: EntryKind::AssistantMessage,
                text: "dispatching".into(),
                call_id: None,
                thinking: String::new(),
            }),
        );
        super::super::reduce(
            &mut state,
            FrontendAction::ToolStarted {
                call_id: "c1".to_owned(),
                provider_call_id: "p1".to_owned(),
                name: "agent".to_owned(),
                target: "scout map the crate".to_owned(),
            },
        );
        assert_eq!(entries(&state), ["evt-step", "call-c1"]);
        assert_eq!(ids(&state.preparing_calls), ["prep-p2"]);
        super::super::reduce(
            &mut state,
            FrontendAction::ChatFailed {
                message: crate::view_model::CHAT_CANCELLED.to_owned(),
                head: String::new(),
            },
        );
        assert!(
            state.preparing_calls.is_empty(),
            "a call that never started leaves no row behind"
        );
    }

    #[test]
    fn an_agent_job_takes_its_goal_from_the_call_target() {
        use crate::view_model::{ActiveConversation, FrontendAction, WorkspaceState};

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: false,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        // The `queued`/`prompt` lines never arrive: the durable target on
        // `ToolStarted` still names the role and the short goal.
        super::super::reduce(
            &mut state,
            FrontendAction::ToolStarted {
                call_id: "c1".to_owned(),
                provider_call_id: "p1".to_owned(),
                name: "agent".to_owned(),
                target: "scout Audit Rust client crate".to_owned(),
            },
        );
        let job = state.live_jobs.first().expect("job");
        assert_eq!(job.role, "scout");
        assert_eq!(job.label, "Audit Rust client crate");
        // A `task` re-arm fills the brief without touching the step log.
        let steps = job.log.len();
        super::super::reduce(
            &mut state,
            FrontendAction::ToolProgress {
                call_id: "c1".to_owned(),
                provider_call_id: "p1".to_owned(),
                name: String::new(),
                message: "agent|scout|task|read the crate".to_owned(),
            },
        );
        let job = state.live_jobs.first().expect("job");
        assert_eq!(job.prompt, "read the crate");
        assert_eq!(job.log.len(), steps, "a re-arm adds no log line");
    }

    #[test]
    fn a_reopened_job_backfills_its_goal_from_the_replayed_row() {
        use crate::view_model::{
            ActiveConversation, ConversationEntry, EntryKind, FrontendAction, WorkspaceState,
        };

        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".to_owned(),
                branch_id: "br".to_owned(),
                head: "evt".to_owned(),
                entries: vec![ConversationEntry {
                    event_id: "call-c1".to_owned(),
                    kind: EntryKind::ToolCall,
                    text: "agent  scout Audit Rust client crate".into(),
                    call_id: Some("c1".to_owned()),
                    thinking: String::new(),
                }],
                older_before: None,
                streaming: None,
                running: true,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..WorkspaceState::default()
        };
        // A mid-turn reopen rebuilds the job from a `tool` progress line;
        // the goal comes back from the replayed call row.
        super::super::reduce(
            &mut state,
            FrontendAction::ToolProgress {
                call_id: "c1".to_owned(),
                provider_call_id: "p1".to_owned(),
                name: String::new(),
                message: "agent|scout|tool|read src/lib.rs".to_owned(),
            },
        );
        let job = state.live_jobs.first().expect("job");
        assert_eq!(job.role, "scout");
        assert_eq!(job.label, "Audit Rust client crate");
        assert!(job.step.ends_with("read src/lib.rs"), "{}", job.step);
    }
}
