use super::*;
use crate::view_model::{ActiveConversation, FrontendAction, reduce};
use mycode_app::builtin::{AgentWorkflowNode, AgentWorkflowSnapshot};
use mycode_app::{CodeModeProgress, InnerCallPhase};

mod replay;
mod revisions;

fn conversation(id: &str) -> ActiveConversation {
    ActiveConversation {
        session_id: id.into(),
        branch_id: "branch".into(),
        head: "head".into(),
        entries: Vec::new(),
        older_before: None,
        streaming: None,
        running: false,
        compacting: false,
        context_used: 0,
        context_cache: 0,
    }
}
fn state() -> WorkspaceState {
    WorkspaceState {
        sending: true,
        active: Some(conversation("session-a")),
        ..Default::default()
    }
}
fn run(status: AgentRunStatus) -> AgentRunSnapshot {
    AgentRunSnapshot {
        id: "run-stable".into(),
        call_id: "child.0".into(),
        role: "scout".into(),
        description: "Inspect cancellation".into(),
        prompt: "Read the source and report evidence".into(),
        status,
        step: "read src/lib.rs".into(),
        path: "C:/project".into(),
        started_ms: 100,
        finished_ms: status.is_terminal().then_some(1100),
        result: (status == AgentRunStatus::Completed).then(|| "Useful result".into()),
        error: (status == AgentRunStatus::Failed).then(|| "Tool failed".into()),
        artifact_path: Some("C:/results/answer.md".into()),
        workflow_id: None,
        node_id: None,
    }
}
fn packet(event: AgentStateEvent) -> String {
    format!(
        "{AGENT_STATE_PREFIX}{}",
        serde_json::to_string(&event).unwrap()
    )
}
fn progress(state: &mut WorkspaceState, message: String) {
    reduce(
        state,
        FrontendAction::ToolProgress {
            call_id: "outer-ledger".into(),
            provider_call_id: "outer-provider".into(),
            name: "agent".into(),
            message,
        },
    );
}
fn end(state: &mut WorkspaceState) {
    reduce(
        state,
        FrontendAction::ChatDone {
            head: "done".into(),
            entry: ConversationEntry {
                event_id: "done".into(),
                kind: EntryKind::AssistantMessage,
                agent_states: None,
                text: "done".into(),
                call_id: None,
                thinking: String::new(),
            },
        },
    );
}

#[test]
fn typed_terminal_results_and_selection_survive_turn_end_and_stale_packets() {
    let mut state = state();
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
    );
    reduce(
        &mut state,
        FrontendAction::SubagentWindowChanged(Some("run-stable".into())),
    );
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Completed))),
    );
    end(&mut state);
    assert_eq!(state.subagent_window.as_deref(), Some("run-stable"));
    assert!(state.live_jobs.is_empty());
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
    );
    state.sending = true;
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Queued))),
    );
    progress(&mut state, "agent|scout|tool|stale read".into());
    let saved = &state.subagent_session().unwrap().runs[0];
    assert_eq!(saved.status, AgentRunStatus::Completed);
    assert_eq!(saved.result.as_deref(), Some("Useful result"));
    assert_eq!(crate::view_model::agent_run_elapsed_ms(saved), 1000);
    // Legacy events paired with the actual completed child cannot restart it.
    tool_progress(
        &mut state,
        "child.0".into(),
        "child.0".into(),
        "agent".into(),
        "agent|scout|step|stale".into(),
    );
    assert!(!state.live_jobs.iter().any(|job| job.call_id == "child.0"));
}

