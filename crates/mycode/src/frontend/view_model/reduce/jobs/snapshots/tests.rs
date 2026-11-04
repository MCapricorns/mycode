use super::*;
use crate::frontend::view_model::{ActiveConversation, FrontendAction, reduce};
use crate::tools::{CodeModeProgress, InnerCallPhase};

mod replay;

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
    let saved = &state.subagent_session().unwrap().runs[0];
    assert_eq!(saved.status, AgentRunStatus::Completed);
    assert_eq!(saved.result.as_deref(), Some("Useful result"));
}

#[test]
fn nested_typed_runs_keep_the_actual_child_cancel_identity() {
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
}
