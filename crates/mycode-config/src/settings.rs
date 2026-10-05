//! Strict visual-settings authority for the desktop product.
//!
//! [`AppSettings`] is the single settings document at `settings.json`: the
//! desktop settings page edits it through typed APIs and it is published with
//! revision compare-and-swap through the hardened owned-file transaction.
//! Secrets never live here; credentials stay in the Host vault.
//!
//! A trailing comma from an earlier writer is repaired and the canonical
//! document is rewritten. Missing fields take their defaults. Unknown fields
//! and a wrong kind still fail closed.

mod mcp;
mod providers;
mod subagent_roles;
mod tools_shell;
mod user_agent;
mod web;

pub use mcp::{
    MAX_MCP_ENV_VARS, MAX_MCP_SERVERS, McpServerSettings, builtin_mcp_servers, is_mcp_executable,
    split_command_line,
};
pub use providers::{
    MAX_MODELS_PER_PROVIDER, MAX_PROVIDERS, ProviderSettings, VALID_PROVIDER_KINDS,
};
pub use subagent_roles::{
    MAX_SUBAGENT_CONCURRENCY, MAX_SUBAGENT_ROLES, SubagentRoleSettings, SubagentSettings,
};
pub use tools_shell::{ShellSettings, ToolsSettings, VALID_SHELL_KINDS};
pub use user_agent::default_user_agent;
pub use web::{
    MAX_WEB_BACKENDS, VALID_WEB_KINDS, WebBackendSettings, WebSettings, builtin_web_backends,
};

use serde::{Deserialize, Serialize};

use subagent_roles::subagents_are_default;
use tools_shell::{retire_unsupported_shell, tools_are_default};

use crate::ConfigError;
use crate::authority::AuthorityRevision;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};

/// Settings document path below the owned home.
pub const SETTINGS_PATH: &str = "settings.json";
/// Maximum encoded authority document size: 256 KiB.
///
/// The cap is domain-neutral on purpose: compaction and export authorities
/// bound their documents with the same limit.
pub const MAX_AUTHORITY_DOCUMENT_BYTES: usize = 256 * 1024;
/// Back-compat alias of [`MAX_AUTHORITY_DOCUMENT_BYTES`] under the previous
/// settings-scoped name, kept so existing cross-crate callers keep compiling.
pub const MAX_SETTINGS_BYTES: usize = MAX_AUTHORITY_DOCUMENT_BYTES;
/// Settings format version.
pub const SETTINGS_FORMAT_VERSION: u32 = 1;
/// Settings kind tag.
pub const SETTINGS_KIND: &str = "mycode-app-settings";
/// Maximum string field length in bytes.
pub(super) const MAX_FIELD_BYTES: usize = 8 * 1024;
/// Base URL maximum length.
pub(super) const MAX_URL_BYTES: usize = 2 * 1024;

/// Usage accounting settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSettings {
    /// Built-in usage accounting enabled.
    pub enabled: bool,
}

impl Default for UsageSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Palette ids the desktop can paint. Slate is the default.
pub const VALID_PALETTES: [&str; 8] = [
    "slate", "ocean", "forest", "dusk", "sand", "rose", "ink", "moss",
];

/// UI language ids: follow the system, English, or Simplified Chinese.
pub const VALID_LANGUAGES: [&str; 3] = ["auto", "en", "zh"];

fn default_palette() -> String {
    "slate".to_owned()
}

fn default_language() -> String {
    "auto".to_owned()
}

/// Appearance settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppearanceSettings {
    /// Always `dark`. A stored `light` is rewritten on read.
    pub theme: String,
    /// `slate`, `ocean`, `forest`, `dusk`, `sand`, `rose`, `ink`, or `moss`.
    #[serde(default = "default_palette")]
    pub palette: String,
    /// `auto`, `en`, or `zh`.
    #[serde(default = "default_language")]
    pub language: String,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "dark".to_owned(),
            palette: default_palette(),
            language: default_language(),
        }
    }
}

