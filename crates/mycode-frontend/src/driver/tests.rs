use super::ReplyScope;
use crate::view_model::{FrontendAction, WorkspaceState, reduce};
use mycode_app::ActiveConversation;

fn conversation(session: &str, branch: &str) -> ActiveConversation {
    ActiveConversation {
        session_id: session.to_owned(),
        branch_id: branch.to_owned(),
        head: "empty".to_owned(),
        entries: Vec::new(),
        older_before: None,
        streaming: None,
        running: false,
        compacting: false,
        context_used: 0,
        context_cache: 0,
    }
}

#[test]
fn rejected_start_releases_sending_and_removes_the_waiting_bubble() {
    let mut state = WorkspaceState {
        active: Some(conversation("a", "branch")),
        ..WorkspaceState::default()
    };
    reduce(&mut state, FrontendAction::TurnArmed);
    assert!(state.sending);
    assert!(state.active.as_ref().unwrap().streaming.is_some());
    reduce(
        &mut state,
        FrontendAction::ChatFailed {
            message: "start refused".to_owned(),
            head: String::new(),
        },
    );
    assert!(!state.sending);
    assert!(state.active.as_ref().unwrap().streaming.is_none());
    assert_eq!(state.error.as_deref(), Some("start refused"));
    assert!(crate::view_model::open_session_is_empty(&state));
}

#[test]
fn late_turn_and_recall_replies_cannot_change_another_session_or_branch() {
    let mut state = WorkspaceState {
        active: Some(conversation("a", "branch")),
        ..WorkspaceState::default()
    };
    let scope = ReplyScope::Session {
        session: "a".to_owned(),
        branch: Some("branch".to_owned()),
    };
    assert!(scope.applies(&state, None));
    state.active = Some(conversation("b", "branch"));
    assert!(!scope.applies(&state, None));
    state.active = Some(conversation("a", "rewound"));
    assert!(!scope.applies(&state, None));
}

#[test]
fn superseded_open_and_file_search_replies_are_ignored() {
    let mut state = WorkspaceState {
        active: Some(conversation("a", "branch")),
        ..WorkspaceState::default()
    };
    assert!(!ReplyScope::Open("old".to_owned()).applies(&state, Some("new")));
    assert!(!ReplyScope::Open("old".to_owned()).applies(&state, None));
    assert!(ReplyScope::Open("new".to_owned()).applies(&state, Some("new")));
    reduce(
        &mut state,
        FrontendAction::ComposerChanged("@second".to_owned()),
    );
    assert!(
        !ReplyScope::Mention {
            session: "a".to_owned(),
            query: "first".to_owned()
        }
        .applies(&state, None)
    );
    assert!(
        ReplyScope::Mention {
            session: "a".to_owned(),
            query: "second".to_owned()
        }
        .applies(&state, None)
    );
}

