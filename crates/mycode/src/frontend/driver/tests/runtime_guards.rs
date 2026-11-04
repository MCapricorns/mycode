//! Sending and cancelling must respect the state shown to the user.
use crate::app::{BranchId, BridgeCommand, BridgeReply, SessionId};
use crate::config::AppSettings;
use crate::tools::CodeModeProgress;

use super::{conversation, scratch_driver};
use crate::frontend::driver::ReplyScope;
use crate::frontend::driver::turn::selected_route;
use crate::frontend::view_model::{FrontendAction, SettingsState, WorkspaceState};

#[test]
fn terminal_events_unlock_recall_for_a_conversation_opened_while_busy() {
    let (mut driver, root) = scratch_driver("terminal-recall");
    let mut active = conversation("session", "branch");
    active.entries.push(crate::app::ConversationEntry {
        event_id: "evt1-00000000000000000000000000000002".into(),
        kind: crate::app::EntryKind::UserMessage,
        agent_states: None,
        text: "prompt".into(),
        call_id: None,
        thinking: String::new(),
    });
    let mut out = Vec::new();
    for failed in [false, true] {
        active.running = true;
        driver.vm.active = Some(active.clone());
        driver.vm.sending = true;
        assert!(!driver.can_rewind_message(0));
        let terminal = if failed {
            FrontendAction::ChatFailed {
                message: "provider error".into(),
                head: String::new(),
            }
        } else {
            FrontendAction::ChatDone {
                head: "head".into(),
                entry: crate::app::ConversationEntry {
                    event_id: "answer".into(),
                    kind: crate::app::EntryKind::AssistantMessage,
                    agent_states: None,
                    text: "answer".into(),
                    call_id: None,
                    thinking: String::new(),
                },
            }
        };
        driver.act(terminal, &mut out);
        assert!(driver.can_rewind_message(0));
    }
    active.running = false;
    active.compacting = true;
    driver.vm.active = Some(active);
    driver.vm.compacting = true;
    assert!(!driver.can_rewind_message(0));
    driver.act(
        FrontendAction::CompactFinished {
            ok: false,
            head: String::new(),
        },
        &mut out,
    );
    assert!(driver.can_rewind_message(0));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

fn model_state() -> WorkspaceState {
    let mut settings = SettingsState::from_settings(&AppSettings::default(), 1);
    settings.providers = vec![crate::config::ProviderSettings::magpie(
        "http://127.0.0.1:3425/v1",
        vec!["a/other".into(), "z/default".into()],
        Vec::new(),
    )];
    WorkspaceState {
        settings: Some(settings),
        selected_provider: Some("magpie".into()),
        selected_model: Some("p/pinned".into()),
        ..Default::default()
    }
}

#[test]
fn an_unavailable_selection_never_silently_uses_the_first_model() {
    let mut state = model_state();
    assert!(selected_route(&state).is_err());
    state.selected_model = Some("z/default".into());
    assert_eq!(
        selected_route(&state).unwrap(),
        ("magpie".into(), "z/default".into())
    );
    state.selected_provider = Some("missing-provider".into());
    assert!(selected_route(&state).is_err());
    state.selected_provider = None;
    assert!(selected_route(&state).is_err());
}

#[test]
fn a_published_alias_remains_runnable_after_catalog_refresh() {
    let mut state = model_state();
    let provider = crate::providers::catalog::magpie::provider_from_models(
        br#"{"data":[
            {"id":"p/pinned","owned_by":"p"},
            {"id":"pinned","owned_by":"p"},
            {"id":"z/default","owned_by":"z"}
        ]}"#,
        "http://127.0.0.1:3425/v1",
    )
    .expect("catalog");
    let catalog = crate::providers::catalog::CatalogDocument {
        providers: vec![provider],
    };
    let settings = state.settings.as_mut().expect("settings");
    settings.magpie = crate::config::MagpieSettings {
        enabled: true,
        base_url: "http://127.0.0.1:3425/v1".into(),
        api_key: "fixture-key".into(),
        model: "z/default".into(),
        hidden_models: Vec::new(),
    };
    settings.providers =
        vec![crate::app::magpie_provider_row(&settings.magpie, &catalog).expect("provider")];
    state.catalog = Some(std::sync::Arc::new(catalog));
    state.selected_model = Some("pinned".into());
    assert_eq!(
        selected_route(&state).unwrap(),
        ("magpie".into(), "pinned".into())
    );
    assert_eq!(
        crate::frontend::picker::provider_model_choices(&state, "magpie").len(),
        2
    );
}