#[test]
fn nested_typed_runs_keep_the_actual_child_cancel_identity_and_no_outer_row() {
    let mut state = state();
    progress(
        &mut state,
        CodeModeProgress::Progress {
            call_id: "child.0".into(),
            name: "agent".into(),
            message: packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
        }
        .encode(),
    );
    let job = &state.live_jobs[0];
    assert_eq!(job.call_id, "child.0");
    assert_eq!(job.provider_call_id, "child.0");
    assert_eq!(state.subagent_session().unwrap().runs.len(), 1);
    progress(
        &mut state,
        CodeModeProgress::Progress {
            call_id: "child.0".into(),
            name: "agent".into(),
            message: packet(AgentStateEvent::Run(run(AgentRunStatus::Failed))),
        }
        .encode(),
    );
    progress(
        &mut state,
        CodeModeProgress::Completed {
            call_id: "child.0".into(),
            name: "agent".into(),
            phase: InnerCallPhase::Completed,
            is_error: false,
        }
        .encode(),
    );
    assert!(state.live_jobs.is_empty());
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Failed
    );
    assert_eq!(
        state.subagent_session().unwrap().runs[0].error.as_deref(),
        Some("Tool failed")
    );
}

#[test]
fn synchronous_snapshots_reuse_the_ledger_row_but_cancel_the_provider_child() {
    let mut state = state();
    reduce(
        &mut state,
        FrontendAction::ToolStarted {
            call_id: "ledger-child".into(),
            provider_call_id: "child.0".into(),
            name: "agent".into(),
            target: "scout Inspect".into(),
        },
    );
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
    );
    assert_eq!(state.live_jobs.len(), 1);
    assert_eq!(state.live_jobs[0].call_id, "ledger-child");
    assert_eq!(state.live_jobs[0].provider_call_id, "child.0");
    reduce(
        &mut state,
        FrontendAction::SubagentWindowChanged(Some("ledger-child".into())),
    );
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Completed))),
    );
    assert!(state.live_jobs.is_empty());
    assert_eq!(state.job_elapsed("ledger-child"), Some(1000));
    end(&mut state);
    assert_eq!(state.subagent_window.as_deref(), Some("ledger-child"));
    let selected = state
        .subagent_session()
        .unwrap()
        .run("ledger-child")
        .unwrap();
    assert_eq!(selected.id, "run-stable");
    assert_eq!(selected.result.as_deref(), Some("Useful result"));
}

#[test]
fn cached_snapshots_are_session_scoped_and_reappear_only_in_their_own_session() {
    let mut state = state();
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Completed))),
    );
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(conversation("session-b")),
    );
    assert!(!state.has_subagents());
    assert!(state.subagent_window.is_none());
    assert!(state.job_elapsed("child.0").is_none());
    state.sending = true;
    let mut other = run(AgentRunStatus::Failed);
    other.result = None;
    progress(&mut state, packet(AgentStateEvent::Run(other)));
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Failed
    );
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(conversation("session-a")),
    );
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Completed
    );
    reduce(&mut state, FrontendAction::SessionDeleted);
    assert!(state.subagents.session("session-a").is_none());
}

