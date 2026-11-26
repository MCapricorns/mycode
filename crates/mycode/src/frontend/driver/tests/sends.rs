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

fn interrupted_entry() -> ConversationEntry {
    ConversationEntry {
        event_id: "evt1-00000000000000000000000000000003".into(),
        kind: EntryKind::AssistantMessage,
        text: format!(
            "kept partial\n\n{}",
            crate::core::interrupted_response_text("connection reset")
        )
        .into(),
        call_id: None,
        thinking: "kept reasoning".into(),
        agent_states: None,
    }
}

#[test]
fn continuing_a_reopened_failure_appends_a_request_and_preserves_the_draft() {
    let (mut driver, root) = scratch_driver("continue-saved-failure");
    let (session, _) = ready_send(&mut driver);
    let active = driver.vm.active.as_mut().unwrap();
    active.entries = vec![user_entry(), interrupted_entry()];
    let mut summary = user_entry();
    summary.event_id = "evt1-00000000000000000000000000000004".into();
    summary.text = crate::app::display_summary_text("completed tool results").into();
    active.entries.push(summary);
    active.head = interrupted_entry().event_id;
    driver.vm.composer_draft = "my next edit".into();
    assert!(driver.vm.turn_alert.is_none(), "failure survives reopening");
    assert_eq!(
        driver.turn_failure_message().as_deref(),
        Some("connection reset")
    );
    let target = driver
        .recovery_target()
        .expect("saved failure can continue");
    let mut out = Vec::new();
    driver.continue_failed_turn(&target, &mut out);
    assert_eq!(driver.vm.composer_draft, "my next edit");
    assert!(
        !out.iter()
            .any(|effect| matches!(effect, super::super::Effect::ComposerSet { .. }))
    );
    let Some(BridgeCommand::SendMessage {
        session: sent_to,
        text,
        attachments,
        ..
    }) = driver.held_send.as_ref()
    else {
        panic!("continuation must commit a new user message before requesting a model")
    };
    assert_eq!(sent_to.as_str(), session);
    assert!(!text.is_empty());
    assert!(attachments.is_empty());
    assert_eq!(
        driver.vm.active.as_ref().unwrap().entries[1],
        interrupted_entry()
    );
    driver.continue_failed_turn(&target, &mut out);
    assert_eq!(
        driver.submitted_turns.len(),
        1,
        "double click cannot resubmit"
    );
    assert!(driver.recovery_target().is_none());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_stale_continue_button_cannot_recover_a_different_head_or_chat() {
    let (mut driver, root) = scratch_driver("continue-stale-target");
    let (session, _) = ready_send(&mut driver);
    driver.vm.active.as_mut().unwrap().entries = vec![user_entry(), interrupted_entry()];
    let target = driver.recovery_target().unwrap();
    let mut out = Vec::new();
    driver.vm.active.as_mut().unwrap().head = interrupted_entry().event_id;
    driver.continue_failed_turn(&target, &mut out);
    assert!(driver.held_send.is_none());
    let target = driver.recovery_target().unwrap();
    driver.pending_open = Some(SessionId::generate().unwrap().to_string());
    driver.continue_failed_turn(&target, &mut out);
    assert!(
        driver.held_send.is_none(),
        "navigation already targets another chat"
    );
    driver.pending_open = Some(session);
    driver
        .vm
        .active
        .as_mut()
        .unwrap()
        .entries
        .push(user_entry());
    assert!(
        driver.recovery_target().is_none(),
        "a newer request supersedes the failure"
    );
    driver.continue_failed_turn(&target, &mut out);
    assert!(driver.held_send.is_none());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn continuing_without_a_valid_route_keeps_the_failed_turn_unchanged() {
    let (mut driver, root) = scratch_driver("continue-unavailable-route");
    ready_send(&mut driver);
    driver.vm.active.as_mut().unwrap().entries = vec![user_entry(), interrupted_entry()];
    let target = driver.recovery_target().unwrap();
    driver.vm.selected_model = Some("removed-model".into());
    driver.continue_failed_turn(&target, &mut Vec::new());
    assert!(driver.held_send.is_none());
    assert!(driver.submitted_turns.is_empty());
    assert_eq!(driver.recovery_target(), Some(target));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_failed_continuation_commit_never_inserts_generated_text_into_the_draft() {
    for offscreen in [false, true] {
        let (mut driver, root) = scratch_driver("continue-commit-failure");
        let (session, _) = ready_send(&mut driver);
        driver.vm.active.as_mut().unwrap().entries = vec![user_entry(), interrupted_entry()];
        let target = driver.recovery_target().unwrap();
        let mut out = Vec::new();
        driver.continue_failed_turn(&target, &mut out);
        let Some(BridgeCommand::SendMessage { text, .. }) = driver.held_send.take() else {
            panic!("missing continuation")
        };
        if offscreen {
            driver.park_conversation(&mut out);
        }
        driver.vm.composer_draft = "unsent follow-up".into();
        out.clear();
        driver.on_reply(
            ReplyScope::Send,
            BridgeReply::Sent {
                session: session.clone(),
                text,
                outcome: Err("ledger write refused".into()),
            },
            &mut out,
        );
        assert_eq!(driver.vm.composer_draft, "unsent follow-up");
        assert!(!out.iter().any(|effect| matches!(
            effect,
            super::super::Effect::ComposerSet { .. } | super::super::Effect::AttachmentsRestored(_)
        )));
        assert!(!driver.failed_send_drafts.contains_key(&session));
        assert!(!driver.submitted_turns.contains_key(&session));
        driver.shutdown();
        let _ = std::fs::remove_dir_all(root);
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
    let stats = driver
        .vm
        .last_turn
        .as_ref()
        .expect("stopped turn statistics");
    assert_eq!(
        Some(stats.model.as_str()),
        driver.vm.selected_model.as_deref()
    );
    assert_eq!(stats.requests, 0);
    assert_eq!(stats.cache, None);
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
