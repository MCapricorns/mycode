use super::*;
use mycode_app::session::{EventKind, SessionCallId, SessionService};
use mycode_app::{
    BridgeCommand, BridgeReply, ContentBlock, CoreBridge, HeadStamp, HomeLayout, TextBlock,
    ToolResultMessage,
};

#[test]
fn typed_results_take_precedence_over_compatibility_json_without_changing_the_answer() {
    let mut state = state();
    let answer = serde_json::to_string(&AgentStateEvent::Run(run(AgentRunStatus::Failed))).unwrap();
    let entry = ConversationEntry {
        event_id: "result".into(),
        kind: EntryKind::ToolResult,
        text: answer.clone().into(),
        call_id: Some("outer".into()),
        thinking: String::new(),
        agent_states: Some(vec![AgentStateEvent::Run(run(AgentRunStatus::Completed))].into()),
    };
    reduce(&mut state, FrontendAction::ToolResultAppended(entry));
    assert_eq!(
        state.subagent_session().unwrap().runs,
        [run(AgentRunStatus::Completed)]
    );
    assert_eq!(
        state.active.as_ref().unwrap().entries[0].text.as_ref(),
        answer
    );
}

#[test]
fn durable_agent_and_code_mode_results_restore_typed_vm_state_after_reopen() {
    let root = std::env::temp_dir().join(format!(
        "mycode-agent-replay-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    let home = HomeLayout::from_root(&root).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sessions = runtime.block_on(async {
        let service = SessionService::new(&home);
        let mut sessions = Vec::new();
        for (name, status) in [("agent", AgentRunStatus::Completed), ("run_code", AgentRunStatus::Failed), ("wait_code", AgentRunStatus::Completed)] {
            let created = service.create(None).await.unwrap();
            let call_id = SessionCallId::generate().unwrap();
            let mut head = HeadStamp::Empty;
            let snapshot = run(status);
            let mut details = serde_json::json!({"agent_states": [AgentStateEvent::Run(snapshot.clone())]});
            if name != "agent" {
                details["inner_calls"] = serde_json::json!([
                    {"seq": 0, "name": "agent", "target": "scout Inspect cancellation", "phase": "completed", "is_error": false}
                ]);
            }
            let result = ToolResultMessage {
                tool_call_id: "provider-outer".into(),
                content: vec![ContentBlock::Text(TextBlock::new("Readable tool answer"))],
                is_error: false,
                details: Some(details),
            };
            for (kind, payload) in [
                (EventKind::ToolCall, serde_json::to_vec(&serde_json::json!({"name": name, "target": "scout Inspect cancellation"})).unwrap()),
                (EventKind::ToolResult, serde_json::to_vec(&result).unwrap()),
            ] {
                let reservation = service.reserve_event(&created.session_id, &created.branch_id, kind, Some(call_id.clone()), &payload).await.unwrap();
                head = service.append(&created.session_id, &created.branch_id, &head, &reservation).await.unwrap().head;
            }
            sessions.push((created.session_id, call_id, snapshot, name));
        }
        service.shutdown().await;
        sessions
    });
    let (mut bridge, _events) = CoreBridge::start(home);
    let mut cached = WorkspaceState::default();
    for (id, ledger_id, expected, name) in &sessions {
        let reply = runtime.block_on(bridge.request(BridgeCommand::OpenSession(id.clone())));
        let BridgeReply::Conversation(Ok(opened)) = reply else {
            panic!("open failed: {reply:?}")
        };
        let result = opened
            .entries
            .iter()
            .find(|entry| entry.kind == EntryKind::ToolResult)
            .unwrap()
            .clone();
        assert_eq!(result.call_id.as_deref(), Some(ledger_id.as_str()));
        assert!(result.text.starts_with("Readable tool answer"));
        assert!(!result.text.contains("agent_states"));
        assert_eq!(
            result.text.contains(mycode_app::CODE_CALLS_HEADING),
            *name != "agent"
        );
        // Feed the actual app projection through both live delivery and cold replay.
        let mut live = conversation(id.as_str());
        live.running = true;
        reduce(&mut cached, FrontendAction::ConversationOpened(live));
        reduce(&mut cached, FrontendAction::ToolResultAppended(result));
        assert_eq!(
            cached.subagent_session().unwrap().runs.as_slice(),
            std::slice::from_ref(expected)
        );
        reduce(
            &mut cached,
            FrontendAction::ConversationOpened(opened.clone()),
        );
        assert_eq!(
            cached.subagent_session().unwrap().runs.as_slice(),
            std::slice::from_ref(expected)
        );
        let mut cold = WorkspaceState::default();
        reduce(&mut cold, FrontendAction::ConversationOpened(opened));
        assert_eq!(
            cold.subagent_session().unwrap().runs.as_slice(),
            std::slice::from_ref(expected)
        );
        assert!(cold.live_jobs.is_empty());
        let mut paged = WorkspaceState::default();
        let mut tail = conversation(id.as_str());
        tail.older_before = Some("cursor".into());
        reduce(&mut paged, FrontendAction::ConversationOpened(tail));
        let older = FrontendAction::OlderLoaded {
            session_id: id.to_string(),
            entries: cold.active.as_ref().unwrap().entries.clone(),
            older_before: None,
            requested_before: "cursor".into(),
        };
        reduce(&mut paged, older);
        assert_eq!(
            paged.subagent_session().unwrap().runs.as_slice(),
            std::slice::from_ref(expected)
        );
        // A page from another session must not seed the selected session's cache.
        reduce(
            &mut paged,
            FrontendAction::OlderLoaded {
                session_id: "another-session".into(),
                entries: cold.active.as_ref().unwrap().entries.clone(),
                older_before: None,
                requested_before: "cursor".into(),
            },
        );
        assert!(paged.subagents.session("another-session").is_none());
    }
    // All sessions intentionally reuse the same run id; each keeps its own verdict.
    let reply = runtime.block_on(bridge.request(BridgeCommand::OpenSession(sessions[0].0.clone())));
    let BridgeReply::Conversation(Ok(opened)) = reply else {
        panic!("open failed: {reply:?}")
    };
    reduce(&mut cached, FrontendAction::ConversationOpened(opened));
    assert_eq!(
        cached.subagent_session().unwrap().runs,
        [sessions[0].2.clone()]
    );
    for (id, _, snapshot, _) in &sessions {
        assert_eq!(
            cached
                .subagents
                .session(id.as_str())
                .unwrap()
                .runs
                .as_slice(),
            std::slice::from_ref(snapshot)
        );
    }
    bridge.shutdown();
    drop(runtime);
    std::fs::remove_dir_all(root).unwrap();
}
