//! Strict visual-settings authority for the desktop product.
//!
//! [`AppSettings`] is the single settings document at `settings.json`: the
//! desktop settings page edits it through typed APIs and it is published with
//! revision compare-and-swap through the hardened owned-file transaction.
//! Secrets never live here; credentials stay in the secret store.
//!
//! A trailing comma from an earlier writer is repaired and the canonical
//! document is rewritten. Missing fields take their defaults. A document that
//! cannot be parsed or validated is copied aside and replaced with defaults
//! so startup can continue. Session ledgers are not part of this document.

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
    DEFAULT_SUBAGENT_CONCURRENCY, MAX_SUBAGENT_CONCURRENCY, MAX_SUBAGENT_ROLES,
    SubagentRoleSettings, SubagentSettings,
};
pub use tools_shell::{ShellSettings, ToolsSettings, VALID_SHELL_KINDS};
pub use user_agent::default_user_agent;
pub use web::{
    MAX_WEB_BACKENDS, VALID_WEB_KINDS, WebBackendSettings, WebSettings, builtin_web_backends,
};

use serde::{Deserialize, Serialize};

use subagent_roles::subagents_are_default;
use tools_shell::tools_are_default;

use crate::ConfigError;
use crate::authority::AuthorityRevision;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::locked_update_owned_file;

/// Settings document path below the owned home.
pub const SETTINGS_PATH: &str = "settings.json";
/// Maximum encoded authority document size: 256 KiB.
///
/// The cap is domain-neutral on purpose: compaction and export authorities
/// bound their documents with the same limit.
pub const MAX_AUTHORITY_DOCUMENT_BYTES: usize = 256 * 1024;
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
///
/// Six palettes, each a different hue. Older ids are not migrated; a file
/// that names one is reset to the defaults.
pub const VALID_PALETTES: [&str; 6] = ["slate", "ocean", "forest", "dusk", "ember", "aurora"];

/// UI language ids: follow the system, English, or Simplified Chinese.
pub const VALID_LANGUAGES: [&str; 3] = ["auto", "en", "zh"];

/// Interface font-size ids, smallest to largest.
///
/// `s` / `m` / `l` / `xl` are about 12 / 13 / 14 / 16 px of body text.
pub const VALID_FONT_SIZES: [&str; 4] = ["s", "m", "l", "xl"];

/// Named UI font families the settings document accepts.
///
/// Empty and `"system"` are not in this list: both mean the operating-system
/// UI font. A stored id is one of these names, not a free-typed family.
pub const VALID_FONT_FAMILIES: [&str; 4] = ["Inter", "Segoe UI", "PingFang", "Noto Sans"];

/// Stored id for the operating-system UI font.
pub const SYSTEM_FONT_FAMILY: &str = "system";

fn default_palette() -> String {
    "slate".to_owned()
}

fn default_language() -> String {
    "auto".to_owned()
}

fn default_font_size() -> String {
    "m".to_owned()
}

fn default_font_family() -> String {
    SYSTEM_FONT_FAMILY.to_owned()
}

/// Canonical `appearance.fontFamily` value.
///
/// Empty and `"system"` are the OS UI font. Named values must be one of
/// [`VALID_FONT_FAMILIES`]. Anything else is rejected.
#[must_use]
pub fn canonical_font_family(value: &str) -> Option<&'static str> {
    if value.is_empty() || value == SYSTEM_FONT_FAMILY {
        Some(SYSTEM_FONT_FAMILY)
    } else {
        VALID_FONT_FAMILIES.into_iter().find(|id| *id == value)
    }
}

