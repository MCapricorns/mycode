use super::reduce;
use crate::view_model::{
    ActiveConversation, ConversationEntry, EntryKind, FrontendAction, TurnStats, UpdateState,
    WorkspaceState,
};

#[test]
fn hiding_and_showing_models_edits_the_provider_hidden_list() {
    let mut provider = mycode_config::ProviderSettings {
        id: "openai".to_owned(),
        kind: "openai-responses".to_owned(),
        base_url: "https://api.openai.com/v1".to_owned(),
        models: vec!["gpt".to_owned()],
        enabled: true,
        context_limit: None,
        max_output: None,
        hidden_models: Vec::new(),
    };
    let ids = |models: &[&str]| models.iter().map(|id| (*id).to_owned()).collect();
    assert!(super::settings::set_models_shown(
        &mut provider,
        ids(&["a", "b", "a"]),
        false
    ));
    assert_eq!(provider.hidden_models, ["a", "b"]);
    assert!(!provider.shows_model("a"));
    assert!(provider.shows_model("gpt"));
    assert!(!super::settings::set_models_shown(
        &mut provider,
        ids(&["b"]),
        false
    ));
    assert!(super::settings::set_models_shown(
        &mut provider,
        ids(&["a", "z"]),
        true
    ));
    assert_eq!(provider.hidden_models, ["b"]);
    assert!(!super::settings::set_models_shown(
        &mut provider,
        ids(&["z"]),
        true
    ));
}

fn user(id: &str, text: &str) -> ConversationEntry {
    ConversationEntry {
        event_id: id.to_owned(),
        kind: EntryKind::UserMessage,
        text: text.into(),
        call_id: None,
        thinking: String::new(),
    }
}

fn open(entries: Vec<ConversationEntry>, older: Option<&str>) -> WorkspaceState {
    WorkspaceState {
        history_loading: true,
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-head".to_owned(),
            entries,
            older_before: older.map(str::to_owned),
            streaming: None,
            running: false,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    }
}

#[test]
fn an_offer_found_publishes_the_offer_and_flips_available_together() {
    let offer = mycode_app::UpdateOffer {
        version: "9.9.9".to_owned(),
        notes_url: "https://example.com/releases/v9.9.9".to_owned(),
        asset_url: "https://example.com/app.zip".to_owned(),
        checksum_url: "https://example.com/app.zip.sha256".to_owned(),
    };
    let mut state = WorkspaceState::default();
    reduce(&mut state, FrontendAction::UpdateOfferFound(offer.clone()));
    // The download button's handler reads `last_offer`; an Available
    // state without a published offer is a dead button.
    assert_eq!(state.last_offer, Some(offer));
    assert_eq!(
        state.update,
        UpdateState::Available {
            version: "9.9.9".to_owned(),
            notes_url: "https://example.com/releases/v9.9.9".to_owned(),
        }
    );
    reduce(
        &mut state,
        FrontendAction::UpdateStaged(mycode_app::PreparedUpdate {
            new_binary: std::path::PathBuf::from("stage/app.exe"),
            stage_dir: std::path::PathBuf::from("stage"),
            binary_sha256: "abc123".to_owned(),
        }),
    );
    // Staging reports the offered version, not the running one.
    assert_eq!(
        state.update,
        UpdateState::Ready {
            version: "9.9.9".to_owned(),
        }
    );
}

fn ledger_id(index: usize) -> String {
    format!("evt1-{index:032x}")
}

#[test]
fn the_open_conversation_is_capped_at_the_resident_entry_ceiling() {
    let mut entries: Vec<ConversationEntry> = (0..super::MAX_RESIDENT_ENTRIES + 400)
        .map(|index| user(&ledger_id(index), "row"))
        .collect();
    // A live tool placeholder (`call-*`) is not a ledger event id; if it
    // lands at the front of the kept window, `older_before` must skip it
    // and anchor on the first real ledger entry.
    entries[400] = ConversationEntry {
        event_id: "call-live-1".to_owned(),
        kind: EntryKind::ToolCall,
        text: "read  src/lib.rs".into(),
        call_id: Some("live-1".to_owned()),
        thinking: String::new(),
    };
    let mut state = open(entries, None);
    reduce(&mut state, FrontendAction::TurnArmed);
    let active = state.active.expect("conversation stays open");
    assert_eq!(
        active.entries.len(),
        super::MAX_RESIDENT_ENTRIES,
        "resident entries must be trimmed to the ceiling"
    );
    assert_eq!(
        active.entries.last().expect("kept tail").event_id,
        ledger_id(super::MAX_RESIDENT_ENTRIES + 399),
        "trimming drops the oldest rows, never the tail"
    );
    assert_eq!(
        active.older_before.as_deref(),
        Some(ledger_id(401).as_str()),
        "older_before advances to the first remaining ledger event id so trimmed rows can page back in"
    );
}

