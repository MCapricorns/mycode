//! Pure frontend view-model: state, actions, and reducer.
//!
//! This module tree has no GPUI dependency. The render layer turns
//! [`WorkspaceState`] into elements and feeds [`FrontendAction`]s back, so the
//! product behavior stays testable without a GPU or window. This file is the
//! import facade; the declarations live in topical submodules and the
//! transitions in [`reduce`].
mod chat;
mod command_palette;
mod models;
pub mod nav;
mod projects;
mod settings;
mod state;
mod subagents;
mod usage;

mod reduce;
// The transcript vocabulary is the core's protocol, not a rendering concern:
// it is defined in `mycode-app` and re-exported here so render code keeps one
// import path.
pub use mycode_app::{
    ActiveConversation, CHAT_CANCELLED, ConversationEntry, EntryKind, SessionSummary,
    StreamingReply,
};

pub use self::chat::{
    LiveJob, MentionGroup, MentionItem, MentionKind, capped_artisan_count, capped_artisan_status,
    elapsed_label, session_title_matches, subagent_cancellable, subagent_wait_status,
};
pub use self::command_palette::{
    PaletteCatalog, PaletteCommand, PaletteGroup, PaletteGroupId, PaletteItem, palette_catalog,
    palette_item,
};
pub use self::models::{
    canonical_model_id, effective_reasoning_level, model_context_window, model_display_name,
    provider_display_name, selected_reasoning_level,
};
pub use self::nav::{
    DismissLayer, SettingsBack, highlight, moved, settings_back_target, top_dismiss_layer,
};
pub use self::projects::{
    has_open_folder, newest_session_in_project, open_session_is_empty, project_label,
    project_of_session, project_session_rows, same_project_path, session_rows,
    task_surface_visible,
};
pub use self::reduce::{
    ComposerSubmit, PREPARING_ROW_PREFIX, close_floating_menus, composer_submit, is_preparing_row,
    preferred_slash_index, reasoning_levels_for, reduce, selected_model_supports_reasoning,
    selected_reasoning_levels,
};
pub use self::state::{MAX_COMPOSER_CHARS, WorkspaceState};
pub use self::subagents::{
    SubagentHistory, SubagentSession, agent_run_elapsed_ms, agent_run_status_label,
    agent_workflow_status_label, workflow_terminal,
};
pub use self::usage::{
    CacheAlert, ContextMeterParts, TurnStats, cache_alert_text, compact_count, context_meter_parts,
    context_remaining_percent, turn_cache_hit,
};

// Kept public API: the `FrontendAction` vocabulary is `pub`, so its payload
// types must be too (`private_interfaces`).
pub use self::settings::{ModelsSubview, SettingsSection, SettingsState, SkillEntry};
pub use self::state::{FrontendAction, MainView, ProxyTestState, UpdateState};
pub use self::usage::CacheBreak;
