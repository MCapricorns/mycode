//! Sending must finish in its original chat even while another view is open.
use crate::app::{BranchId, BridgeCommand, BridgeEvent, BridgeReply, SessionId};
use crate::config::{AppSettings, ProviderSettings};
use crate::frontend::driver::{Effect, ReplyScope, ToastKind};
use crate::frontend::view_model::{ConversationEntry, EntryKind, FrontendAction, SettingsState};

use super::{code_attachment, conversation, scratch_driver};

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
fn a_send_keeps_its_route_and_starts_after_the_project_is_parked() {
    let (mut driver, root) = scratch_driver("send-parked");
    let (session, branch) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    driver.park_conversation(&mut out);
    driver.vm.selected_model = Some("other".into());
    let submitted = driver.submitted_turns.get(&session).unwrap();
    assert!(matches!(
        submitted.command.as_ref(),
        Some(Ok(BridgeCommand::ChatTurn {
            session: origin,
            branch: origin_branch,
            provider_id,
            model,
            ..
        })) if origin.as_str() == session && origin_branch.as_str() == branch
            && provider_id == "magpie" && model == "original"
    ));
    assert!(ReplyScope::Send.applies(&driver.vm, None));
    out.clear();
    driver.on_reply(
        ReplyScope::Send,
        BridgeReply::Sent {
            session: session.clone(),
            text: "original prompt".into(),
            outcome: Ok((user_entry().event_id.clone(), user_entry())),
        },
        &mut out,
    );
    assert!(driver.vm.active.is_none(), "the parked desk stays parked");
    assert!(driver.submitted_turns[&session].command.is_none());
    assert!(
        out.is_empty(),
        "an off-screen send must not change this composer"
    );
    // A terminal event must release the origin even while it is off screen.
    driver.on_event(
        BridgeEvent::ChatFailed {
            session_id: session.clone(),
            message: "fixture terminal".into(),
            head: String::new(),
        },
        &mut out,
    );
    assert!(!driver.submitted_turns.contains_key(&session));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_offscreen_failed_send_restores_text_and_attachments_only_in_its_own_chat() {
    let (mut driver, root) = scratch_driver("send-failed-offscreen");
    let (session, branch) = ready_send(&mut driver);
    let mut out = Vec::new();
    assert!(
        driver
            .submit_with("original prompt", code_attachment(), &mut out)
            .is_none()
    );
    let _ = driver.held_send.take();
    driver.pending_open = None;
    driver.act(
        FrontendAction::ConversationOpened(conversation("other-session", "other-branch")),
        &mut out,
    );
    driver.vm.composer_draft = "other draft".into();
    out.clear();
    driver.on_reply(
        ReplyScope::Send,
        BridgeReply::Sent {
            session: session.clone(),
            text: "original prompt".into(),
            outcome: Err("disk full".into()),
        },
        &mut out,
    );
    assert_eq!(driver.vm.composer_draft, "other draft");
    assert!(driver.pending_send_attachments.is_empty());
    assert!(
        !out.iter()
            .any(|effect| matches!(effect, Effect::AttachmentsRestored(_)))
    );
    driver.pending_open = Some(session.clone());
    out.clear();
    driver.on_reply(
        ReplyScope::Open(session.clone()),
        BridgeReply::Conversation(Ok(conversation(&session, &branch))),
        &mut out,
    );
    assert_eq!(driver.vm.composer_draft, "original prompt");
    assert!(driver.failed_send_drafts.is_empty());
    assert!(out.iter().any(|effect| matches!(
        effect, Effect::AttachmentsRestored(attachments)
            if attachments.files.len() == 1 && attachments.files[0].name == "main.rs"
    )));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reopening_a_pending_send_keeps_it_busy_instead_of_accepting_a_second_send() {
    let (mut driver, root) = scratch_driver("send-reopened");
    let (session, branch) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    driver.park_conversation(&mut out);
    driver.pending_open = Some(session.clone());
    driver.on_reply(
        ReplyScope::Open(session.clone()),
        BridgeReply::Conversation(Ok(conversation(&session, &branch))),
        &mut out,
    );
    assert!(driver.vm.sending);
    let returned = driver.submit_with("second prompt", code_attachment(), &mut out);
    assert!(returned.is_some_and(|attachments| !attachments.is_empty()));
    assert_eq!(driver.submitted_turns.len(), 1);
    assert!(driver.submitted_turns[&session].command.is_some());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_offscreen_start_rejection_clears_only_its_originating_send() {
    let (mut driver, root) = scratch_driver("send-start-refused");
    let (session, branch) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    driver.park_conversation(&mut out);
    driver.vm.composer_draft = "new draft".into();
    out.clear();
    let scope = ReplyScope::SentTurn {
        session: session.clone(),
        branch,
    };
    assert!(scope.applies(&driver.vm, None));
    driver.on_reply(
        scope,
        BridgeReply::ChatStarted(Err("start refused".into())),
        &mut out,
    );
    assert!(!driver.submitted_turns.contains_key(&session));
    assert_eq!(driver.vm.composer_draft, "new draft");
    assert!(out.iter().any(|effect| matches!(
        effect, Effect::Toast { text, .. } if text == "start refused"
    )));
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
fn a_missing_turn_does_not_swallow_the_interrupts_held_message() {
    let (mut driver, root) = scratch_driver("send-missing-turn-interrupt");
    let (session, _) = ready_send(&mut driver);
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    driver.submitted_turns.get_mut(&session).unwrap().command = None;
    driver.held_interrupt = Some(super::super::HeldInterrupt {
        session_id: session.clone(),
        send: Some("replacement prompt".into()),
    });
    driver.vm.composer_draft = "new unsent draft".into();
    driver.on_reply(
        ReplyScope::Session {
            session: session.clone(),
            branch: None,
        },
        BridgeReply::ChatCancelled(Err("no turn is running for this session".into())),
        &mut out,
    );
    assert!(driver.held_interrupt.is_none());
    assert!(driver.submitted_turns[&session].command.is_some());
    assert_eq!(driver.vm.composer_draft, "new unsent draft");
    assert!(matches!(
        driver.held_send.as_ref(),
        Some(BridgeCommand::SendMessage { text, .. }) if text == "replacement prompt"
    ));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_unavailable_captured_route_settles_the_committed_send_without_a_fallback() {
    let (mut driver, root) = scratch_driver("send-unavailable-route");
    let (session, _) = ready_send(&mut driver);
    driver.vm.selected_model = Some("unavailable-model".into());
    let mut out = Vec::new();
    driver.submit("original prompt", &mut out);
    let _ = driver.held_send.take();
    assert!(matches!(
        driver.submitted_turns[&session].command.as_ref(),
        Some(Err(_))
    ));
    // Selecting another model after submit must not silently reroute the
    // accepted message; the originally captured rejection owns this send.
    driver.vm.selected_model = Some("other".into());
    out.clear();
    driver.on_reply(
        ReplyScope::Send,
        BridgeReply::Sent {
            session: session.clone(),
            text: "original prompt".into(),
            outcome: Ok((user_entry().event_id.clone(), user_entry())),
        },
        &mut out,
    );
    assert!(!driver.vm.sending);
    assert!(!driver.submitted_turns.contains_key(&session));
    // The driver consumes the reducer error into a user-visible toast.
    assert!(out.iter().any(|effect| matches!(
        effect, Effect::Toast { kind: ToastKind::Error, text }
            if text.contains("unavailable") || text.contains("不可用")
    )));
    assert_eq!(driver.vm.selected_model.as_deref(), Some("other"));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