#[test]
fn a_failed_or_stopped_turn_marks_the_transcript_tail() {
    let mut state = open(Vec::new(), None);
    reduce(&mut state, FrontendAction::TurnArmed);
    reduce(
        &mut state,
        FrontendAction::ChatFailed {
            message: "provider returned 500".to_owned(),
            head: String::new(),
        },
    );
    assert_eq!(
        state.turn_alert.as_deref(),
        Some("provider returned 500"),
        "a provider failure keeps its message as the red tail row"
    );
    // A new send clears the previous turn's alert before the reply starts.
    reduce(
        &mut state,
        FrontendAction::MessageSent {
            head: "evt-x".to_owned(),
            entry: user("evt-x", "again"),
        },
    );
    assert!(state.turn_alert.is_none());

    // The user stop shows the same red row with a stopped label, not the
    // cancellation sentinel, and raises no error banner.
    reduce(&mut state, FrontendAction::TurnArmed);
    reduce(
        &mut state,
        FrontendAction::ChatFailed {
            message: mycode_app::CHAT_CANCELLED.to_owned(),
            head: String::new(),
        },
    );
    assert!(state.error.is_none());
    assert_eq!(
        state.turn_alert.as_deref(),
        Some(crate::i18n::t("Stopped by user", "已被用户停止")),
        "a user stop also paints the transcript tail"
    );

    // Leaving the conversation takes the alert with it.
    reduce(&mut state, FrontendAction::ConversationParked);
    assert!(state.turn_alert.is_none());
}

#[test]
fn older_loaded_is_not_trimmed_past_the_resident_ceiling() {
    let tail: Vec<ConversationEntry> = (10..super::MAX_RESIDENT_ENTRIES + 10)
        .map(|index| user(&ledger_id(index), "row"))
        .collect();
    let older_page: Vec<ConversationEntry> = (0..10)
        .map(|index| user(&ledger_id(index), "older"))
        .collect();
    let requested = ledger_id(10);
    let mut state = open(tail, Some(&requested));
    reduce(
        &mut state,
        FrontendAction::OlderLoaded {
            session_id: "ses".to_owned(),
            entries: older_page,
            older_before: None,
            requested_before: requested,
        },
    );
    reduce(
        &mut state,
        FrontendAction::ComposerChanged("draft".to_owned()),
    );
    reduce(&mut state, FrontendAction::HistoryLoadStarted);
    let active = state.active.expect("open");
    assert_eq!(active.entries.len(), super::MAX_RESIDENT_ENTRIES + 10);
    assert_eq!(
        active.entries.first().expect("first").event_id,
        ledger_id(0)
    );
    assert!(active.older_before.is_none());
}

#[test]
fn non_turn_failure_preserves_sending_and_streaming_during_a_turn() {
    use crate::view_model::StreamingReply;

    let mut state = WorkspaceState {
        sending: true,
        active: Some(ActiveConversation {
            session_id: "ses".to_owned(),
            branch_id: "br".to_owned(),
            head: "evt-1".to_owned(),
            entries: Vec::new(),
            older_before: None,
            streaming: Some(StreamingReply {
                text: "partial".to_owned(),
                thinking: String::new(),
                status: "Replying".to_owned(),
            }),
            running: false,
            context_used: 0,
            context_cache: 0,
        }),
        ..WorkspaceState::default()
    };
    reduce(
        &mut state,
        FrontendAction::Failed("branch head moved on".to_owned()),
    );
    assert!(state.sending, "non-turn failure must not clear sending");
    assert_eq!(state.error.as_deref(), Some("branch head moved on"));
    assert!(
        state
            .active
            .as_ref()
            .and_then(|active| active.streaming.as_ref())
            .is_some()
    );
}

#[test]
fn inspector_changed_toggles_open_and_keeps_the_pin() {
    let mut state = WorkspaceState::default();
    assert!(!state.inspector_open);
    assert!(!state.inspector_pinned);
    reduce(
        &mut state,
        FrontendAction::InspectorChanged {
            open: true,
            pinned: false,
        },
    );
    assert!(state.inspector_open);
    assert!(!state.inspector_pinned);
    reduce(
        &mut state,
        FrontendAction::InspectorChanged {
            open: true,
            pinned: true,
        },
    );
    assert!(state.inspector_open);
    assert!(state.inspector_pinned);
    reduce(
        &mut state,
        FrontendAction::InspectorChanged {
            open: false,
            pinned: true,
        },
    );
    assert!(!state.inspector_open);
    assert!(state.inspector_pinned);
}

