//! Pure desktop view-model: state, actions, and reducer.
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
    effective_reasoning_level, model_context_window, model_display_name, provider_display_name,
    rank_model_ids, selected_reasoning_level,
};
pub use self::nav::{
    DismissLayer, SettingsBack, highlight, moved, settings_back_target, top_dismiss_layer,
};
pub use self::projects::{
    has_open_folder, newest_session_in_project, open_session_is_empty, project_label,
    project_of_session, same_project_path, session_rows, sessions_removed_with_root,
    task_surface_visible,
};
pub use self::reduce::{
    ComposerSubmit, close_floating_menus, composer_submit, preferred_slash_index, preset_pick,
    reasoning_levels_for, reduce, selected_model_supports_reasoning, selected_reasoning_levels,
};
pub use self::state::{MAX_COMPOSER_CHARS, MAX_QUEUED_MESSAGES, WorkspaceState};
pub use self::usage::{
    CacheAlert, ContextMeterParts, TurnStats, cache_alert_text, compact_count, context_meter_parts,
    context_remaining_percent, turn_cache_hit,
};

// Kept public API: the `FrontendAction` vocabulary is `pub`, so its payload
// types must be too (`private_interfaces`).
pub use self::settings::{
    CopilotSignIn, McpSubview, ModelsSubview, SettingsSection, SettingsState, SkillEntry,
};
pub use self::state::{FrontendAction, MainView, UpdateState};
pub use self::usage::CacheBreak;
