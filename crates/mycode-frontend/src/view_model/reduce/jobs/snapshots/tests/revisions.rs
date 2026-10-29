use super::*;

fn workflow(revision: u32, status: AgentWorkflowStatus) -> AgentWorkflowSnapshot {
    let terminal = crate::view_model::workflow_terminal(status);
    AgentWorkflowSnapshot {
        id: "workflow".into(),
        name: "code-review".into(),
        revision,
        status,
        nodes: vec![AgentWorkflowNode {
            id: "review".into(),
            role: "sentinel".into(),
            description: "Independent acceptance".into(),
            dependencies: vec!["writer".into()],
            status: match status {
                AgentWorkflowStatus::Completed => AgentRunStatus::Completed,
                AgentWorkflowStatus::Failed => AgentRunStatus::Failed,
                _ => AgentRunStatus::Running,
            },
            run_id: Some(format!("review-{revision}")),
        }],
        acceptance: terminal.then(|| if revision == 0 { "FAIL" } else { "PASS" }.into()),
        result: terminal.then(|| format!("Round {revision} verdict")),
    }
}

#[test]
fn rework_replaces_workflow_nodes_and_verdict_without_reviving_old_runs() {
    let mut state = state();
    let mut old_run = run(AgentRunStatus::Failed);
    old_run.workflow_id = Some("workflow".into());
    old_run.node_id = Some("review".into());
    progress(&mut state, packet(AgentStateEvent::Run(old_run.clone())));
    progress(
        &mut state,
        packet(AgentStateEvent::Workflow(workflow(
            0,
            AgentWorkflowStatus::Failed,
        ))),
    );
    end(&mut state);
    reduce(&mut state, FrontendAction::TurnArmed);

    let active = workflow(1, AgentWorkflowStatus::Running);
    progress(
        &mut state,
        packet(AgentStateEvent::Workflow(active.clone())),
    );
    let mut new_run = old_run.clone();
    new_run.id = "fresh-run-uuid".into();
    new_run.call_id = "fresh-child-uuid".into();
    new_run.status = AgentRunStatus::Running;
    new_run.finished_ms = None;
    new_run.error = None;
    progress(&mut state, packet(AgentStateEvent::Run(new_run.clone())));
    assert_eq!(
        state.subagent_session().unwrap().workflows.as_slice(),
        std::slice::from_ref(&active)
    );
    assert_eq!(
        state.subagent_session().unwrap().run(&old_run.id),
        Some(&old_run)
    );
    assert_eq!(
        state.subagent_session().unwrap().run(&new_run.id),
        Some(&new_run)
    );

    for status in [
        AgentWorkflowStatus::Running,
        AgentWorkflowStatus::Completed,
        AgentWorkflowStatus::Failed,
    ] {
        progress(
            &mut state,
            packet(AgentStateEvent::Workflow(workflow(0, status))),
        );
        assert_eq!(
            state.subagent_session().unwrap().workflows.as_slice(),
            std::slice::from_ref(&active)
        );
    }
    let mut queued = active.clone();
    queued.status = AgentWorkflowStatus::Queued;
    queued.nodes[0].status = AgentRunStatus::Queued;
    progress(&mut state, packet(AgentStateEvent::Workflow(queued)));
    assert_eq!(
        state.subagent_session().unwrap().workflows.as_slice(),
        std::slice::from_ref(&active)
    );

    let done = workflow(1, AgentWorkflowStatus::Completed);
    progress(&mut state, packet(AgentStateEvent::Workflow(done.clone())));
    new_run.status = AgentRunStatus::Completed;
    new_run.finished_ms = Some(2100);
    new_run.result = Some("Rework accepted".into());
    progress(&mut state, packet(AgentStateEvent::Run(new_run.clone())));
    // The next turn can receive late progress from either round.
    end(&mut state);
    reduce(&mut state, FrontendAction::TurnArmed);
    progress(&mut state, packet(AgentStateEvent::Workflow(active)));
    progress(
        &mut state,
        packet(AgentStateEvent::Workflow(workflow(
            0,
            AgentWorkflowStatus::Completed,
        ))),
    );
    let mut stale_run = old_run.clone();
    stale_run.status = AgentRunStatus::Running;
    progress(&mut state, packet(AgentStateEvent::Run(stale_run)));
    assert_eq!(
        state.subagent_session().unwrap().workflows.as_slice(),
        std::slice::from_ref(&done)
    );
    assert_eq!(
        state.subagent_session().unwrap().run(&old_run.id),
        Some(&old_run)
    );
    assert_eq!(
        state.subagent_session().unwrap().run(&new_run.id),
        Some(&new_run)
    );

    // Identical workflow ids in another session have independent revisions.
    let mut other = conversation("session-b");
    other.running = true;
    reduce(&mut state, FrontendAction::ConversationOpened(other));
    let other_workflow = workflow(0, AgentWorkflowStatus::Running);
    progress(
        &mut state,
        packet(AgentStateEvent::Workflow(other_workflow.clone())),
    );
    assert_eq!(
        state.subagent_session().unwrap().workflows,
        [other_workflow]
    );
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(conversation("session-a")),
    );
    assert_eq!(state.subagent_session().unwrap().workflows, [done]);
}

#[test]
fn same_revision_stale_nodes_cannot_replace_a_terminal_verdict() {
    let mut session = crate::view_model::SubagentSession::default();
    let mut waiting = workflow(1, AgentWorkflowStatus::Running);
    waiting.nodes[0].status = AgentRunStatus::Failed;
    assert!(session.upsert_workflow(waiting.clone()));
    for status in [
        AgentRunStatus::Queued,
        AgentRunStatus::Running,
        AgentRunStatus::Completed,
    ] {
        let mut stale = waiting.clone();
        stale.nodes[0].status = status;
        assert!(session.upsert_workflow(stale));
        assert_eq!(session.workflows.as_slice(), std::slice::from_ref(&waiting));
    }
    let mut failed = waiting;
    failed.status = AgentWorkflowStatus::Failed;
    failed.acceptance = Some("FAIL".into());
    assert!(session.upsert_workflow(failed.clone()));
    let mut stale = failed.clone();
    stale.nodes[0].status = AgentRunStatus::Completed;
    stale.result = None;
    assert!(session.upsert_workflow(stale));
    assert_eq!(session.workflows, [failed]);
}

#[test]
fn a_new_revision_can_restart_queued_and_legacy_workflows_default_to_zero() {
    let failed = workflow(0, AgentWorkflowStatus::Failed);
    let mut legacy = serde_json::to_value(&failed).unwrap();
    legacy.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        serde_json::from_value::<AgentWorkflowSnapshot>(legacy).unwrap(),
        failed
    );
    let mut session = crate::view_model::SubagentSession::default();
    assert!(session.upsert_workflow(failed));
    let mut queued = workflow(1, AgentWorkflowStatus::Queued);
    queued.nodes[0].status = AgentRunStatus::Queued;
    queued.nodes[0].run_id = None;
    assert!(session.upsert_workflow(queued.clone()));
    assert_eq!(session.workflows, [queued]);
}