#[test]
fn older_page_prepends_when_the_cursor_still_matches() {
    let mut state = open(vec![user("e5", "new")], Some("e5"));
    reduce(
        &mut state,
        FrontendAction::OlderLoaded {
            session_id: "ses".to_owned(),
            entries: vec![user("e0", "old")],
            older_before: None,
            requested_before: "e5".to_owned(),
        },
    );
    let active = state.active.expect("open");
    assert_eq!(
        active
            .entries
            .iter()
            .map(|entry| entry.event_id.as_str())
            .collect::<Vec<_>>(),
        ["e0", "e5"]
    );
    assert!(active.older_before.is_none());
    assert!(!state.history_loading);
}

#[test]
fn stale_older_page_is_dropped() {
    let mut state = open(vec![user("e5", "new")], Some("e5"));
    reduce(
        &mut state,
        FrontendAction::OlderLoaded {
            session_id: "ses".to_owned(),
            entries: vec![user("e0", "old")],
            older_before: None,
            requested_before: "e9".to_owned(),
        },
    );
    let active = state.active.expect("open");
    assert_eq!(active.entries.len(), 1);
    assert_eq!(active.entries[0].event_id, "e5");
    assert_eq!(active.older_before.as_deref(), Some("e5"));
    assert!(!state.history_loading);
}

#[test]
fn reasoning_pick_stays_on_the_session_and_does_not_dirty_settings() {
    use std::sync::Arc;

    use mycode_providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

    let mut state = WorkspaceState::default();
    let mut settings = crate::view_model::SettingsState::from_settings(
        &mycode_config::AppSettings::default(),
        1,
        Vec::new(),
    );
    settings.saving = true;
    state.settings = Some(settings);
    state.catalog = Some(Arc::new(CatalogDocument {
        providers: vec![CatalogProvider {
            id: "zhipu".to_owned(),
            models: vec![CatalogModel {
                id: "glm-4.7".to_owned(),
                reasoning: true,
                reasoning_efforts: vec!["high".to_owned()],
                ..CatalogModel::default()
            }],
            ..CatalogProvider::default()
        }],
    }));
    state.selected_provider = Some("zhipu".to_owned());
    state.selected_model = Some("glm-4.7".to_owned());
    state.active = Some(mycode_app::ActiveConversation {
        session_id: "session-a".to_owned(),
        branch_id: "branch".to_owned(),
        head: "empty".to_owned(),
        entries: Vec::new(),
        older_before: None,
        streaming: None,
        running: false,
        context_used: 0,
        context_cache: 0,
    });
    reduce(
        &mut state,
        FrontendAction::SettingsReasoningChanged("high".to_owned()),
    );
    let settings = state.settings.expect("settings");
    assert_eq!(settings.reasoning.as_deref(), Some("high"));
    assert_eq!(settings.to_settings().reasoning_effort, None);
    assert!(!settings.dirty);
    assert!(settings.saving);
    assert_eq!(
        mycode_config::session_model(&state.session_models, "session-a")
            .and_then(|pin| pin.reasoning.as_deref()),
        Some("high")
    );
}