#[test]
fn an_unavailable_session_pin_blocks_chat_and_compaction() {
    let (mut driver, root) = scratch_driver("unavailable-pin");
    let session = SessionId::generate().unwrap().to_string();
    let branch = BranchId::generate().unwrap().to_string();
    driver.vm = model_state();
    driver.vm.active = Some(conversation(&session, &branch));
    driver.vm.sending = true;
    crate::config::upsert_session_model(
        &mut driver.vm.session_models,
        &session,
        "magpie",
        "p/pinned",
        None,
    );
    let mut out = Vec::new();
    driver
        .prepare_chat_turn()
        .expect_err("an unavailable route refuses the turn");
    driver.vm.sending = false;
    assert_eq!(driver.vm.selected_model.as_deref(), Some("p/pinned"));
    driver.compact(&mut out);
    assert!(
        !driver.vm.compacting,
        "compaction must respect the same selection"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_early_cancel_refusal_keeps_the_inner_child_available_for_retry() {
    let (mut driver, root) = scratch_driver("early-child-cancel");
    driver.vm.active = Some(conversation("session", "branch"));
    driver.vm.sending = true;
    let mut out = Vec::new();
    driver.act(
        FrontendAction::ToolProgress {
            call_id: "ledger-outer".into(),
            provider_call_id: "outer".into(),
            name: "run_code".into(),
            message: CodeModeProgress::Started {
                call_id: "outer.0".into(),
                name: "agent".into(),
                seq: 0,
                target: "scout inspect source".into(),
            }
            .encode(),
        },
        &mut out,
    );
    driver.cancel_subagent("outer.0", &mut out);
    assert_eq!(
        driver.vm.live_jobs.len(),
        1,
        "a request is not an acknowledgement"
    );
    assert!(!driver.vm.finished_jobs.contains("outer.0"));
    let command = BridgeCommand::CancelSubagent {
        session_id: "session".into(),
        call_id: "outer.0".into(),
    };
    driver.on_reply(
        ReplyScope::for_command(&command),
        BridgeReply::SubagentCancelled(Err("that subagent is not running".into())),
        &mut out,
    );
    let progress = || FrontendAction::ToolProgress {
        call_id: "ledger-wait".into(),
        provider_call_id: "wait".into(),
        name: "wait_code".into(),
        message: CodeModeProgress::Progress {
            call_id: "outer.0".into(),
            name: "agent".into(),
            message: "agent|scout|tool|read src/lib.rs".into(),
        }
        .encode(),
    };
    driver.act(progress(), &mut out);
    assert_eq!(driver.vm.live_jobs.len(), 1);
    assert!(driver.vm.live_jobs[0].step.ends_with("read src/lib.rs"));
    driver.cancel_subagent("outer.0", &mut out);
    driver.on_reply(
        ReplyScope::for_command(&command),
        BridgeReply::SubagentCancelled(Ok(())),
        &mut out,
    );
    assert!(driver.vm.live_jobs.is_empty());
    assert!(driver.vm.finished_jobs.contains("outer.0"));
    driver.act(progress(), &mut out);
    assert!(
        driver.vm.live_jobs.is_empty(),
        "confirmed cancellation rejects queued progress"
    );
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_confirmed_cancel_resolves_the_provider_id_back_to_the_ledger_row() {
    let (mut driver, root) = scratch_driver("confirmed-child-cancel");
    driver.vm.active = Some(conversation("session", "branch"));
    driver.vm.sending = true;
    let mut out = Vec::new();
    driver.act(
        FrontendAction::ToolStarted {
            call_id: "ledger-child".into(),
            provider_call_id: "wire-child".into(),
            name: "agent".into(),
            target: "artisan implement change".into(),
        },
        &mut out,
    );
    let command = BridgeCommand::CancelSubagent {
        session_id: "session".into(),
        call_id: "wire-child".into(),
    };
    let scope = ReplyScope::for_command(&command);
    assert!(scope.applies(&driver.vm, None));
    driver.on_reply(scope, BridgeReply::SubagentCancelled(Ok(())), &mut out);
    assert!(driver.vm.live_jobs.is_empty());
    assert!(driver.vm.finished_jobs.contains("ledger-child"));
    driver.vm.active = Some(conversation("another-session", "branch"));
    assert!(!ReplyScope::for_command(&command).applies(&driver.vm, None));
    driver.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
