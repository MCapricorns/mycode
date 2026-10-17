//! Strict owned configuration authorities for MYCode.
//!
//! [`HomeLayout`] resolves the relocatable owned home root lexically, without
//! filesystem I/O. Authority files are lazy, bounded, strict JSON documents
//! published through the locked owned-file machinery. Settings and secrets
//! carry a revision compare-and-swap; advisory state is last-writer-wins.
//! There is no alias, layered merge, or fallback for obsolete layouts:
//! unrelated paths are never inputs.
//!
//! Product documents (`settings.json`, `secrets.json`, `ui.json`) are strict:
//! a file that cannot be parsed or validated is copied aside and republished,
//! keeping whatever fields still decode so startup can continue. Session
//! ledgers are not repaired that way.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

mod authority;
mod compaction;
mod document_repair;
mod error;
mod home;
mod mcp_import;
mod project_mcp;
mod resources;
mod secrets;
mod secure_fs;
mod settings;
mod subagents;
mod ui_state;

#[doc(inline)]
pub use authority::AuthorityRevision;
#[doc(inline)]
pub use compaction::{
    COMPACTION_FORMAT_VERSION, COMPACTION_KIND, CompactionCheckpoint, MAX_SUMMARY_CHARS,
    estimate_token_count, read_compaction, write_compaction,
};
pub use document_repair::{DocumentRepair, quarantine_owned_bytes};
pub use error::{ConfigError, ConfigErrorKind};
pub use home::{HomeEnv, HomeLayout, SCRATCH_DIR, SESSIONS_DIR};
pub use mcp_import::{normalize_api_key, parse_mcp_import};
pub use project_mcp::project_mcp_servers;
#[doc(inline)]
pub use resources::{
    MAX_SKILLS, ResourceFile, SkillFile, discover_resources, discover_skills, os_user_home,
    render_resource_prompt, render_skill_catalog, skill_read_roots,
};
pub use secrets::{
    MAX_SECRETS_BYTES, ProviderSecrets, SECRETS_FORMAT_VERSION, SECRETS_PATH,
    read_provider_secrets, read_provider_secrets_with_repair, replace_provider_secrets,
    salvage_provider_secrets,
};
#[doc(inline)]
pub use secure_fs::owned_file::{locked_update_owned_file, read_owned_file};

/// Test-only fixture writer: publishes `bytes` through the app's own secure
/// transaction so the file carries the platform's private permissions (Unix
/// 0600, Windows protected DACL) instead of the temp directory's inherited
/// ones. Plain `std::fs::write` fixtures fail the read path's fail-closed
/// owner/ACL checks on Windows.
#[cfg(test)]
pub(crate) fn write_owned_test_bytes(
    home: &HomeLayout,
    relative: &str,
    bytes: &[u8],
) -> Result<(), ConfigError> {
    locked_update_owned_file(home, relative, bytes.len().max(1), |_| Ok(bytes.to_vec()))
}
#[doc(inline)]
pub use settings::{
    AppSettings, AppearanceSettings, MAX_AUTHORITY_DOCUMENT_BYTES, MAX_DISABLED_SKILLS,
    MAX_HIDDEN_MODELS_PER_PROVIDER, MAX_MCP_ENV_VARS, MAX_MCP_SERVERS, MAX_MODELS_PER_PROVIDER,
    MAX_PROVIDERS, MAX_WEB_BACKENDS, McpServerSettings, PROXY_AUTO, PROXY_DIRECT, PROXY_SCHEMES,
    ProviderSettings, ProxyPreference, SETTINGS_FORMAT_VERSION, SETTINGS_PATH,
    SUBAGENT_CONCURRENCY, SkillsSettings, SubagentRoleSettings, SubagentSettings, VALID_FONT_SIZES,
    VALID_LANGUAGES, VALID_THEMES, WebBackendSettings, WebSettings, builtin_mcp_servers,
    builtin_web_backends, is_mcp_executable, merged_web_backends, read_app_settings,
    read_app_settings_with_repair, replace_app_settings, salvage_app_settings, split_command_line,
    web_key_env,
};
#[doc(inline)]
pub use subagents::{
    RoleCatalog, RoleIsolation, RoleOrigin, RoleThinking, SubagentRole, builtin_roles,
    discover_roles,
};
#[doc(inline)]
pub use ui_state::{
    MAX_RECENT_PROJECTS, MAX_SESSION_PROJECTS, MAX_SESSION_WORKSPACES, MAX_WINDOW_EDGE_PX,
    MAX_WORKSPACE_ROOTS, MAX_WORKSPACES, ModelPin, SessionModelPin, UiState, WindowSize,
    WorkspaceDef, is_valid_effort_token, read_ui_state, read_ui_state_with_repair, remember_model,
    replace_ui_state, salvage_ui_state, same_project_path, session_model, upsert_session_model,
};

/// Returns the wall-clock duration since the Unix epoch, saturating at zero
/// when the system clock predates the epoch.
pub(crate) fn unix_now() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}