#[test]
fn a_chat_opened_mid_turn_reloads_from_the_ledger_when_the_turn_ends() {
    use mycode_app::{BranchId, BridgeEvent, BridgeReply, ConversationEntry, EntryKind, SessionId};

    let root = std::env::temp_dir().join(format!(
        "mycode-driver-resync-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = mycode_app::HomeLayout::from_root(&root).expect("home");
    let mut driver = super::FrontendDriver::start(home, || {});
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    let mut running = conversation(&session, &branch);
    running.running = true;
    let mut out = Vec::new();
    driver.pending_open = Some(session.clone());
    driver.on_reply(
        ReplyScope::Open(session.clone()),
        BridgeReply::Conversation(Ok(running)),
        &mut out,
    );
    assert!(driver.vm.sending, "the reopened turn is still running");
    assert!(driver.pending_open.is_none());
    driver.on_event(
        BridgeEvent::ChatDone {
            session_id: session.clone(),
            head: "empty".to_owned(),
            entry: ConversationEntry {
                event_id: "evt".to_owned(),
                kind: EntryKind::AssistantMessage,
                text: "done".into(),
                call_id: None,
                thinking: String::new(),
            },
        },
        &mut out,
    );
    assert_eq!(
        driver.pending_open.as_deref(),
        Some(session.as_str()),
        "the turn end reopens the chat to pick up steps the open missed"
    );
    assert!(driver.resync_on_turn_end.is_none());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A driver on a scratch home, plus that home for cleanup.
fn scratch_driver(label: &str) -> (super::FrontendDriver, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "mycode-driver-{label}-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = mycode_app::HomeLayout::from_root(&root).expect("home");
    (super::FrontendDriver::start(home, || {}), root)
}

/// A reopen during `/compact` restores the compaction, not a running turn.
/// `CompactFinished` is the only event that ends it; a reopen that set
/// `sending` left the composer stuck on Stop after the compaction ended.
#[test]
fn a_chat_opened_mid_compact_unlocks_when_the_compaction_finishes() {
    use mycode_app::{BranchId, BridgeEvent, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("compact-reopen");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    let mut compacting = conversation(&session, &branch);
    compacting.compacting = true;
    let mut out = Vec::new();
    driver.pending_open = Some(session.clone());
    driver.on_reply(
        ReplyScope::Open(session.clone()),
        BridgeReply::Conversation(Ok(compacting)),
        &mut out,
    );
    assert!(driver.vm.compacting, "the composer stays locked");
    assert!(!driver.vm.sending, "a compaction is not a running turn");
    driver.on_event(
        BridgeEvent::CompactFinished {
            session_id: session.clone(),
            message: "compacted".to_owned(),
            ok: true,
            head: "empty".to_owned(),
        },
        &mut out,
    );
    assert!(!driver.vm.compacting);
    assert!(!driver.vm.sending);
    assert_eq!(
        driver.pending_open.as_deref(),
        Some(session.as_str()),
        "the end reloads the rows the open may have missed"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Stop on a turn the core no longer runs settles the composer, and the
/// reload a mid-turn open still owed runs now instead of after the next
/// turn, where it reopened that turn as running.
#[test]
fn stop_on_a_turn_that_is_gone_runs_the_owed_reload() {
    use mycode_app::{BranchId, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("stop-settle");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    let mut running = conversation(&session, &branch);
    running.running = true;
    let mut out = Vec::new();
    driver.pending_open = Some(session.clone());
    driver.on_reply(
        ReplyScope::Open(session.clone()),
        BridgeReply::Conversation(Ok(running)),
        &mut out,
    );
    assert!(driver.vm.sending);
    driver.on_reply(
        ReplyScope::Session {
            session: session.clone(),
            branch: None,
        },
        BridgeReply::ChatCancelled(Err("no turn is running for this session".to_owned())),
        &mut out,
    );
    assert!(!driver.vm.sending, "Stop settles a turn the core lost");
    assert!(driver.resync_on_turn_end.is_none());
    assert_eq!(driver.pending_open.as_deref(), Some(session.as_str()));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A send while the chat on screen is being reopened waits for that
/// snapshot. Applied after the send's reply, the older snapshot wiped the
/// message off the screen.
#[test]
fn a_send_during_a_reopen_of_the_same_chat_waits_for_the_snapshot() {
    use mycode_app::{BranchId, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("held-send");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    driver.pending_open = Some(session.clone());
    let mut out = Vec::new();
    driver.submit("next question", &mut out);
    assert!(driver.vm.sending, "the turn arms right away");
    assert!(driver.held_send.is_some(), "the send waits for the reopen");
    driver.on_reply(
        ReplyScope::Open(session.clone()),
        BridgeReply::Conversation(Ok(conversation(&session, &branch))),
        &mut out,
    );
    assert!(
        driver.held_send.is_none(),
        "the snapshot is in; the send left"
    );
    assert!(driver.vm.sending);
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// The composer is cleared when a send goes out, so a send the core could
/// not write hands its text back instead of losing it.
#[test]
fn a_failed_send_puts_its_text_back_in_the_composer() {
    use mycode_app::{BranchId, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("failed-send");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    let mut out = Vec::new();
    driver.submit("lost words", &mut out);
    assert!(driver.vm.composer_draft.is_empty());
    driver.act(
        FrontendAction::ComposerChanged("typed after".to_owned()),
        &mut out,
    );
    driver.on_reply(
        ReplyScope::Session {
            session: session.clone(),
            branch: Some(branch.clone()),
        },
        BridgeReply::Sent {
            session: session.clone(),
            text: "lost words".to_owned(),
            outcome: Err("disk full".to_owned()),
        },
        &mut out,
    );
    assert!(!driver.vm.sending);
    assert_eq!(driver.vm.composer_draft, "lost words\n\ntyped after");
    assert!(out.iter().any(|effect| matches!(
        effect,
        super::Effect::ComposerSet { text, .. } if text == "lost words\n\ntyped after"
    )));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A failed send hands its attachments back too, not only the text.
#[test]
fn a_failed_send_puts_its_attachments_back() {
    use mycode_app::{BranchId, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("failed-send-attachments");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    let mut out = Vec::new();
    let returned = driver.submit_with("look", code_attachment(), &mut out);
    assert!(returned.is_none(), "the send took the attachments");
    driver.on_reply(
        ReplyScope::Session {
            session: session.clone(),
            branch: Some(branch.clone()),
        },
        BridgeReply::Sent {
            session: session.clone(),
            text: "look".to_owned(),
            outcome: Err("disk full".to_owned()),
        },
        &mut out,
    );
    let restored = out
        .iter()
        .find_map(|effect| match effect {
            super::Effect::AttachmentsRestored(attachments) => Some(attachments),
            _ => None,
        })
        .expect("attachments restored");
    assert_eq!(restored.files.len(), 1);
    assert_eq!(restored.files[0].name, "main.rs");
    assert!(driver.pending_send_attachments.is_empty());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// An interrupt while the turn summarizes for an automatic compaction stops
/// the turn once the summary ends, not before: stopping it threw the
/// summary away, and the send after it summarized the same history again.
#[test]
fn an_interrupt_during_auto_compaction_waits_for_the_summary() {
    use mycode_app::{BranchId, BridgeEvent, SessionId};

    let (mut driver, root) = scratch_driver("compact-interrupt");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    let mut out = Vec::new();
    driver.act(FrontendAction::TurnArmed, &mut out);
    let compacting = |active| BridgeEvent::ContextCompacting {
        session_id: session.clone(),
        active,
    };
    driver.on_event(compacting(true), &mut out);
    driver.interrupt_send("change of plan", &mut out);
    {
        let held = driver.held_interrupt.as_ref().expect("the stop is held");
        assert_eq!(held.session_id, session);
        assert_eq!(held.send.as_deref(), Some("change of plan"));
    }
    // The message left the composer while the stop waits.
    assert!(out.iter().any(|effect| matches!(
        effect,
        super::Effect::ComposerSet { text, .. } if text.is_empty()
    )));
    driver.on_event(compacting(false), &mut out);
    assert!(!driver.vm.auto_compacting, "the summary ended");
    assert!(
        driver.held_interrupt.is_some(),
        "the message waits for the turn to be gone"
    );
    driver.on_event(
        BridgeEvent::ChatFailed {
            session_id: session.clone(),
            message: mycode_app::CHAT_CANCELLED.to_owned(),
            head: String::new(),
        },
        &mut out,
    );
    assert!(
        driver.held_interrupt.is_none(),
        "the stop landed; the held message went out"
    );
    assert!(driver.vm.sending, "the held message started the next turn");
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_repair_after_the_startup_toast_still_toasts_once() {
    let root = std::env::temp_dir().join(format!(
        "mycode-driver-repairs-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = mycode_app::HomeLayout::from_root(&root).expect("home");
    let mut driver = super::FrontendDriver::start(home, || {});
    let repair = |path: &'static str| mycode_app::DocumentRepair {
        path,
        backup: format!("{path}.bak"),
    };
    let toasts = |driver: &mut super::FrontendDriver| {
        driver
            .inbox_tx
            .send(super::DriverMsg::RepairToastTick)
            .expect("tick");
        let mut out = Vec::new();
        driver.poll(&mut out);
        out.into_iter()
            .filter_map(|effect| match effect {
                super::Effect::Toast { text, .. } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    driver.note_config_repairs(&[repair("settings.json")]);
    let first = toasts(&mut driver);
    assert!(first[0].contains("settings.json"), "{first:?}");
    // Later loads repeat the startup list and add a new reset.
    driver.note_config_repairs(&[repair("settings.json"), repair("ui.json")]);
    assert!(
        driver.repair_toast_armed,
        "the new reset must schedule a toast"
    );
    let second = toasts(&mut driver);
    assert!(second[0].contains("ui.json"), "{second:?}");
    assert!(!second[0].contains("settings.json"), "{second:?}");
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ensure_catalog_loads_once_the_composer_has_a_model() {
    let root = std::env::temp_dir().join(format!(
        "mycode-driver-catalog-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = mycode_app::HomeLayout::from_root(&root).expect("home");
    let mut driver = super::FrontendDriver::start(home, || {});
    // Nothing selected on a fresh launch: no surface reads the catalog, so
    // the document stays a lazy load.
    driver.ensure_catalog();
    assert!(
        !driver.catalog_requested,
        "no selection must not fetch the catalog"
    );
    // The composer's model chip and context ring resolve through the
    // catalog once the open conversation has a model.
    driver.vm.selected_provider = Some("openai-codex".to_owned());
    driver.vm.selected_model = Some("gpt-5".to_owned());
    driver.vm.active = Some(conversation("a", "branch"));
    driver.ensure_catalog();
    assert!(
        driver.catalog_requested,
        "the composer surfaces need the catalog once a model is selected"
    );
    let mut out = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while driver.vm.catalog.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the GetCatalog reply never populated vm.catalog"
        );
        driver.poll(&mut out);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

// Removed: poll_until and lists only served the deleted git-watch test.

// Removed: the_driver_keeps_the_open_folders_git_status_fresh ran `git init`
// in %TEMP% and intermittently hung on this Windows host. The watch path is
// covered by the changes panel in normal use.

/// A steer goes to the running turn without stopping it and shows as
/// pending; a turn that cannot take it hands it back to the composer.
#[test]
fn a_steer_reaches_the_running_turn_and_a_refusal_returns_it() {
    use mycode_app::{BranchId, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("steer");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    let mut out = Vec::new();
    driver.act(FrontendAction::TurnArmed, &mut out);
    driver.steer_send("also check b.rs", &mut out);
    assert!(driver.vm.sending, "the turn keeps running");
    assert_eq!(driver.vm.pending_steers, ["also check b.rs"]);
    assert!(out.iter().any(|effect| matches!(
        effect,
        super::Effect::ComposerSet { text, .. } if text.is_empty()
    )));
    driver.on_reply(
        ReplyScope::Steer {
            session: session.clone(),
            text: "also check b.rs".to_owned(),
        },
        BridgeReply::Steered(Err("no turn is running for this session".to_owned())),
        &mut out,
    );
    assert!(driver.vm.pending_steers.is_empty());
    assert!(
        out.iter().any(|effect| matches!(
            effect,
            super::Effect::ComposerSet { text, .. } if text == "also check b.rs"
        )),
        "the refused steer is back in the composer"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// With no turn running there is nothing to steer: the keypress explains
/// itself instead of sending the draft.
#[test]
fn a_steer_with_no_turn_running_does_not_send() {
    use mycode_app::{BranchId, SessionId};

    let (mut driver, root) = scratch_driver("steer-idle");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    let mut out = Vec::new();
    driver.steer_send("start here", &mut out);
    assert!(!driver.vm.sending, "the draft did not start a turn");
    assert!(driver.vm.pending_steers.is_empty());
    assert!(
        out.iter().any(|effect| matches!(
            effect,
            super::Effect::Toast {
                kind: super::ToastKind::Warning,
                ..
            }
        )),
        "the refused steer says why"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Steers a chat off screen did not take wait parked for it instead of
/// being dropped with the event.
#[test]
fn steers_returned_to_a_chat_off_screen_are_parked() {
    use mycode_app::{BranchId, BridgeEvent, SessionId};

    let (mut driver, root) = scratch_driver("steer-parked");
    let open = SessionId::generate().expect("session").as_str().to_owned();
    let other = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&open, &branch));
    let mut out = Vec::new();
    driver.on_event(
        BridgeEvent::SteersReturned {
            session_id: other.clone(),
            texts: vec!["late steer".to_owned()],
        },
        &mut out,
    );
    assert!(
        driver.vm.pending_steers.is_empty(),
        "another chat's steer never shows here"
    );
    assert_eq!(
        driver.vm.parked_steers,
        [(other, vec!["late steer".to_owned()])]
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

fn code_attachment() -> mycode_app::Attachments {
    let mut attachments = mycode_app::Attachments::default();
    attachments
        .push_file("main.rs", "fn main() {}")
        .expect("text file");
    attachments
}

/// A busy turn cannot take attachments with a steer, so the submit hands
/// them back instead of dropping them.
#[test]
fn a_busy_turn_returns_attachments_to_the_composer() {
    use mycode_app::{BranchId, SessionId};

    let (mut driver, root) = scratch_driver("attach-busy");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    driver.vm.sending = true;
    let mut out = Vec::new();
    let returned = driver.submit_with("look", code_attachment(), &mut out);
    assert!(returned.is_some_and(|attachments| !attachments.is_empty()));
    assert!(
        driver.vm.pending_steers.is_empty(),
        "the text stayed in the composer with them"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// A welcome send already waiting for its new session owns the next message;
/// a second submit keeps its attachments instead of losing them.
#[test]
fn a_held_welcome_send_returns_the_next_attachments() {
    let (mut driver, root) = scratch_driver("attach-held");
    driver.vm.pending_welcome_send = Some("first".to_owned());
    let mut out = Vec::new();
    let returned = driver.submit_with("second", code_attachment(), &mut out);
    assert!(returned.is_some_and(|attachments| !attachments.is_empty()));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Attachments alone on the welcome desk start a task, using their note as
/// the prompt so the first message and the title keep a text anchor.
#[test]
fn attachments_alone_start_a_task_from_the_welcome_desk() {
    let (mut driver, root) = scratch_driver("attach-welcome");
    driver.vm.project_dir = Some(root.display().to_string());
    let mut out = Vec::new();
    let returned = driver.submit_with("", code_attachment(), &mut out);
    assert!(returned.is_none(), "the send took the attachments");
    assert_eq!(
        driver.vm.pending_welcome_send.as_deref(),
        Some("[file] main.rs")
    );
    assert!(!driver.welcome_attachments.is_empty());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// With no folder open the welcome desk cannot start a task, so the
/// attachments stay with the user.
#[test]
fn a_welcome_submit_without_a_folder_returns_attachments() {
    let (mut driver, root) = scratch_driver("attach-nofolder");
    let mut out = Vec::new();
    let returned = driver.submit_with("", code_attachment(), &mut out);
    assert!(returned.is_some_and(|attachments| !attachments.is_empty()));
    assert!(driver.vm.pending_welcome_send.is_none());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
