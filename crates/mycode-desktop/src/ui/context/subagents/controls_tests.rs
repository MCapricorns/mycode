use super::*;
use mycode_frontend::builtin::{AgentStateEvent, AgentWorkflowNode};

pub(super) fn reworked_state() -> WorkspaceState {
    let mut state = WorkspaceState {
        sending: true,
        ..Default::default()
    };
    let mut previous = run(AgentRunStatus::Failed);
    previous.description = "Research document".into();
    previous.error = Some("FAIL: research document needs evidence".into());
    previous.result = Some("First research document".into());
    let mut workflow = AgentWorkflowSnapshot {
        id: "workflow".into(),
        name: "research".into(),
        revision: 0,
        status: AgentWorkflowStatus::Failed,
        nodes: vec![AgentWorkflowNode {
            id: "apply-api".into(),
            role: "artisan".into(),
            description: "Revise research document".into(),
            dependencies: Vec::new(),
            status: AgentRunStatus::Failed,
            run_id: Some(previous.id.clone()),
        }],
        acceptance: Some("FAIL".into()),
        result: Some("Needs revision".into()),
    };
    let mut current = previous.clone();
    current.id = "current-run-id".into();
    current.call_id = "current-child".into();
    current.status = AgentRunStatus::Running;
    current.step = "Adding review evidence".into();
    current.result = None;
    current.error = None;
    current.finished_ms = None;
    let old_workflow = workflow.clone();
    workflow.revision = 1;
    workflow.status = AgentWorkflowStatus::Running;
    workflow.acceptance = None;
    workflow.result = None;
    workflow.nodes[0].status = AgentRunStatus::Running;
    workflow.nodes[0].run_id = Some(current.id.clone());
    let mut stale_run = previous.clone();
    stale_run.status = AgentRunStatus::Running;
    for event in [
        AgentStateEvent::Run(previous),
        AgentStateEvent::Workflow(old_workflow.clone()),
        AgentStateEvent::Workflow(workflow),
        AgentStateEvent::Run(current),
        AgentStateEvent::Workflow(old_workflow),
        AgentStateEvent::Run(stale_run),
    ] {
        crate::view_model::reduce(
            &mut state,
            FrontendAction::ToolResultAppended(crate::view_model::ConversationEntry {
                event_id: "workflow-event".into(),
                kind: crate::view_model::EntryKind::ToolResult,
                text: "Readable result".into(),
                call_id: Some("workflow-ledger".into()),
                thinking: String::new(),
                agent_states: Some(vec![event].into()),
            }),
        );
    }
    state
}

#[test]
fn rework_shows_current_progress_and_keeps_previous_results_selectable() {
    let mut state = reworked_state();
    let items = rows(&state);
    let workflow_row = items.iter().find(|row| row.id == "workflow").unwrap();
    assert_eq!(workflow_row.activity, "Adding review evidence");
    assert_eq!(workflow_row.tone, StatusTone::Active);
    assert!(workflow_row.status.contains('1'));
    let session = state.subagent_session().unwrap();
    let workflow = &session.workflows[0];
    assert!(!acceptance_text(workflow).contains("FAIL"));
    let previous = previous_workflow_runs(workflow, session);
    assert_eq!(previous.len(), 1);
    let previous_id = previous[0].id.clone();
    assert_eq!(previous[0].status, AgentRunStatus::Failed);
    assert_eq!(
        previous[0].result.as_deref(),
        Some("First research document")
    );
    crate::view_model::reduce(
        &mut state,
        FrontendAction::SubagentWindowChanged(Some(previous_id)),
    );
    let selected = state
        .subagent_session()
        .unwrap()
        .run(state.subagent_window.as_deref().unwrap())
        .unwrap();
    assert_eq!(selected.result.as_deref(), Some("First research document"));
    assert_eq!(
        selected.error.as_deref(),
        Some("FAIL: research document needs evidence")
    );
    assert_eq!(state.subagent_session().unwrap().workflows[0].revision, 1);
}

fn run(status: AgentRunStatus) -> AgentRunSnapshot {
    AgentRunSnapshot {
        id: "display-run-id".into(),
        call_id: "actual-child.3".into(),
        role: "artisan".into(),
        description: "Migrate API".into(),
        prompt: String::new(),
        status,
        step: "checking".into(),
        path: String::new(),
        started_ms: 100,
        finished_ms: status.is_terminal().then_some(1100),
        result: None,
        error: None,
        artifact_path: None,
        workflow_id: Some("workflow".into()),
        node_id: Some("apply-api".into()),
    }
}

#[test]
fn stop_is_available_only_for_active_children_and_uses_the_child_identity() {
    for status in [AgentRunStatus::Queued, AgentRunStatus::Running] {
        assert_eq!(
            run_row(&run(status)).cancel_id.as_deref(),
            Some("actual-child.3")
        );
    }
    for status in [
        AgentRunStatus::Settling,
        AgentRunStatus::Completed,
        AgentRunStatus::Failed,
        AgentRunStatus::Cancelled,
        AgentRunStatus::Blocked,
    ] {
        assert!(run_row(&run(status)).cancel_id.is_none());
    }
}

#[test]
fn workflow_and_typed_child_rows_do_not_duplicate_the_legacy_projection() {
    let mut state = WorkspaceState {
        sending: true,
        ..Default::default()
    };
    // A real protocol packet enters through the same reducer as workflow progress.
    crate::view_model::reduce(&mut state, FrontendAction::ToolProgress {
        call_id: "workflow-ledger".into(), provider_call_id: "workflow-provider".into(), name: "agent_workflow".into(),
        message: r#"agent_state|{"kind":"run","snapshot":{"id":"display-run-id","call_id":"actual-child.3","role":"artisan","description":"Migrate API","prompt":"","status":"running","step":"checking","path":"","started_ms":100,"finished_ms":null,"result":null,"error":null,"artifact_path":null,"workflow_id":"workflow","node_id":"apply-api"}}"#.into(),
    });
    let items = rows(&state);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "display-run-id");
    assert_eq!(items[0].cancel_id.as_deref(), Some("actual-child.3"));
}
