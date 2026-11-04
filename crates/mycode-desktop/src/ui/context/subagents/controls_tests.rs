use super::*;

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