/// The complete settings document.
///
/// Missing fields take [`Default`] so an older copy can be read and rewritten
/// with the options this build expects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AppSettings {
    /// Outbound User-Agent. Defaults to the pi agent identity so the
    /// value is present in `settings.json` and stays configurable.
    pub user_agent: String,
    /// Configured providers.
    pub providers: Vec<ProviderSettings>,
    /// Web search settings.
    pub web: WebSettings,
    /// Usage settings.
    pub usage: UsageSettings,
    /// MCP servers.
    pub mcp_servers: Vec<McpServerSettings>,
    /// Appearance.
    pub appearance: AppearanceSettings,
    /// Requested reasoning effort from models.dev options; absent keeps the
    /// provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Subagent delegation settings.
    #[serde(default, skip_serializing_if = "subagents_are_default")]
    pub subagents: SubagentSettings,
    /// Tool-runtime preferences, including the platform shell.
    #[serde(default, skip_serializing_if = "tools_are_default")]
    pub tools: ToolsSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            user_agent: default_user_agent(),
            providers: Vec::new(),
            // Both first-class vendors ship ready; the user pastes an API
            // key and enables one. At most one backend may be enabled.
            web: WebSettings {
                backends: builtin_web_backends(),
            },
            usage: UsageSettings { enabled: true },
            mcp_servers: Vec::new(),
            appearance: AppearanceSettings::default(),
            reasoning_effort: None,
            subagents: SubagentSettings::default(),
            tools: ToolsSettings::default(),
        }
    }
}

impl AppSettings {
    /// Returns the effective User-Agent, falling back to the pi agent
    /// identity when unset.
    #[must_use]
    pub fn effective_user_agent(&self) -> String {
        let configured = self.user_agent.trim();
        if configured.is_empty() {
            default_user_agent()
        } else {
            configured.to_owned()
        }
    }

    /// The only painted theme. Stored `light` is retired before this is read.
    #[must_use]
    pub fn effective_theme(&self) -> &'static str {
        "dark"
    }

    /// Rewrites a stored light theme to dark.
    ///
    /// Light mode is no longer painted. Call this before [`Self::validate`]
    /// so an older document stays readable and can be published back as dark.
    pub fn retire_light_theme(&mut self) -> bool {
        if self.appearance.theme == "light" {
            self.appearance.theme = "dark".to_owned();
            true
        } else {
            false
        }
    }

    /// Returns the effective palette. Unknown values read as slate.
    #[must_use]
    pub fn effective_palette(&self) -> &'static str {
        VALID_PALETTES
            .into_iter()
            .find(|id| *id == self.appearance.palette)
            .unwrap_or("slate")
    }

    /// Validates the complete document.
    ///
    /// Family-specific bounds, grammar, and cross-field rules live in the
    /// owning submodule and are invoked in document order.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for any bound,
    /// grammar, or cross-field violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        bounded_text(&self.user_agent, MAX_FIELD_BYTES)
            .map_err(|_| invalid("userAgent: too long or contains control characters"))?;
        if let Some(level) = self.reasoning_effort.as_deref()
            && (level == "default" || crate::RoleThinking::parse(level).is_none())
        {
            return Err(invalid(
                "reasoningEffort: must be a models.dev option (off, on, minimal, low, medium, high, xhigh, max)",
            ));
        }
        self.validate_providers()?;
        self.validate_web()?;
        self.validate_mcp()?;
        if self.appearance.theme != "dark" {
            return Err(invalid("appearance.theme: must be dark"));
        }
        if !VALID_PALETTES.contains(&self.appearance.palette.as_str()) {
            return Err(invalid(
                "appearance.palette: must be slate, ocean, forest, dusk, sand, rose, ink, or moss",
            ));
        }
        if !VALID_LANGUAGES.contains(&self.appearance.language.as_str()) {
            return Err(invalid("appearance.language: must be auto, en, or zh"));
        }
        self.validate_subagent_roles()?;
        self.validate_tools_shell()?;
        Ok(())
    }
}

