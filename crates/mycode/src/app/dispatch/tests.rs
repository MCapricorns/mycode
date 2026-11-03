use super::*;
use crate::app::{BranchId, HeadStamp, SessionId};

#[tokio::test]
async fn deletion_releases_retained_subagent_runtime() {
    let root = std::env::temp_dir().join(format!("mycode-delete-runs-{}", uuid::Uuid::new_v4()));
    let home = HomeLayout::from_root(&root).unwrap();
    let state = CoreState::new(home, Vec::new());
    let created = state.service.create(None).await.unwrap();
    let session_id = created.session_id.to_string();
    let runtime = Arc::new(crate::tools::builtin::AgentRuntime::new(
        root.join("results"),
    ));
    let retained = Arc::downgrade(&runtime);
    state
        .agent_runtimes
        .lock()
        .insert(session_id.clone(), runtime);
    let reply = handle(&state, BridgeCommand::DeleteSession { session_id }).await;
    assert!(
        matches!(reply, BridgeReply::SessionDeleted(Ok(()))),
        "{reply:?}"
    );
    assert!(
        retained.upgrade().is_none(),
        "deleted session retains subagent reports"
    );
    state.service.clone().shutdown().await;
    drop(state);
    for _ in 0..100 {
        if !root.exists() || std::fs::remove_dir_all(&root).is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("temporary session store remained open");
}

#[tokio::test]
async fn recall_is_rejected_during_turns_and_manual_compaction() {
    let root = std::env::temp_dir().join(format!(
        "mycode-recall-busy-{}-{}",
        std::process::id(),
        crate::core::unix_now().as_nanos()
    ));
    let home = HomeLayout::from_root(&root).unwrap();
    let state = CoreState::new(home, Vec::new());
    let session = SessionId::generate().unwrap();
    let branch = BranchId::generate().unwrap();
    for kind in [
        crate::app::state::WorkKind::Turn,
        crate::app::state::WorkKind::Compaction,
    ] {
        let token = Arc::new(tokio_util::sync::CancellationToken::new());
        state.turn_cancels.lock().insert(
            session.to_string(),
            crate::app::state::LiveWork {
                token: Arc::clone(&token),
                kind,
            },
        );
        let reply = handle(
            &state,
            BridgeCommand::RecallMessage {
                session: session.clone(),
                branch: branch.clone(),
                expected_head: HeadStamp::Empty,
                before_event: "evt1-00000000000000000000000000000001".into(),
                edit: Some("original".into()),
            },
        )
        .await;
        assert!(
            matches!(reply, BridgeReply::Recalled(Err(message)) if message.contains("finish sending or compacting"))
        );
        assert!(!token.is_cancelled(), "recall must not stop ongoing work");
    }
    state.service.clone().shutdown().await;
    drop(state);
    for _ in 0..100 {
        if !root.exists() || std::fs::remove_dir_all(&root).is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("temporary session store remained open");
}

#[tokio::test]
async fn a_second_turn_waits_for_the_first_terminal_even_after_stop() {
    let root = std::env::temp_dir().join(format!(
        "mycode-turn-admission-{}-{}",
        std::process::id(),
        crate::core::unix_now().as_nanos()
    ));
    let home = HomeLayout::from_root(root.clone()).unwrap();
    let state = Arc::new(CoreState::new(home, Vec::new()));
    let session = SessionId::parse("sess-20261019-120000-0123abcd").unwrap();
    let branch = BranchId::parse("br1-0123456789abcdef0123456789abcdef").unwrap();
    let command = BridgeCommand::ChatTurn {
        session: session.clone(),
        branch: branch.clone(),
        provider_id: "magpie".into(),
        model: "test-model".into(),
        reasoning: None,
    };
    assert!(reject_overlapping_turn(&state, &command).is_none());
    let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
    // Control is published before provider setup is polled. This models
    // two commands queued together without requiring an external gateway.
    let first = chat_turn(
        Arc::clone(&state),
        events,
        session.clone(),
        branch.clone(),
        "magpie".into(),
        "test-model".into(),
        None,
    );
    let token = Arc::clone(&state.turn_cancels.lock()[session.as_str()].token);
    for command in [
        command,
        BridgeCommand::CompactSession {
            session: session.clone(),
            branch,
            expected_head: HeadStamp::Empty,
            provider_id: "magpie".into(),
            model: "test-model".into(),
        },
    ] {
        assert!(matches!(
            reject_overlapping_turn(&state, &command),
            Some(BridgeReply::ChatStarted(Err(_)))
        ));
    }
    assert!(
        !token.is_cancelled(),
        "a duplicate start must not stop existing work"
    );
    token.cancel();
    let command = BridgeCommand::ChatTurn {
        session,
        branch: BranchId::parse("br1-0123456789abcdef0123456789abcdef").unwrap(),
        provider_id: "magpie".into(),
        model: "test-model".into(),
        reasoning: None,
    };
    assert!(reject_overlapping_turn(&state, &command).is_some());
    drop(first);
    assert!(reject_overlapping_turn(&state, &command).is_none());
    let service = state.service.clone();
    drop(state);
    service.shutdown().await;
    // Shutdown acknowledges the actor before its SQLite handle is dropped.
    for _ in 0..100 {
        if !root.exists() || std::fs::remove_dir_all(&root).is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the session actor kept the temporary home open");
}

#[tokio::test]
async fn deletion_preserves_disk_when_actor_eviction_fails() {
    let root = std::env::temp_dir().join(format!(
        "mycode-delete-fence-{}-{}",
        std::process::id(),
        crate::core::unix_now().as_nanos()
    ));
    let home = HomeLayout::from_root(&root).unwrap();
    let state = CoreState::new(home, Vec::new());
    let created = state.service.create(None).await.unwrap();
    let session_id = created.session_id.to_string();
    let relative = crate::config::locate_session_dir(&state.home, &session_id)
        .unwrap()
        .unwrap();
    let directory = state.home.owned_join(relative).unwrap();
    let marker = directory.join("keep.txt");
    std::fs::write(&marker, "preserve this session").unwrap();
    state.service.clone().shutdown().await;

    let reply = handle(&state, BridgeCommand::DeleteSession { session_id }).await;
    assert!(matches!(reply, BridgeReply::SessionDeleted(Err(_))));
    assert!(
        marker.is_file(),
        "failed eviction must not delete session data"
    );
    let listing = crate::agent::session::inspect_sessions(&state.home).unwrap();
    assert_eq!(listing.sessions.len(), 1);
    drop(state);
    for _ in 0..100 {
        if std::fs::remove_dir_all(&root).is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("temporary session store remained open");
}
