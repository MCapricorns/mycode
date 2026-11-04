use super::ReplyScope;
use crate::app::ActiveConversation;
use crate::frontend::view_model::{FrontendAction, WorkspaceState, reduce};

mod catalog;
mod creation;
mod recall;
mod runtime_guards;
mod scroll;
mod sends;

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
    assert!(crate::frontend::view_model::open_session_is_empty(&state));
}

#[test]
fn a_chat_opened_mid_turn_reloads_from_the_ledger_when_the_turn_ends() {
    use crate::app::{BranchId, BridgeEvent, BridgeReply, ConversationEntry, EntryKind, SessionId};

    let root = std::env::temp_dir().join(format!(
        "mycode-driver-resync-{}-{}",
        std::process::id(),
        crate::core::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = crate::config::HomeLayout::from_root(&root).expect("home");
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
                agent_states: None,
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
        crate::core::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = crate::config::HomeLayout::from_root(&root).expect("home");
    (super::FrontendDriver::start(home, || {}), root)
}

/// Removing project navigation must not dispose a running conversation or
/// its draft. Conversation deletion has its own action and confirmation.
#[test]
fn removing_a_project_keeps_its_conversation_and_working_directory() {
    let (mut driver, root) = scratch_driver("unlink-project");
    let project = root.to_string_lossy().into_owned();
    let session = crate::app::SessionId::generate()
        .unwrap()
        .as_str()
        .to_owned();
    let mut active = conversation(&session, "branch");
    active.running = true;
    driver.vm.active = Some(active.clone());
    driver.vm.sending = true;
    driver.vm.project_dir = Some(project.clone());
    driver.vm.workspace_roots = vec![project.clone()];
    driver.vm.session_projects = vec![(session.clone(), project.clone())];
    driver.vm.composer_draft = "Keep this follow-up".to_owned();
    driver.vm.sessions = vec![crate::app::SessionSummary {
        session_id: session.clone(),
        root_branch_id: "branch".to_owned(),
        title: "Image capability investigation".to_owned(),
        updated_at_ms: None,
        event_count: 2,
        active: true,
        corrupt: false,
    }];
    let mut effects = Vec::new();
    driver.remove_workspace_root(&project, &mut effects);
    assert!(driver.vm.workspace_roots.is_empty());
    assert_eq!(driver.vm.active.as_ref(), Some(&active));
    assert_eq!(driver.vm.project_dir.as_deref(), Some(project.as_str()));
    assert_eq!(
        driver.vm.session_projects,
        [(session.clone(), project.clone())]
    );
    assert_eq!(driver.vm.sessions[0].session_id, session);
    assert_eq!(driver.vm.composer_draft, "Keep this follow-up");
    assert!(driver.vm.sending);
    assert!(effects.is_empty(), "unlinking must not reset the composer");
    // The turn completion reloads its snapshot. That passive reply must not
    // recreate the project shortcut the user just removed.
    active.running = false;
    driver.pending_open = Some(session.clone());
    driver.on_reply(
        ReplyScope::Open(session),
        crate::app::BridgeReply::Conversation(Ok(active)),
        &mut effects,
    );
    assert!(driver.vm.workspace_roots.is_empty());
    assert_eq!(driver.vm.project_dir.as_deref(), Some(project.as_str()));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

/// A reopen during `/compact` restores the compaction, not a running turn.
/// `CompactFinished` is the only event that ends it; a reopen that set
/// `sending` left the composer stuck on Stop after the compaction ended.
#[test]
fn a_chat_opened_mid_compact_unlocks_when_the_compaction_finishes() {
    use crate::app::{BranchId, BridgeEvent, BridgeReply, SessionId};

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

/// The composer is cleared when a send goes out, so a send the core could
/// not write hands its text back instead of losing it.
#[test]
fn a_failed_send_puts_its_text_back_in_the_composer() {
    use crate::app::{BranchId, BridgeReply, SessionId};

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
    use crate::app::{BranchId, BridgeReply, SessionId};

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
    use crate::app::{BranchId, BridgeEvent, SessionId};

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
        let held = driver
            .held_interrupts
            .get(&session)
            .expect("the stop is held");
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
        driver.held_interrupts.contains_key(&session),
        "the message waits for the turn to be gone"
    );
    driver.on_event(
        BridgeEvent::ChatFailed {
            session_id: session.clone(),
            message: crate::app::CHAT_CANCELLED.to_owned(),
            head: String::new(),
        },
        &mut out,
    );
    assert!(
        driver.held_interrupts.is_empty(),
        "the stop landed; the held message went out"
    );
    assert!(driver.vm.sending, "the held message started the next turn");
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ensure_catalog_loads_once_the_composer_has_a_model() {
    let root = std::env::temp_dir().join(format!(
        "mycode-driver-catalog-{}-{}",
        std::process::id(),
        crate::core::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = crate::config::HomeLayout::from_root(&root).expect("home");
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

/// A steer goes to the running turn without stopping it and shows as
/// pending; a turn that cannot take it hands it back to the composer.
#[test]
fn a_steer_reaches_the_running_turn_and_a_refusal_returns_it() {
    use crate::app::{BranchId, BridgeReply, SessionId};

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
    use crate::app::{BranchId, SessionId};

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

/// A retracted steer leaves the strip and returns to the composer only
/// when the core confirms it was still pending; one the turn already read
/// stays sent.
#[test]
fn a_retracted_steer_returns_only_when_the_core_confirms_it() {
    use crate::app::{BranchId, BridgeReply, SessionId};

    let (mut driver, root) = scratch_driver("steer-retract");
    let session = SessionId::generate().expect("session").as_str().to_owned();
    let branch = BranchId::generate().expect("branch").as_str().to_owned();
    driver.vm.active = Some(conversation(&session, &branch));
    let mut out = Vec::new();
    driver.act(FrontendAction::TurnArmed, &mut out);
    driver.steer_send("check a.rs", &mut out);
    driver.steer_send("check b.rs", &mut out);
    assert_eq!(driver.vm.pending_steers, ["check a.rs", "check b.rs"]);
    // Already read by the turn: the retraction changes nothing.
    driver.on_reply(
        ReplyScope::RetractSteer {
            session: session.clone(),
            text: "check a.rs".to_owned(),
        },
        BridgeReply::SteerRetracted(Ok(false)),
        &mut out,
    );
    assert_eq!(driver.vm.pending_steers, ["check a.rs", "check b.rs"]);
    // Still pending: it leaves the strip and its text is back in the
    // composer as a `ComposerSet` effect.
    out.clear();
    driver.on_reply(
        ReplyScope::RetractSteer {
            session: session.clone(),
            text: "check a.rs".to_owned(),
        },
        BridgeReply::SteerRetracted(Ok(true)),
        &mut out,
    );
    assert_eq!(driver.vm.pending_steers, ["check b.rs"]);
    assert!(
        out.iter().any(|effect| matches!(
            effect,
            super::Effect::ComposerSet { text, .. } if text == "check a.rs"
        )),
        "the retracted steer is back in the composer"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

fn code_attachment() -> crate::core::Attachments {
    let mut attachments = crate::core::Attachments::default();
    attachments
        .push_file("main.rs", "fn main() {}")
        .expect("text file");
    attachments
}

/// A busy turn cannot take attachments with a steer, so the submit hands
/// them back instead of dropping them.
#[test]
fn a_busy_turn_returns_attachments_to_the_composer() {
    use crate::app::{BranchId, SessionId};

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

/// With no folder open the welcome desk still starts a default chatbox
/// session and takes the attachments along.
#[test]
fn a_welcome_submit_without_a_folder_starts_a_default_session() {
    let (mut driver, root) = scratch_driver("attach-nofolder");
    let mut out = Vec::new();
    let returned = driver.submit_with("", code_attachment(), &mut out);
    assert!(returned.is_none());
    assert_eq!(
        driver.vm.pending_welcome_send.as_deref(),
        Some("[file] main.rs")
    );
    assert!(!driver.welcome_attachments.is_empty());
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
