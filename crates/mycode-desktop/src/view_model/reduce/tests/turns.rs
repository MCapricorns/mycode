//! Turn lifecycle: commits, failures, summaries, steers, and new sessions.

use super::*;
use crate::view_model::{ComposerSubmit, composer_submit};

#[test]
fn a_finished_turn_promotes_its_live_meter_to_last_turn() {
    let mut state = open(Vec::new(), None);
    reduce(
        &mut state,
        DesktopAction::UsageSnapshot {
            model: "glm-5.3".to_owned(),
            input: 100,
            context: 12_000,
            context_cache: 11_800,
            output: 20,
            cache: Some(11_800),
            requests: 1,
            elapsed_ms: 40,
        },
    );
    assert!(state.live_turn.is_some());
    assert!(state.last_turn.is_none());
    assert_eq!(state.context_used, 12_000);
    assert_eq!(state.context_cache, 11_800);

    reduce(
        &mut state,
        DesktopAction::ChatDone {
            head: "evt-2".to_owned(),
            entry: user("evt-2", "reply"),
        },
    );
    let turn = state.last_turn.as_ref().expect("promoted stats");
    assert_eq!(turn.model, "glm-5.3");
    assert_eq!(turn.input, 100);
    assert_eq!(turn.output, 20);
    assert_eq!(turn.cache, Some(11_800));
    assert!(state.live_turn.is_none());
    // The meter is the latest prompt; the finished turn keeps it.
    assert_eq!(state.context_used, 12_000);
    assert_eq!(state.context_cache, 11_800);
}

