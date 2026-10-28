use super::*;
use crate::{BranchId, HeadStamp, SessionId};

#[tokio::test]
async fn a_second_turn_waits_for_the_first_terminal_even_after_stop() {
    let root = std::env::temp_dir().join(format!(
        "mycode-turn-admission-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
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
