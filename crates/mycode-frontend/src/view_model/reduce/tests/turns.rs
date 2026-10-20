//! Turn lifecycle: commits, failures, summaries, steers, and new sessions.

use super::*;
use crate::view_model::{ComposerSubmit, composer_submit};

#[test]
fn a_finished_turn_promotes_its_live_meter_to_last_turn() {
    let mut state = open(Vec::new(), None);
    reduce(&mut state, FrontendAction::TurnArmed);
    reduce(
        &mut state,
        FrontendAction::UsageSnapshot {
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
        FrontendAction::ChatDone {
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
            running: false,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        FrontendAction::SessionCreated(mycode_app::SessionSummary {
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
            running: false,
            compacting: false,
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
    reduce(&mut state, FrontendAction::SummaryShown(entry.clone()));
    reduce(&mut state, FrontendAction::SummaryShown(entry));
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
            running: false,
            compacting: false,
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
    reduce(&mut state, FrontendAction::SummaryShown(summary));
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
        FrontendAction::ChatDone {
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
        FrontendAction::WelcomeSendHeld("explain the build".to_owned()),
    );
    assert_eq!(
        state.pending_welcome_send.as_deref(),
        Some("explain the build")
    );
    assert_eq!(state.composer_draft, "explain the build");
    reduce(
        &mut state,
        FrontendAction::SessionCreated(mycode_app::SessionSummary {
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
        FrontendAction::ConversationOpened(ActiveConversation {
            session_id: "new".to_owned(),
            branch_id: "branch".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            running: false,
            compacting: false,
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
    reduce(&mut state, FrontendAction::WelcomeSendConsumed);
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
        FrontendAction::WelcomeSendHeld("hello".to_owned()),
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
            running: false,
            compacting: false,
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
            running: false,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        FrontendAction::ChatFailed {
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
        FrontendAction::ChatFailed {
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
    reduce(&mut state, FrontendAction::TurnArmed);
    reduce(
        &mut state,
        FrontendAction::MessageQueued("follow up".to_owned()),
    );
    state.pending_steers.push("steer".to_owned());
    assert!(state.sending);
    reduce(
        &mut state,
        FrontendAction::SessionCreated(mycode_app::SessionSummary {
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
    assert_eq!(
        state.parked_queues,
        [("ses".to_owned(), vec!["follow up".to_owned()])],
        "the previous chat keeps its queue"
    );
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

fn reopened(session: &str, running: bool) -> ActiveConversation {
    ActiveConversation {
        session_id: session.to_owned(),
        branch_id: "br".to_owned(),
        head: "empty".to_owned(),
        entries: Vec::new(),
        older_before: None,
        streaming: None,
        running,
        compacting: false,
        context_used: 0,
        context_cache: 0,
    }
}

/// Follow-ups queued behind a turn used to vanish when another chat took
/// the screen. They stay with their chat and come back when it opens.
#[test]
fn queued_follow_ups_come_back_with_their_chat() {
    let mut state = open(Vec::new(), None);
    reduce(&mut state, FrontendAction::TurnArmed);
    reduce(
        &mut state,
        FrontendAction::MessageQueued("follow up".to_owned()),
    );
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(reopened("other", false)),
    );
    assert!(
        state.queued.is_empty(),
        "another chat never sends this queue"
    );
    // Back while the turn still runs: the queue starts when it ends.
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(reopened("ses", true)),
    );
    assert_eq!(state.queued, ["follow up"]);
    assert!(!state.queue_paused);
    // Away and back after the turn ended: the queue waits for a send.
    reduce(&mut state, FrontendAction::ConversationParked);
    assert!(state.queued.is_empty());
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(reopened("ses", false)),
    );
    assert_eq!(state.queued, ["follow up"]);
    assert!(
        state.queue_paused,
        "a queue whose turn ended away waits for an explicit send"
    );
    // A deleted chat's parked queue goes with it.
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(reopened("other", false)),
    );
    assert_eq!(state.parked_queues.len(), 1);
    reduce(
        &mut state,
        FrontendAction::SessionBindingsForgotten("ses".to_owned()),
    );
    assert!(state.parked_queues.is_empty());
}

/// A reopen of the chat on screen that lands just after a send armed the
/// next turn keeps the waiting bubble the wire copy never carries.
#[test]
fn a_reopen_after_a_send_keeps_the_waiting_bubble() {
    let mut state = open(Vec::new(), None);
    reduce(&mut state, FrontendAction::TurnArmed);
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(reopened("ses", false)),
    );
    assert!(state.sending);
    assert!(
        state
            .active
            .as_ref()
            .and_then(|active| active.streaming.as_ref())
            .is_some()
    );
}

#[test]
fn streaming_text_stops_at_the_character_cap() {
    let mut state = open(Vec::new(), None);
    state.sending = true;
    // Three bytes per char: the byte fast path runs out well before the
    // character cap, so both paths are exercised.
    let chunk = "\u{4e2d}".repeat(1024);
    for _ in 0..(mycode_app::MAX_STREAMING_CHARS / 1024 + 4) {
        reduce(&mut state, FrontendAction::ChatDelta(chunk.clone()));
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
            running: false,
            compacting: false,
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
        FrontendAction::SteerCommitted {
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
        FrontendAction::SteerCommitted {
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
        FrontendAction::AskRequested(vec![prompt.clone()]),
    );
    reduce(
        &mut state,
        FrontendAction::AskChoicePicked {
            index: 0,
            answer: "a".to_owned(),
        },
    );
    reduce(
        &mut state,
        FrontendAction::SubagentWindowChanged(Some("call-1".to_owned())),
    );
    reduce(
        &mut state,
        FrontendAction::ChatFailed {
            message: mycode_app::CHAT_CANCELLED.to_owned(),
            head: String::new(),
        },
    );
    assert!(state.pending_ask.is_none());
    assert!(state.ask_answers.is_empty());
    assert!(state.subagent_window.is_none());

    state.sending = true;
    reduce(&mut state, FrontendAction::AskRequested(vec![prompt]));
    reduce(
        &mut state,
        FrontendAction::SubagentWindowChanged(Some("call-2".to_owned())),
    );
    reduce(
        &mut state,
        FrontendAction::ChatDone {
            head: "evt-done".to_owned(),
            entry: user("evt-done", "done"),
        },
    );
    assert!(state.pending_ask.is_none());
    assert!(state.ask_answers.is_empty());
    assert!(state.subagent_window.is_none());
}

#[test]
fn stragglers_after_the_turn_do_not_resurrect_running_state() {
    let mut state = open(Vec::new(), None);
    state.sending = true;
    reduce(
        &mut state,
        FrontendAction::ToolStarted {
            call_id: "c1".to_owned(),
            provider_call_id: "call_1".to_owned(),
            name: "agent".to_owned(),
            target: "scout map".to_owned(),
        },
    );
    reduce(
        &mut state,
        FrontendAction::ChatDone {
            head: "evt-2".to_owned(),
            entry: user("evt-2", "reply"),
        },
    );
    assert!(!state.sending);
    assert!(state.live_jobs.is_empty());

    // A progress line queued behind the turn's Done must not rebuild the
    // live job, the status line, or a streaming bubble.
    reduce(
        &mut state,
        FrontendAction::ToolProgress {
            call_id: "c1".to_owned(),
            provider_call_id: String::new(),
            name: String::new(),
            message: "agent|scout|step|still running".to_owned(),
        },
    );
    reduce(&mut state, FrontendAction::ChatDelta("tail".to_owned()));
    reduce(
        &mut state,
        FrontendAction::UsageSnapshot {
            model: "m".to_owned(),
            input: 1,
            context: 1,
            context_cache: 0,
            output: 1,
            cache: None,
            requests: 1,
            elapsed_ms: 1,
        },
    );
    assert!(state.live_jobs.is_empty());
    assert!(state.live_turn.is_none());
    assert!(
        state
            .active
            .as_ref()
            .and_then(|active| active.streaming.as_ref())
            .is_none()
    );
}

#[test]
fn reopening_a_session_with_a_live_turn_restores_sending() {
    let mut state = open(Vec::new(), None);
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(ActiveConversation {
            session_id: "other".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-head".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            running: true,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
    );
    assert!(state.sending);
    // Live events for the in-flight turn flow into the view again.
    reduce(
        &mut state,
        FrontendAction::ToolProgress {
            call_id: "c1".to_owned(),
            provider_call_id: String::new(),
            name: String::new(),
            message: "agent|scout|step|mapping".to_owned(),
        },
    );
    assert_eq!(state.live_jobs.len(), 1);
}

#[test]
fn compacting_locks_the_composer_and_finish_advances_the_head() {
    let mut state = WorkspaceState {
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "e1".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            running: false,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(&mut state, FrontendAction::CompactArmed);
    assert!(state.compacting);
    // A draft typed while the card is up is still in the composer, and a
    // queued message waits — nothing is dropped mid-compact.
    state.composer_draft = "follow up".to_owned();
    state.queued.push("queued".to_owned());
    reduce(
        &mut state,
        FrontendAction::CompactFinished {
            ok: true,
            head: "evt-sum".to_owned(),
        },
    );
    assert!(!state.compacting);
    assert_eq!(state.active.as_ref().expect("open").head, "evt-sum");
    assert_eq!(state.composer_draft, "follow up");
    assert_eq!(state.queued.len(), 1);
}

#[test]
fn a_failed_compact_clears_the_lock_without_moving_the_head() {
    let mut state = WorkspaceState {
        compacting: true,
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "e1".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            running: false,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        FrontendAction::CompactFinished {
            ok: false,
            head: "e1".to_owned(),
        },
    );
    assert!(!state.compacting);
    assert_eq!(state.active.as_ref().expect("open").head, "e1");
}

/// Reopening the session mid-turn replaces live rows with ledger-replayed
/// ones. Because live and replayed rows share the ledger call identity, the
/// live job keeps matching the row (cancel affordance) and its result still
/// closes the job and pairs with the row.
#[test]
fn a_mid_turn_reopen_keeps_agent_calls_pairable() {
    let mut state = open(Vec::new(), None);
    state.sending = true;
    reduce(
        &mut state,
        FrontendAction::ToolStarted {
            call_id: "call1-a".to_owned(),
            provider_call_id: "call_00_1".to_owned(),
            name: "agent".to_owned(),
            target: "scout verify".to_owned(),
        },
    );
    assert_eq!(state.live_jobs.len(), 1);
    assert_eq!(state.live_jobs[0].provider_call_id, "call_00_1");
    let waiting = state
        .active
        .as_ref()
        .and_then(|active| active.streaming.as_ref())
        .map(|streaming| streaming.status.clone())
        .unwrap_or_default();
    // The reopen projects the same call row with the same identity.
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-head".to_owned(),
            entries: vec![ConversationEntry {
                event_id: "evt-call".to_owned(),
                kind: EntryKind::ToolCall,
                text: "agent scout verify".into(),
                call_id: Some("call1-a".to_owned()),
                thinking: String::new(),
            }],
            older_before: None,
            streaming: None,
            running: true,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
    );
    assert!(
        crate::view_model::subagent_cancellable(
            Some("call1-a"),
            false,
            state.sending,
            &state.live_jobs
        ),
        "the replayed row must still own the running job"
    );
    reduce(
        &mut state,
        FrontendAction::ToolResultAppended(ConversationEntry {
            event_id: "evt-res".to_owned(),
            kind: EntryKind::ToolResult,
            text: "done".into(),
            call_id: Some("call1-a".to_owned()),
            thinking: String::new(),
        }),
    );
    assert!(state.live_jobs.is_empty());
    let status = state
        .active
        .as_ref()
        .and_then(|active| active.streaming.as_ref())
        .map(|streaming| streaming.status.clone())
        .unwrap_or_default();
    assert_ne!(status, waiting);
}

/// Switching to a session whose turn is still running drops the old view's
/// jobs; the running subagent reappears through its next progress line. That
/// line carries the provider id, so the row's stop button still reaches the
/// core's cancel token.
#[test]
fn a_job_seen_only_through_progress_keeps_its_cancel_id() {
    let mut state = open(Vec::new(), None);
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(ActiveConversation {
            session_id: "ses-running".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-head".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: None,
            running: true,
            compacting: false,
            context_used: 0,
            context_cache: 0,
        }),
    );
    assert!(state.sending);
    assert!(state.live_jobs.is_empty());
    reduce(
        &mut state,
        FrontendAction::ToolProgress {
            call_id: "call1-a".to_owned(),
            provider_call_id: "call_00_1".to_owned(),
            name: String::new(),
            message: "agent|scout|step|mapping".to_owned(),
        },
    );
    assert_eq!(state.live_jobs.len(), 1);
    assert_eq!(state.live_jobs[0].call_id, "call1-a");
    assert_eq!(state.live_jobs[0].provider_call_id, "call_00_1");
}