/// Reads and validates `settings.json` without creating filesystem objects.
///
/// A missing document yields the defaults; present documents must validate
/// strictly.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security, oversized content, or
/// strict validation failures.
pub fn read_app_settings(home: &crate::HomeLayout) -> Result<AppSettings, ConfigError> {
    let bytes = read_owned_file(home, SETTINGS_PATH, MAX_AUTHORITY_DOCUMENT_BYTES)?;
    let Some(bytes) = bytes else {
        return Ok(AppSettings::default());
    };
    let parsed = decode_settings(bytes.as_slice())?;
    if parsed.migrated {
        let _ = replace_app_settings(home, parsed.revision, &parsed.settings);
    }
    Ok(parsed.settings)
}

/// Replaces `settings.json` under revision compare-and-swap.
///
/// A missing document has logical revision zero. The replacement is fully
/// validated before publication.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] for a stale expectation and
/// [`ConfigError`] for validation or transaction failures.
pub fn replace_app_settings(
    home: &crate::HomeLayout,
    expected_revision: AuthorityRevision,
    settings: &AppSettings,
) -> Result<AuthorityRevision, ConfigError> {
    settings.validate()?;
    let mut published_revision = None;
    locked_update_owned_file(
        home,
        SETTINGS_PATH,
        MAX_AUTHORITY_DOCUMENT_BYTES,
        |current| {
            let current_revision = match current {
                Some(bytes) => parse_document_header(bytes)?,
                None => AuthorityRevision::ABSENT,
            };
            if current_revision != expected_revision {
                return Err(ConfigError::new(ConfigErrorKind::RevisionConflict));
            }
            let revision = current_revision.checked_next()?;
            let document = SerializedSettings {
                format_version: SETTINGS_FORMAT_VERSION,
                kind: SETTINGS_KIND,
                revision: revision.get(),
                settings,
            };
            let mut bytes = serde_json::to_vec_pretty(&document)
                .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
            bytes.push(b'\n');
            if bytes.len() > MAX_AUTHORITY_DOCUMENT_BYTES {
                return Err(ConfigError::new(ConfigErrorKind::Oversized));
            }
            published_revision = Some(revision);
            Ok(bytes)
        },
    )?;
    published_revision.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedSettings<'a> {
    format_version: u32,
    kind: &'static str,
    revision: u64,
    #[serde(flatten)]
    settings: &'a AppSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedSettings {
    format_version: u32,
    kind: String,
    revision: u64,
    #[serde(flatten)]
    settings: AppSettings,
}

/// Reads the current revision without full validation of the body.
struct ParsedSettings {
    settings: AppSettings,
    revision: AuthorityRevision,
    migrated: bool,
}

fn parse_document_header(bytes: &[u8]) -> Result<AuthorityRevision, ConfigError> {
    Ok(decode_settings(bytes)?.revision)
}

fn decode_settings(bytes: &[u8]) -> Result<ParsedSettings, ConfigError> {
    let decoded =
        crate::json_recover::decode_json::<DeserializedSettings>(bytes).map_err(|_| {
            ConfigError::authority_rejection().with_detail(
                "settings.json: unknown field, wrong type, or JSON that a comma repair cannot fix",
            )
        })?;
    let document = decoded.value;
    if document.format_version != SETTINGS_FORMAT_VERSION || document.kind != SETTINGS_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("settings.json: formatVersion or kind does not match this build"));
    }
    let revision = AuthorityRevision::new(document.revision)?;
    let mut settings = document.settings;
    let retired_shell = retire_unsupported_shell(&mut settings);
    let retired_theme = settings.retire_light_theme();
    settings.validate()?;
    Ok(ParsedSettings {
        settings,
        revision,
        migrated: decoded.migrated || retired_shell || retired_theme,
    })
}

fn bounded_text(value: &str, max: usize) -> Result<(), ConfigError> {
    if value.len() <= max && !value.contains(['\0', '\r', '\n']) {
        Ok(())
    } else {
        Err(ConfigError::authority_rejection())
    }
}

fn is_portable_id(value: &str) -> bool {
    crate::home::is_valid_portable_id(value)
}

fn is_https_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    if value.len() > MAX_URL_BYTES {
        return false;
    }
    let host = rest.split('/').next().unwrap_or_default();
    !host.is_empty()
        && !host
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
        && host
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b'-')
}