#[test]
fn a_new_session_inherits_the_open_chat_model() {
    let mut settings = crate::view_model::SettingsState::from_settings(
        &mycode_config::AppSettings::default(),
        1,
        Vec::new(),
    );
    settings.reasoning = Some("on".to_owned());
    let mut state = WorkspaceState {
        settings: Some(settings),
        selected_provider: Some("zai".to_owned()),
        selected_model: Some("glm-4.7".to_owned()),
        active: Some(mycode_app::ActiveConversation {
            session_id: "old".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        DesktopAction::SessionCreated(mycode_app::SessionSummary {
            session_id: "new".to_owned(),
            root_branch_id: "branch".to_owned(),
            title: String::new(),
            updated_at_ms: None,
            event_count: 0,
            active: false,
            corrupt: false,
        }),
    );
    let inherited = mycode_config::session_model(&state.session_models, "new").expect("pin");
    assert_eq!(inherited.provider, "zai");
    assert_eq!(inherited.model, "glm-4.7");
    assert_eq!(inherited.reasoning.as_deref(), Some("on"));
    let previous = mycode_config::session_model(&state.session_models, "old").expect("old");
    assert_eq!(previous.model, "glm-4.7");
}

#[test]
fn manual_and_auto_compaction_show_the_summary_in_the_chat() {
    use mycode_app::protocol::{ConversationEntry, EntryKind};

    let mut state = WorkspaceState {
        active: Some(mycode_app::ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "branch".to_owned(),
            head: "e1".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    let text = mycode_app::display_summary_text("files: src/main.rs");
    let entry = ConversationEntry {
        event_id: "sum-1".to_owned(),
        kind: EntryKind::UserMessage,
        text: text.into(),
        call_id: None,
        thinking: String::new(),
    };
    // Both triggers emit this action with the written summary.
    reduce(&mut state, DesktopAction::SummaryShown(entry.clone()));
    reduce(&mut state, DesktopAction::SummaryShown(entry));
    let entries = &state.active.expect("open").entries;
    assert_eq!(entries.len(), 1);
    assert!(mycode_app::is_compaction_summary(&entries[0].text));
    assert!(mycode_app::summary_body(&entries[0].text).contains("src/main.rs"));
}

#[test]
fn a_mid_turn_summary_lands_after_the_final_reply() {
    use mycode_app::protocol::{ConversationEntry, EntryKind};

    let tool = ConversationEntry {
        event_id: "call-1".to_owned(),
        kind: EntryKind::ToolCall,
        text: "grep  src".into(),
        call_id: Some("c1".into()),
        thinking: String::new(),
    };
    let result = ConversationEntry {
        event_id: "res-1".to_owned(),
        kind: EntryKind::ToolResult,
        text: "src/main.rs:1:fn main".into(),
        call_id: Some("c1".into()),
        thinking: String::new(),
    };
    let mut state = WorkspaceState {
        sending: true,
        active: Some(mycode_app::ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "branch".to_owned(),
            head: "e1".to_owned(),
            entries: vec![tool, result],
            older_before: None,
            streaming: Some(mycode_app::StreamingReply {
                text: String::new(),
                thinking: "weigh the matches".to_owned(),
                status: String::new(),
            }),
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    let summary = ConversationEntry {
        event_id: "sum-1".to_owned(),
        kind: EntryKind::UserMessage,
        text: mycode_app::display_summary_text("files: src/main.rs").into(),
        call_id: None,
        thinking: String::new(),
    };
    reduce(&mut state, DesktopAction::SummaryShown(summary));
    let held = state.active.as_ref().expect("open");
    assert!(
        held.entries
            .iter()
            .all(|entry| !mycode_app::is_compaction_summary(&entry.text)),
        "the card must not sit above the still-streaming reply"
    );
    assert_eq!(
        state
            .pending_summary
            .as_ref()
            .map(|entry| entry.event_id.as_str()),
        Some("sum-1")
    );
    let reply = ConversationEntry {
        event_id: "reply-1".to_owned(),
        kind: EntryKind::AssistantMessage,
        text: "done".into(),
        call_id: None,
        thinking: "weigh the matches".to_owned(),
    };
    reduce(
        &mut state,
        DesktopAction::ChatDone {
            head: "reply-1".to_owned(),
            entry: reply,
        },
    );
    let entries = &state.active.expect("open").entries;
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].kind, EntryKind::ToolCall);
    assert_eq!(entries[1].kind, EntryKind::ToolResult);
    assert_eq!(entries[2].event_id, "reply-1");
    assert!(mycode_app::is_compaction_summary(&entries[3].text));
    assert!(state.pending_summary.is_none());
    assert!(!state.sending);
}

#[test]
fn welcome_enter_holds_the_draft_until_the_new_session_opens() {
    let mut state = WorkspaceState {
        project_dir: Some("/tmp/app".to_owned()),
        composer_draft: "explain the build".to_owned(),
        ..WorkspaceState::default()
    };
    assert_eq!(
        composer_submit(&state, &state.composer_draft),
        ComposerSubmit::StartTask("explain the build".to_owned())
    );
    reduce(
        &mut state,
        DesktopAction::WelcomeSendHeld("explain the build".to_owned()),
    );
    assert_eq!(
        state.pending_welcome_send.as_deref(),
        Some("explain the build")
    );
    assert_eq!(state.composer_draft, "explain the build");
    reduce(
        &mut state,
        DesktopAction::SessionCreated(mycode_app::SessionSummary {
            session_id: "new".to_owned(),
            root_branch_id: "branch".to_owned(),
            title: String::new(),
            updated_at_ms: None,
            event_count: 0,
            active: false,
            corrupt: false,
        }),
    );
    assert_eq!(
        state.pending_welcome_send.as_deref(),
        Some("explain the build")
    );
    assert_eq!(state.composer_draft, "explain the build");
    assert!(state.active.is_some());
    reduce(
        &mut state,
        DesktopAction::ConversationOpened(ActiveConversation {
            session_id: "new".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            context_used: 0,
            context_cache: 0,
        }),
    );
    assert_eq!(state.composer_draft, "explain the build");
    assert_eq!(
        state.pending_welcome_send.as_deref(),
        Some("explain the build")
    );
    assert_eq!(
        composer_submit(&state, &state.composer_draft),
        ComposerSubmit::Send
    );
    reduce(&mut state, DesktopAction::WelcomeSendConsumed);
    assert!(state.pending_welcome_send.is_none());
}

#[test]
fn welcome_enter_without_a_folder_does_not_start_a_task() {
    let mut state = WorkspaceState {
        composer_draft: "hello".to_owned(),
        ..WorkspaceState::default()
    };
    assert_eq!(composer_submit(&state, "hello"), ComposerSubmit::NeedFolder);
    reduce(
        &mut state,
        DesktopAction::WelcomeSendHeld("hello".to_owned()),
    );
    assert!(state.pending_welcome_send.is_none());
    assert_eq!(state.composer_draft, "hello");
    let open = WorkspaceState {
        project_dir: Some("/tmp/app".to_owned()),
        composer_draft: "  ".to_owned(),
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    assert_eq!(composer_submit(&open, "still here"), ComposerSubmit::Send);
    assert_eq!(
        composer_submit(
            &WorkspaceState {
                project_dir: Some("/tmp/app".to_owned()),
                ..WorkspaceState::default()
            },
            "   ",
        ),
        ComposerSubmit::Ignore
    );
}

#[test]
fn chat_failed_head_updates_conversation_without_clearing_context() {
    use crate::view_model::StreamingReply;

    let mut state = WorkspaceState {
        context_used: 25_000,
        context_cache: 24_300,
        sending: true,
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-old".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: Some(StreamingReply {
                text: "PARTIAL_A".to_owned(),
                thinking: String::new(),
                status: "working".to_owned(),
            }),
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        DesktopAction::ChatFailed {
            message: mycode_app::CHAT_CANCELLED.to_owned(),
            head: "evt-partial".to_owned(),
        },
    );
    let conversation = state.active.as_ref().expect("conversation");
    assert_eq!(conversation.head, "evt-partial");
    assert!(conversation.streaming.is_none());
    assert!(!state.sending);
    assert!(state.error.is_none());
    assert_eq!(state.context_used, 25_000);
    assert_eq!(state.context_cache, 24_300);

    reduce(
        &mut state,
        DesktopAction::ChatFailed {
            message: "provider down".to_owned(),
            head: String::new(),
        },
    );
    assert_eq!(
        state.active.as_ref().expect("conversation").head,
        "evt-partial"
    );
    assert_eq!(state.context_used, 25_000);
    assert_eq!(state.error.as_deref(), Some("provider down"));
}

#[test]
fn a_session_created_mid_turn_does_not_inherit_the_running_turn() {
    let mut state = open(Vec::new(), None);
    reduce(&mut state, DesktopAction::TurnArmed);
    reduce(
        &mut state,
        DesktopAction::MessageQueued("follow up".to_owned()),
    );
    reduce(&mut state, DesktopAction::SteerSent("steer".to_owned()));
    assert!(state.sending);
    reduce(
        &mut state,
        DesktopAction::SessionCreated(mycode_app::SessionSummary {
            session_id: "new".to_owned(),
            root_branch_id: "branch".to_owned(),
            title: String::new(),
            updated_at_ms: None,
            event_count: 0,
            active: false,
            corrupt: false,
        }),
    );
    // The old turn's events no longer reach this view, so nothing would
    // ever clear these for the fresh chat.
    assert!(!state.sending);
    assert!(state.queued.is_empty());
    assert!(state.pending_steers.is_empty());
    assert!(!state.history_loading);
    assert_eq!(
        state
            .active
            .as_ref()
            .map(|active| active.session_id.as_str()),
        Some("new")
    );
}

#[test]
fn streaming_text_stops_at_the_character_cap() {
    let mut state = open(Vec::new(), None);
    // Three bytes per char: the byte fast path runs out well before the
    // character cap, so both paths are exercised.
    let chunk = "\u{4e2d}".repeat(1024);
    for _ in 0..(mycode_app::MAX_STREAMING_CHARS / 1024 + 4) {
        reduce(&mut state, DesktopAction::ChatDelta(chunk.clone()));
    }
    let text = &state
        .active
        .as_ref()
        .and_then(|active| active.streaming.as_ref())
        .expect("streaming")
        .text;
    assert_eq!(text.chars().count(), mycode_app::MAX_STREAMING_CHARS);
}

#[test]
fn steer_committed_appends_the_entry_and_moves_the_head() {
    let mut state = WorkspaceState {
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-old".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    let entry = ConversationEntry {
        event_id: "evt-steer".to_owned(),
        kind: EntryKind::UserMessage,
        text: "keep going".into(),
        call_id: None,
        thinking: String::new(),
    };
    reduce(
        &mut state,
        DesktopAction::SteerCommitted {
            session_id: "other".to_owned(),
            head: "evt-ignored".to_owned(),
            entry: entry.clone(),
        },
    );
    assert_eq!(state.active.as_ref().expect("conversation").head, "evt-old");
    assert!(
        state
            .active
            .as_ref()
            .expect("conversation")
            .entries
            .is_empty()
    );
    reduce(
        &mut state,
        DesktopAction::SteerCommitted {
            session_id: "ses".to_owned(),
            head: "evt-steer".to_owned(),
            entry,
        },
    );
    let conversation = state.active.as_ref().expect("conversation");
    assert_eq!(conversation.head, "evt-steer");
    assert_eq!(conversation.entries.len(), 1);
    assert_eq!(conversation.entries[0].text.as_ref(), "keep going");
}

#[test]
fn ending_a_turn_clears_pending_ask_and_subagent_detail_window() {
    let prompt = mycode_app::AskPrompt {
        question: "Pick one?".to_owned(),
        choices: Vec::new(),
        optional: false,
        multiple: false,
    };
    let mut state = open(Vec::new(), None);
    state.sending = true;
    reduce(
        &mut state,
        DesktopAction::AskRequested(vec![prompt.clone()]),
    );
    reduce(
        &mut state,
        DesktopAction::AskChoicePicked {
            index: 0,
            answer: "a".to_owned(),
        },
    );
    reduce(
        &mut state,
        DesktopAction::SubagentWindowChanged(Some("call-1".to_owned())),
    );
    reduce(
        &mut state,
        DesktopAction::ChatFailed {
            message: mycode_app::CHAT_CANCELLED.to_owned(),
            head: String::new(),
        },
    );
    assert!(state.pending_ask.is_none());
    assert!(state.ask_answers.is_empty());
    assert!(state.subagent_window.is_none());

    state.sending = true;
    reduce(&mut state, DesktopAction::AskRequested(vec![prompt]));
    reduce(
        &mut state,
        DesktopAction::SubagentWindowChanged(Some("call-2".to_owned())),
    );
    reduce(
        &mut state,
        DesktopAction::ChatDone {
            head: "evt-done".to_owned(),
            entry: user("evt-done", "done"),
        },
    );
    assert!(state.pending_ask.is_none());
    assert!(state.ask_answers.is_empty());
    assert!(state.subagent_window.is_none());
}

#[test]
fn workspace_rename_strips_control_characters_for_durable_ui_state() {
    let ws = mycode_config::WorkspaceDef::generate("Default", Vec::new());
    let mut state = WorkspaceState {
        active_workspace: Some(ws.id.clone()),
        workspaces: vec![ws],
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        DesktopAction::WorkspaceRenamed("  Line 1\nLine 2\t ".to_owned()),
    );
    assert_eq!(state.workspaces[0].name, "Line 1 Line 2");
    let ui_state = mycode_config::UiState {
        workspaces: state.workspaces.clone(),
        active_workspace: state.active_workspace.clone(),
        ..mycode_config::UiState::default()
    };
    assert!(ui_state.validate().is_ok());
}