/// Appearance settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppearanceSettings {
    /// Must be `dark`. Any other stored value fails validation, and startup
    /// replaces the whole document with defaults.
    pub theme: String,
    /// One of [`VALID_PALETTES`]. Slate is the default.
    #[serde(default = "default_palette")]
    pub palette: String,
    /// `auto`, `en`, or `zh`.
    #[serde(default = "default_language")]
    pub language: String,
    /// `s`, `m`, `l`, or `xl`. Missing values read as medium.
    #[serde(default = "default_font_size")]
    pub font_size: String,
    /// UI font family. Missing, empty, and `"system"` are the OS UI font.
    /// Named values are one of [`VALID_FONT_FAMILIES`].
    #[serde(default = "default_font_family")]
    pub font_family: String,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "dark".to_owned(),
            palette: default_palette(),
            language: default_language(),
            font_size: default_font_size(),
            font_family: default_font_family(),
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

    /// The only painted theme.
    #[must_use]
    pub fn effective_theme(&self) -> &'static str {
        "dark"
    }

    /// Returns the effective palette. Unknown values read as slate.
    #[must_use]
    pub fn effective_palette(&self) -> &'static str {
        VALID_PALETTES
            .into_iter()
            .find(|id| *id == self.appearance.palette)
            .unwrap_or("slate")
    }

    /// Returns the effective interface font size. Unknown values read as `m`.
    #[must_use]
    pub fn effective_font_size(&self) -> &'static str {
        VALID_FONT_SIZES
            .into_iter()
            .find(|id| *id == self.appearance.font_size)
            .unwrap_or("m")
    }

    /// Returns the effective UI font family.
    ///
    /// Missing, empty, and `"system"` are [`SYSTEM_FONT_FAMILY`]. An unknown
    /// stored value also reads as system here; [`Self::validate`] rejects it
    /// before a document is published or returned from [`read_app_settings`].
    #[must_use]
    pub fn effective_font_family(&self) -> &'static str {
        canonical_font_family(&self.appearance.font_family).unwrap_or(SYSTEM_FONT_FAMILY)
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
            return Err(invalid(&format!(
                "appearance.palette: must be one of {}",
                VALID_PALETTES.join(", ")
            )));
        }
        if !VALID_LANGUAGES.contains(&self.appearance.language.as_str()) {
            return Err(invalid("appearance.language: must be auto, en, or zh"));
        }
        if !VALID_FONT_SIZES.contains(&self.appearance.font_size.as_str()) {
            return Err(invalid("appearance.fontSize: must be s, m, l, or xl"));
        }
        if canonical_font_family(&self.appearance.font_family).is_none() {
            return Err(invalid(&format!(
                "appearance.fontFamily: must be empty, system, or one of {}",
                VALID_FONT_FAMILIES.join(", ")
            )));
        }
        self.validate_subagent_roles()?;
        self.validate_tools_shell()?;
        Ok(())
    }
}

/// Reads and validates `settings.json`.
///
/// A missing document yields the defaults. A trailing comma is accepted and
/// a canonical rewrite is attempted. A document that cannot be parsed or
/// validated is backed up and replaced with defaults; see
/// [`read_app_settings_with_repair`].
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when a damaged document
/// cannot be copied aside.
pub fn read_app_settings(home: &crate::HomeLayout) -> Result<AppSettings, ConfigError> {
    Ok(read_app_settings_with_repair(home)?.0)
}

/// Reads `settings.json`, repairing a damaged document.
///
/// The second value is set when the previous bytes were copied to a
/// `settings.json.broken-*` file and the original path was replaced with
/// defaults. A valid document does not report a repair.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when the backup or the
/// replacement cannot be published.
pub fn read_app_settings_with_repair(
    home: &crate::HomeLayout,
) -> Result<(AppSettings, Option<crate::DocumentRepair>), ConfigError> {
    let loaded = crate::document_repair::load_or_reset(
        home,
        SETTINGS_PATH,
        MAX_AUTHORITY_DOCUMENT_BYTES,
        |bytes| {
            let parsed = decode_settings(bytes)?;
            if parsed.migrated {
                replace_app_settings(home, parsed.revision, &parsed.settings)?;
            }
            Ok(parsed.settings)
        },
        AppSettings::default,
        || publish_default_settings(home),
    )?;
    Ok((loaded.value, loaded.repair))
}

