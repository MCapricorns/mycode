//! Sending must finish in its original chat even while another view is open.
use crate::app::{BranchId, BridgeCommand, BridgeEvent, BridgeReply, SessionId};
use crate::config::{AppSettings, ProviderSettings};
use crate::frontend::driver::ReplyScope;
use crate::frontend::view_model::{ConversationEntry, EntryKind, FrontendAction, SettingsState};

use super::{conversation, scratch_driver};

fn ready_send(driver: &mut super::super::FrontendDriver) -> (String, String) {
    let session = SessionId::generate().unwrap().to_string();
    let branch = BranchId::generate().unwrap().to_string();
    let mut settings = SettingsState::from_settings(&AppSettings::default(), 1);
    settings.providers = vec![ProviderSettings::magpie(
        "http://127.0.0.1:3425/v1",
        vec!["original".into(), "other".into()],
        Vec::new(),
    )];
    driver.vm.settings = Some(settings);
    driver.vm.selected_provider = Some("magpie".into());
    driver.vm.selected_model = Some("original".into());
    driver.vm.active = Some(conversation(&session, &branch));
    // Hold the actual storage request, so the test controls reply ordering
    // without a network provider or racing a real missing-session error.
    driver.pending_open = Some(session.clone());
    (session, branch)
}

fn user_entry() -> ConversationEntry {
    ConversationEntry {
        event_id: "evt1-00000000000000000000000000000002".into(),
        kind: EntryKind::UserMessage,
        agent_states: None,
        text: "original prompt".into(),
        call_id: None,
        thinking: String::new(),
    }
}

#[test]
fn a_start_rejection_returns_an_interrupt_draft_to_its_original_chat() {
    for offscreen in [false, true] {
        let (mut driver, root) = scratch_driver("start-refused-interrupt");
        let (session, branch) = ready_send(&mut driver);
        let mut out = Vec::new();
        driver.submit("original prompt", &mut out);
        let _ = driver.held_send.take();
        driver.held_interrupts.insert(
            session.clone(),
            super::super::HeldInterrupt {
                send: Some("held message".into()),
            },
        );
        if offscreen {
            driver.park_conversation(&mut out);
        }
        driver.vm.composer_draft = "new draft".into();
        driver.on_reply(
            ReplyScope::SentTurn {
                session: session.clone(),
                branch,
            },
            BridgeReply::ChatStarted(Err("start refused".into())),
            &mut out,
        );
        assert!(driver.held_interrupts.is_empty());
        assert!(!driver.submitted_turns.contains_key(&session));
        if offscreen {
            assert_eq!(driver.vm.composer_draft, "new draft");
            assert_eq!(
                driver.vm.parked_steers,
                vec![(session, vec!["held message".to_owned()])]
            );
        } else {
            assert_eq!(driver.vm.composer_draft, "held message\n\nnew draft");
        }
        driver.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_delayed_send_acknowledgement_preserves_the_next_draft() {
    let (mut driver, root) = scratch_driver("send-next-draft");
    let (session, _) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original @prompt", &mut out);
    let _ = driver.held_send.take();
    assert!(driver.vm.composer_draft.is_empty());
    assert!(driver.vm.mention.is_none());
    driver.act(
        FrontendAction::ComposerChanged("next @file".into()),
        &mut out,
    );
    assert!(driver.vm.mention.is_some());
    driver.on_reply(
        ReplyScope::Send,
        BridgeReply::Sent {
            session,
            text: "original @prompt".into(),
            outcome: Ok((user_entry().event_id.clone(), user_entry())),
        },
        &mut out,
    );
    assert_eq!(driver.vm.composer_draft, "next @file");
    assert!(driver.vm.mention.is_some());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stopping_during_commit_does_not_start_the_saved_turn() {
    let (mut driver, root) = scratch_driver("send-stopped-before-commit");
    let (session, _) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    driver.pending_open = None;
    driver.cancel_chat(&mut out);
    assert!(driver.submitted_turns[&session].cancelled);
    driver.on_reply(
        ReplyScope::Send,
        BridgeReply::Sent {
            session: session.clone(),
            text: "original prompt".into(),
            outcome: Ok((user_entry().event_id.clone(), user_entry())),
        },
        &mut out,
    );
    assert!(!driver.submitted_turns.contains_key(&session));
    assert!(!driver.vm.sending);
    assert!(
        driver
            .vm
            .active
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .any(|entry| {
                entry.kind == EntryKind::UserMessage && entry.text.as_ref() == "original prompt"
            })
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stopping_a_missing_turn_releases_its_send_and_allows_another() {
    let (mut driver, root) = scratch_driver("send-missing-turn");
    let (session, _) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    // Persistence and dispatch already happened, but the core has no turn
    // left. Keep the lifecycle record that the old recovery path leaked.
    driver.submitted_turns.get_mut(&session).unwrap().command = None;
    driver.on_reply(
        ReplyScope::Session {
            session: session.clone(),
            branch: None,
        },
        BridgeReply::ChatCancelled(Err("no turn is running for this session".into())),
        &mut out,
    );
    assert!(!driver.vm.sending);
    assert!(!driver.submitted_turns.contains_key(&session));
    driver.submit("next prompt", &mut out);
    assert!(driver.vm.sending);
    assert!(driver.submitted_turns[&session].command.is_some());
    assert!(matches!(
        driver.held_send.as_ref(),
        Some(BridgeCommand::SendMessage { text, .. }) if text == "next prompt"
    ));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_interrupt_during_manual_compaction_sends_when_compaction_stops_or_finishes() {
    for (ok, message) in [(false, "compaction cancelled"), (true, "compacted")] {
        let (mut driver, root) = scratch_driver("manual-compact-interrupt");
        let (session, _) = ready_send(&mut driver);
        let mut out = Vec::new();
        driver.act(FrontendAction::CompactArmed, &mut out);
        driver.interrupt_send("replacement prompt", &mut out);
        assert!(driver.held_interrupts.contains_key(&session));
        driver.vm.composer_draft = "new unsent draft".into();
        driver.on_event(
            BridgeEvent::CompactFinished {
                session_id: session,
                ok,
                message: message.into(),
                head: user_entry().event_id,
            },
            &mut out,
        );
        assert!(driver.held_interrupts.is_empty());
        assert!(!driver.vm.compacting);
        assert!(driver.vm.sending);
        assert_eq!(driver.vm.composer_draft, "new unsent draft");
        assert!(matches!(
            driver.held_send.as_ref(),
            Some(BridgeCommand::SendMessage { text, expected_head, .. })
                if text == "replacement prompt"
                    && *expected_head == super::super::parse_head(&user_entry().event_id)
        ));
        driver.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn a_failed_manual_compaction_returns_the_interrupt_draft() {
    let (mut driver, root) = scratch_driver("manual-compact-failed-interrupt");
    let (session, _) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.act(FrontendAction::CompactArmed, &mut out);
    driver.interrupt_send("replacement prompt", &mut out);
    driver.vm.composer_draft = "new unsent draft".into();
    driver.on_event(
        BridgeEvent::CompactFinished {
            session_id: session,
            ok: false,
            message: "provider unavailable".into(),
            head: String::new(),
        },
        &mut out,
    );
    assert!(driver.held_interrupts.is_empty());
    assert!(!driver.vm.sending);
    assert!(!driver.vm.compacting);
    assert_eq!(
        driver.vm.composer_draft,
        "replacement prompt\n\nnew unsent draft"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
