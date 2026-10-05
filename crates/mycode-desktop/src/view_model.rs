//! Pure desktop view-model: state, actions, and reducer.
//!
//! This module tree has no GPUI dependency. The render layer turns
//! [`WorkspaceState`] into elements and feeds [`DesktopAction`]s back, so the
//! product behavior stays testable without a GPU or window. This file is the
//! import facade; the declarations live in topical submodules and the
//! transitions in [`reduce`].
mod chat;
mod models;
mod projects;
mod settings;
mod state;
mod usage;

mod reduce;
// The transcript vocabulary is the core's protocol, not a rendering concern:
// it is defined in `mycode-app` and re-exported here so render code keeps one
// import path.
pub(crate) use mycode_app::{
    ActiveConversation, CHAT_CANCELLED, ConversationEntry, EntryKind, SessionSummary,
    StreamingReply,
};

pub(crate) use self::chat::{
    COMPOSER_COMMANDS, ComposerMention, LiveJob, MentionKind, session_title_matches,
};
pub(crate) use self::models::{rank_model_ids, selected_reasoning_level};
pub(crate) use self::projects::{
    active_workspace, newest_session_in_project, project_of_session, same_project_path,
    task_surface_visible, workspace_of_session,
};
pub(crate) use self::reduce::{
    close_floating_menus, reasoning_levels_for, reduce, selected_model_supports_reasoning,
    selected_reasoning_levels,
};
pub(crate) use self::state::{MAX_COMPOSER_CHARS, MAX_QUEUED_MESSAGES, WorkspaceState};
pub(crate) use self::usage::{
    TurnStats, UsageTotal, cache_percent, parse_usage_text, usage_key_matches,
};

// Kept public API: the `DesktopAction` vocabulary is `pub`, so its payload
// types must be too (`private_interfaces`), and the suggestion projection is
// exercised today only by render code outside this module, where a
// restricted visibility would trip `dead_code` in non-test builds.
pub use self::models::suggested_model_ids;
pub use self::settings::{
    CopilotSignIn, McpSubview, ModelsSubview, SettingsSection, SettingsState, SkillEntry,
    WebSubview,
};
pub use self::state::{DesktopAction, MainView, UpdateState};