#[test]
fn result_arrays_restore_workflow_handoff_and_separate_acceptance_from_node_success() {
    let mut state = state();
    let workflow = AgentWorkflowSnapshot {
        id: "workflow".into(),
        name: "migration".into(),
        revision: 0,
        status: AgentWorkflowStatus::WaitingIntegration,
        nodes: vec![
            AgentWorkflowNode {
                id: "apply-api".into(),
                role: "artisan".into(),
                description: "API slice".into(),
                dependencies: vec!["plan".into()],
                status: AgentRunStatus::Completed,
                run_id: Some("run-stable".into()),
            },
            AgentWorkflowNode {
                id: "workflow-acceptance".into(),
                role: "sentinel".into(),
                description: "Independent acceptance".into(),
                dependencies: vec!["gate".into()],
                status: AgentRunStatus::Queued,
                run_id: None,
            },
        ],
        acceptance: None,
        result: Some("Integrate retained worktrees first".into()),
    };
    let events = vec![
        AgentStateEvent::Run(run(AgentRunStatus::Completed)),
        AgentStateEvent::Workflow(workflow.clone()),
    ];
    reduce(
        &mut state,
        FrontendAction::ToolResultAppended(ConversationEntry {
            event_id: "result".into(),
            kind: EntryKind::ToolResult,
            agent_states: None,
            text: serde_json::to_string(&events).unwrap().into(),
            call_id: Some("workflow-call".into()),
            thinking: String::new(),
        }),
    );
    end(&mut state);
    let saved = &state.subagent_session().unwrap().workflows[0];
    assert_eq!(saved.status, AgentWorkflowStatus::WaitingIntegration);
    assert!(!crate::view_model::workflow_terminal(saved.status));
    assert!(saved.acceptance.is_none());
    assert_eq!(saved.nodes[1].status, AgentRunStatus::Queued);
    assert_eq!(
        saved.result.as_deref(),
        Some("Integrate retained worktrees first")
    );
    state.sending = true;
    let mut failed = workflow.clone();
    failed.status = AgentWorkflowStatus::Failed;
    failed.acceptance = Some("FAIL".into());
    failed.nodes[1].status = AgentRunStatus::Failed;
    progress(&mut state, packet(AgentStateEvent::Workflow(failed)));
    progress(&mut state, packet(AgentStateEvent::Workflow(workflow)));
    assert_eq!(
        state.subagent_session().unwrap().workflows[0].status,
        AgentWorkflowStatus::Failed
    );
    assert_eq!(
        state.subagent_session().unwrap().workflows[0]
            .acceptance
            .as_deref(),
        Some("FAIL")
    );
}

#[test]
fn a_late_terminal_retains_its_result_without_reopening_live_progress() {
    let mut state = state();
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
    );
    end(&mut state);
    progress(
        &mut state,
        packet(AgentStateEvent::Run(run(AgentRunStatus::Cancelled))),
    );
    assert!(!state.sending);
    assert!(state.live_jobs.is_empty());
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Cancelled
    );
    assert!(state.active.as_ref().unwrap().streaming.is_none());
}

#[test]
fn history_bounds_sessions_records_and_activity() {
    let mut history = crate::view_model::SubagentHistory::default();
    for session in 0..12 {
        history.session_mut(&format!("s-{session}"));
    }
    assert!(history.session("s-0").is_none());
    let current = history.session_mut("s-11");
    for index in 0..150 {
        let mut item = run(AgentRunStatus::Completed);
        item.id = format!("run-{index}");
        current.upsert_run(item);
    }
    assert_eq!(current.runs.len(), 128);
    assert_eq!(current.activity.len(), 128);
}

#[test]
fn a_failed_inner_completion_is_retained_when_no_typed_terminal_arrives() {
    let mut state = state();
    progress(
        &mut state,
        CodeModeProgress::Progress {
            call_id: "child.0".into(),
            name: "agent".into(),
            message: packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
        }
        .encode(),
    );
    progress(
        &mut state,
        CodeModeProgress::Completed {
            call_id: "child.0".into(),
            name: "agent".into(),
            phase: InnerCallPhase::Failed,
            is_error: true,
        }
        .encode(),
    );
    assert!(state.live_jobs.is_empty());
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Failed
    );
    progress(
        &mut state,
        CodeModeProgress::Progress {
            call_id: "child.0".into(),
            name: "agent".into(),
            message: packet(AgentStateEvent::Run(run(AgentRunStatus::Running))),
        }
        .encode(),
    );
    assert!(state.live_jobs.is_empty());
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Failed
    );
}

#[test]
fn legacy_inner_failure_does_not_become_success() {
    let mut state = state();
    progress(
        &mut state,
        CodeModeProgress::Started {
            call_id: "legacy.0".into(),
            name: "agent".into(),
            seq: 0,
            target: "scout inspect failure".into(),
        }
        .encode(),
    );
    progress(
        &mut state,
        CodeModeProgress::Completed {
            call_id: "legacy.0".into(),
            name: "agent".into(),
            phase: InnerCallPhase::Failed,
            is_error: true,
        }
        .encode(),
    );
    assert_eq!(
        state.subagent_session().unwrap().runs[0].status,
        AgentRunStatus::Failed
    );
}
