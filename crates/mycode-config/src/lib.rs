//! Strict owned configuration authorities for MYCode.
//!
//! [`HomeLayout`] resolves the relocatable owned home root lexically, without
//! filesystem I/O. Authority files are lazy, bounded, strict JSON documents
//! published through the locked owned-file machinery with revision
//! compare-and-swap. There is no alias, layered merge, or fallback for
//! obsolete layouts: unrelated paths are never inputs.
//!
//! Product documents (`settings.json`, `secrets.json`, `ui.json`) recover
//! trailing commas from earlier writers, fill missing fields, and rewrite the
//! canonical document.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

mod authority;
mod compaction;
mod error;
mod home;
mod json_recover;
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
    estimate_token_count, estimate_tokens, read_compaction, write_compaction,
};
pub use error::{ConfigError, ConfigErrorKind};
pub use home::{
    HomeEnv, HomeLayout, MYCODE_DIR_NAME, MYCODE_HOME_ENV, SCRATCH_DIR, SESSIONS_DIR,
    session_relative,
};
pub use mcp_import::{normalize_api_key, parse_mcp_import};
pub use project_mcp::project_mcp_servers;
#[doc(inline)]
pub use resources::{
    ResourceFile, SkillFile, discover_resources, discover_skills, render_resource_prompt,
    render_skill_catalog,
};
pub use secrets::{
    MAX_SECRETS_BYTES, ProviderSecrets, SECRETS_FORMAT_VERSION, SECRETS_KIND, SECRETS_PATH,
    read_provider_secrets, replace_provider_secrets,
};
#[doc(inline)]
pub use secure_fs::owned_file::{
    ensure_owned_directory, locked_update_owned_file, read_owned_file,
};
#[doc(inline)]
pub use settings::{
    AppSettings, AppearanceSettings, DEFAULT_SUBAGENT_CONCURRENCY, MAX_AUTHORITY_DOCUMENT_BYTES,
    MAX_MCP_ENV_VARS, MAX_MCP_SERVERS, MAX_MODELS_PER_PROVIDER, MAX_PROVIDERS,
    MAX_SUBAGENT_CONCURRENCY, MAX_SUBAGENT_ROLES, MAX_WEB_BACKENDS, McpServerSettings,
    ProviderSettings, SETTINGS_FORMAT_VERSION, SETTINGS_KIND, SETTINGS_PATH, ShellSettings,
    SubagentRoleSettings, SubagentSettings, ToolsSettings, UsageSettings, VALID_FONT_SIZES,
    VALID_LANGUAGES, VALID_PALETTES, VALID_PROVIDER_KINDS, VALID_SHELL_KINDS, VALID_WEB_KINDS,
    WebBackendSettings, WebSettings, builtin_mcp_servers, builtin_web_backends, default_user_agent,
    is_mcp_executable, read_app_settings, replace_app_settings, split_command_line,
};
#[doc(inline)]
pub use subagents::{
    MAX_ROLE_BYTES, MAX_ROLES, ROLE_DIR_NAME, RoleCatalog, RoleIsolation, RoleOrigin, RoleThinking,
    SubagentRole, builtin_roles, discover_roles,
};
#[doc(inline)]
pub use ui_state::{
    MAX_RECENT_MODELS, MAX_RECENT_PROJECTS, MAX_SESSION_PROJECTS, MAX_SESSION_WORKSPACES,
    MAX_STARRED_MODELS, MAX_TRUSTED_PROJECTS, MAX_WORKSPACE_NAME_CHARS, MAX_WORKSPACE_ROOTS,
    MAX_WORKSPACES, ModelPin, UI_STATE_FORMAT_VERSION, UI_STATE_KIND, UI_STATE_PATH, UiState,
    WorkspaceDef, read_ui_state, remember_model, replace_ui_state, toggle_star,
};
