use super::{conversation, scratch_driver};
use crate::driver::{Effect, RecallRequest};
use crate::view_model::{ConversationEntry, EntryKind};
use mycode_app::{BranchId, BridgeReply, SessionId};

fn user_entry() -> ConversationEntry {
    ConversationEntry {
        event_id: "evt1-00000000000000000000000000000001".into(),
        kind: EntryKind::UserMessage,
        agent_states: None,
        text: "original
多行 text"
            .into(),
        call_id: None,
        thinking: String::new(),
    }
}

#[test]
fn recall_includes_first_and_paginated_messages_and_is_disabled_while_busy() {
    let (mut driver, root) = scratch_driver("recall-gate");
    let session = SessionId::generate().unwrap();
    let branch = BranchId::generate().unwrap();
    let mut active = conversation(session.as_str(), branch.as_str());
    active.entries.push(user_entry());
    active.head = active.entries[0].event_id.clone();
    driver.vm.active = Some(active);
    assert!(driver.can_rewind_message(0));
    assert!(!driver.can_rewind_message(1));
    driver.vm.active.as_mut().unwrap().older_before = Some(user_entry().event_id);
    assert!(
        driver.can_rewind_message(0),
        "pagination never defines the branch root"
    );
    for busy in 0..4 {
        driver.vm.sending = busy == 0;
        driver.vm.compacting = busy == 1;
        let active = driver.vm.active.as_mut().unwrap();
        active.running = busy == 2;
        active.compacting = busy == 3;
        assert!(!driver.can_rewind_message(0));
        let generation = driver.recall_generation;
        let mut effects = Vec::new();
        driver.recall_message(0, &mut effects);
        assert_eq!(
            driver.recall_generation, generation,
            "busy recall must not dispatch"
        );
        assert!(effects.is_empty());
    }
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn recall_restores_exact_text_only_after_success_and_drops_stale_replies() {
    let (mut driver, root) = scratch_driver("recall-replies");
    let session = SessionId::generate().unwrap();
    let branch = BranchId::generate().unwrap();
    let mut active = conversation(session.as_str(), branch.as_str());
    active.entries.push(user_entry());
    active.head = active.entries[0].event_id.clone();
    driver.vm.active = Some(active.clone());
    let mut effects = Vec::new();
    driver.recall_message(0, &mut effects);
    assert_eq!(
        driver.recall_generation, 1,
        "the first message dispatches a recall"
    );
    assert!(driver.pending_composer_prefill.is_none());
    assert!(effects.is_empty());
    let request = RecallRequest {
        session: session.clone(),
        branch: branch.clone(),
        head: active.head.clone(),
        edit: user_entry().text.to_string(),
        generation: driver.recall_generation,
    };
    let mut rewound = conversation(session.as_str(), BranchId::generate().unwrap().as_str());
    rewound.head = "empty".into();
    let reply =
        || BridgeReply::Recalled(Ok((Box::new(rewound.clone()), Some(request.edit.clone()))));
    for stale in 0..8 {
        driver.vm.active = Some(active.clone());
        driver.vm.sending = stale == 0;
        driver.vm.compacting = stale == 1;
        driver.pending_open = (stale == 2).then(|| session.to_string());
        driver.recall_generation = request.generation + u64::from(stale == 3);
        let current = driver.vm.active.as_mut().unwrap();
        match stale {
            4 => current.head = "newer".into(),
            5 => current.session_id = "another".into(),
            6 => current.branch_id = "another".into(),
            7 => current.compacting = true,
            _ => {}
        }
        driver.resolve_recall_reply(request.clone(), reply(), &mut effects);
        assert!(
            effects.is_empty(),
            "stale reply {stale} changed the composer"
        );
        assert!(driver.pending_composer_prefill.is_none());
    }
    driver.vm.active = Some(active.clone());
    driver.vm.sending = false;
    driver.vm.compacting = false;
    driver.pending_open = None;
    driver.recall_generation = request.generation;
    driver.resolve_recall_reply(
        request.clone(),
        BridgeReply::Recalled(Err("stale head".into())),
        &mut effects,
    );
    assert!(driver.pending_composer_prefill.is_none());
    assert_eq!(
        driver.vm.active.as_ref().unwrap().branch_id,
        active.branch_id
    );
    effects.clear();
    driver.resolve_recall_reply(request.clone(), reply(), &mut effects);
    assert_eq!(
        driver.vm.active.as_ref().unwrap().branch_id,
        rewound.branch_id
    );
    assert!(driver.vm.active.as_ref().unwrap().entries.is_empty());
    assert_eq!(
        driver.pending_composer_prefill.as_deref(),
        Some(request.edit.as_str())
    );
    assert!(effects.iter().any(|effect| matches!(effect, Effect::ComposerSet { text, focus: true } if text == &request.edit)));
    // Recall retains the existing text-only composer contract.
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::AttachmentsRestored(_)))
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