fn publish_default_settings(home: &crate::HomeLayout) -> Result<(), ConfigError> {
    let settings = AppSettings::default();
    settings.validate()?;
    let revision = AuthorityRevision::ABSENT.checked_next()?;
    let document = SerializedSettings {
        format_version: SETTINGS_FORMAT_VERSION,
        kind: SETTINGS_KIND,
        revision: revision.get(),
        settings: &settings,
    };
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    locked_update_owned_file(home, SETTINGS_PATH, MAX_AUTHORITY_DOCUMENT_BYTES, |_| {
        Ok(bytes)
    })
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
    let settings = document.settings;
    // No in-place upgrade. A document that does not validate is rejected
    // and the caller replaces the file with defaults.
    settings.validate()?;
    Ok(ParsedSettings {
        settings,
        revision,
        migrated: decoded.migrated,
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

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, SETTINGS_FORMAT_VERSION, SETTINGS_KIND, SerializedSettings, decode_settings,
    };

    #[test]
    fn legacy_appearance_without_font_size_defaults_to_medium() {
        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 2,
            "appearance": { "theme": "dark", "palette": "ocean", "language": "zh" }
        }"#;
        let parsed = decode_settings(bytes).expect("legacy settings");
        assert_eq!(parsed.settings.appearance.font_size, "m");
        assert_eq!(parsed.settings.effective_font_size(), "m");
        assert_eq!(parsed.settings.appearance.font_family, "system");
        assert_eq!(parsed.settings.effective_font_family(), "system");
        assert_eq!(parsed.settings.appearance.palette, "ocean");
        assert_eq!(parsed.settings.appearance.language, "zh");
        assert!(!parsed.migrated);
    }

    #[test]
    fn font_size_round_trips_through_the_settings_document() {
        let mut settings = AppSettings::default();
        settings.appearance.font_size = "xl".to_owned();
        assert!(settings.validate().is_ok());
        let document = SerializedSettings {
            format_version: SETTINGS_FORMAT_VERSION,
            kind: SETTINGS_KIND,
            revision: 4,
            settings: &settings,
        };
        let bytes = serde_json::to_vec_pretty(&document).expect("encode");
        let parsed = decode_settings(&bytes).expect("decode");
        assert_eq!(parsed.settings.appearance.font_size, "xl");
        assert_eq!(parsed.settings.effective_font_size(), "xl");
        assert!(bytes.windows(10).any(|window| window == br#""fontSize""#));

        settings.appearance.font_size = "huge".to_owned();
        let error = settings.validate().expect_err("unknown size");
        assert!(error.summary().contains("fontSize"), "{}", error.summary());
    }

    #[test]
    fn font_family_empty_or_system_means_the_os_ui_font() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.appearance.font_family, "system");
        assert_eq!(settings.effective_font_family(), "system");

        settings.appearance.font_family.clear();
        assert!(settings.validate().is_ok());
        assert_eq!(settings.effective_font_family(), "system");

        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 3,
            "appearance": {
                "theme": "dark",
                "palette": "slate",
                "language": "auto",
                "fontSize": "m",
                "fontFamily": ""
            }
        }"#;
        let parsed = decode_settings(bytes).expect("empty family");
        assert_eq!(parsed.settings.appearance.font_family, "");
        assert_eq!(parsed.settings.effective_font_family(), "system");
        assert!(!parsed.migrated);

        settings.appearance.font_family = "Comic Sans".to_owned();
        let error = settings.validate().expect_err("unknown family");
        assert!(
            error.summary().contains("fontFamily"),
            "{}",
            error.summary()
        );
    }

    #[test]
    fn appearance_round_trips_through_read_and_replace() {
        let parent = std::env::temp_dir().join(format!(
            "mycode-appearance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&parent).expect("temp parent");
        let guard = TempDir(parent);
        let root = guard.0.join("home");
        std::fs::create_dir(&root).expect("home");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private home");
        }
        let home = crate::HomeLayout::from_root(&root).expect("layout");

        let mut settings = AppSettings::default();
        settings.appearance.palette = "aurora".to_owned();
        settings.appearance.font_size = "l".to_owned();
        settings.appearance.font_family = "Noto Sans".to_owned();
        let revision =
            super::replace_app_settings(&home, crate::AuthorityRevision::ABSENT, &settings)
                .expect("publish");

        let loaded = super::read_app_settings(&home).expect("read");
        assert_eq!(loaded.appearance.palette, "aurora");
        assert_eq!(loaded.appearance.font_size, "l");
        assert_eq!(loaded.appearance.font_family, "Noto Sans");
        assert_eq!(loaded.effective_font_family(), "Noto Sans");
        assert_eq!(loaded.effective_palette(), "aurora");
        assert_eq!(loaded.effective_font_size(), "l");

        let bytes = std::fs::read(home.root().join("settings.json")).expect("settings.json");
        let text = String::from_utf8(bytes).expect("utf-8");
        assert!(text.contains("\"palette\": \"aurora\""), "{text}");
        assert!(text.contains("\"fontSize\": \"l\""), "{text}");
        assert!(text.contains("\"fontFamily\": \"Noto Sans\""), "{text}");

        settings.appearance.font_family = "system".to_owned();
        settings.appearance.palette = "slate".to_owned();
        settings.appearance.font_size = "m".to_owned();
        super::replace_app_settings(&home, revision, &settings).expect("publish system");
        let restored = super::read_app_settings(&home).expect("read system");
        assert_eq!(restored.appearance.font_family, "system");
        assert_eq!(restored.effective_font_family(), "system");
        assert_eq!(restored.appearance.palette, "slate");
        assert_eq!(restored.appearance.font_size, "m");
    }

    #[test]
    fn trailing_comma_repair_surfaces_a_rewrite_failure() {
        let parent = std::env::temp_dir().join(format!(
            "mycode-settings-comma-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&parent).expect("temp parent");
        let guard = TempDir(parent);
        let root = guard.0.join("home");
        std::fs::create_dir(&root).expect("home");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private home");
        }
        let home = crate::HomeLayout::from_root(&root).expect("layout");
        let body =
            br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":9223372036854775807,}"#;
        let path = root.join("settings.json");
        std::fs::write(&path, body).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        }
        let error = super::read_app_settings(&home).expect_err("rewrite must fail");
        assert_eq!(error.kind(), crate::ConfigErrorKind::RevisionExhausted);
        let kept = std::fs::read(&path).expect("original remains");
        assert!(kept.windows(2).any(|window| window == b",}"), "{kept:?}");
    }

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