#[test]
fn a_new_session_clears_the_previous_context_meter() {
    let turn = TurnStats {
        model: "deepseek-v4-flash".to_owned(),
        input: 25_000,
        output: 100,
        cache: Some(24_300),
        requests: 1,
        elapsed_ms: 10,
    };
    let mut state = WorkspaceState {
        context_used: 25_000,
        context_cache: 24_300,
        last_turn: Some(turn.clone()),
        live_turn: Some(turn),
        active: Some(ActiveConversation {
            session_id: "old".to_owned(),
            branch_id: "branch".to_owned(),
            head: "use-1".to_owned(),
            entries: vec![user("use-1", "hello")],
            older_before: None,
            streaming: None,
            running: false,
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
    assert_eq!(
        state
            .active
            .as_ref()
            .map(|active| active.session_id.as_str()),
        Some("new")
    );
    assert_eq!(state.context_used, 0);
    assert_eq!(state.context_cache, 0);
    assert!(state.last_turn.is_none());
    assert!(state.live_turn.is_none());

    // A reopen whose tail has no usage snapshot stays empty. The previous
    // session's in-memory meter does not leak across.
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(ActiveConversation {
            session_id: "old".to_owned(),
            branch_id: "branch".to_owned(),
            head: "use-1".to_owned(),
            entries: vec![user("use-1", "hello")],
            older_before: None,
            streaming: None,
            running: false,
            context_used: 0,
            context_cache: 0,
        }),
    );
    assert_eq!(state.context_used, 0);
    assert_eq!(state.context_cache, 0);
    assert!(state.last_turn.is_none());
    assert!(state.live_turn.is_none());

    // Reopening a session whose tail recorded a prompt restores that
    // meter. The ring must not wait for another turn.
    reduce(
        &mut state,
        FrontendAction::ConversationOpened(ActiveConversation {
            session_id: "metered".to_owned(),
            branch_id: "branch".to_owned(),
            head: "use-2".to_owned(),
            entries: vec![user("use-2", "hello")],
            older_before: None,
            streaming: None,
            running: false,
            context_used: 42_365,
            context_cache: 41_472,
        }),
    );
    assert_eq!(state.context_used, 42_365);
    assert_eq!(state.context_cache, 41_472);
    assert!(state.last_turn.is_none());
    assert!(state.live_turn.is_none());

    reduce(&mut state, FrontendAction::ConversationParked);
    assert!(state.active.is_none());
    assert_eq!(state.context_used, 0);
    assert_eq!(state.context_cache, 0);
    assert!(state.last_turn.is_none());

    state.context_used = 25_000;
    state.context_cache = 24_300;
    reduce(&mut state, FrontendAction::SessionDeleted);
    assert_eq!(state.context_used, 0);
    assert_eq!(state.context_cache, 0);
}

#[test]
fn a_held_queue_stays_held_until_it_empties_or_a_new_session_opens() {
    let mut state = open(Vec::new(), None);
    reduce(&mut state, FrontendAction::QueuePaused(true));
    assert!(!state.queue_paused, "an empty queue is never held");

    reduce(&mut state, FrontendAction::MessageQueued("one".to_owned()));
    reduce(&mut state, FrontendAction::MessageQueued("two".to_owned()));
    reduce(&mut state, FrontendAction::QueuePaused(true));
    assert!(state.queue_paused);
    reduce(
        &mut state,
        FrontendAction::SteersReturned(vec!["three".to_owned()]),
    );
    assert!(state.queue_paused, "returned steers join a held queue");
    reduce(&mut state, FrontendAction::QueuedMessageRemoved(0));
    assert!(state.queue_paused);
    reduce(&mut state, FrontendAction::QueuedMessageTaken);
    reduce(&mut state, FrontendAction::QueuedMessageRemoved(0));
    assert!(state.queued.is_empty());
    assert!(!state.queue_paused, "the hold ends with the queue");

    reduce(&mut state, FrontendAction::MessageQueued("five".to_owned()));
    reduce(&mut state, FrontendAction::MessageQueued("six".to_owned()));
    reduce(&mut state, FrontendAction::QueuePaused(true));
    reduce(&mut state, FrontendAction::QueueCleared);
    assert!(state.queued.is_empty() && !state.queue_paused);

    reduce(&mut state, FrontendAction::MessageQueued("four".to_owned()));
    reduce(&mut state, FrontendAction::QueuePaused(true));
    reduce(&mut state, FrontendAction::SessionDeleted);
    assert!(state.queued.is_empty() && !state.queue_paused);
}

#[test]
fn a_steer_shows_until_it_commits_and_a_returned_one_is_queued() {
    let mut state = open(Vec::new(), None);
    state.composer_draft = "typing the next one".to_owned();
    // The desktop no longer sends steers; the core may still commit or
    // return ones sent before an update, so the arms stay tested.
    state.pending_steers = vec!["first".to_owned(), "second".to_owned()];
    let session_id = state.active.as_ref().expect("active").session_id.clone();
    reduce(
        &mut state,
        FrontendAction::SteerCommitted {
            session_id,
            head: String::new(),
            entry: user("use-steer", "first"),
        },
    );
    assert_eq!(state.pending_steers, ["second"]);
    reduce(
        &mut state,
        FrontendAction::SteersReturned(vec!["second".to_owned()]),
    );
    assert!(state.pending_steers.is_empty());
    assert_eq!(state.queued, ["second"]);
    assert_eq!(
        state.composer_draft, "typing the next one",
        "a returned steer must not wipe the draft"
    );
    state.pending_steers.push("late".to_owned());
    reduce(
        &mut state,
        FrontendAction::ChatFailed {
            message: mycode_app::CHAT_CANCELLED.to_owned(),
            head: String::new(),
        },
    );
    assert!(state.pending_steers.is_empty());
}

mod turns;
