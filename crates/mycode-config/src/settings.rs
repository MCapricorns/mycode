//! Strict product-settings authority for the MYCode frontends.
//!
//! [`AppSettings`] is the single settings document at `settings.json`: the
//! desktop settings pages edit it through typed APIs and it is published with
//! revision compare-and-swap through the hardened owned-file transaction.
//! The gateway binding includes magpie's key; MCP credentials live in the
//! separate secret store.
//!
//! A valid magpie-created bootstrap without any envelope fields is imported
//! as revision 1 without a damage report. Other documents that cannot be
//! parsed or validated are copied aside and replaced: whatever still decodes
//! and validates is kept, the rest is rebuilt from defaults, so startup can
//! continue without re-entering whole sections. Session ledgers are not part
//! of this document.

mod magpie;
mod mcp;
mod providers;
mod proxy;
mod skills;
mod subagent_roles;

pub use magpie::{MAGPIE_PROVIDER_ID, MagpieSettings};
pub(crate) use mcp::{MAX_MCP_ARGS, MAX_MCP_HEADERS, is_env_var_name, is_header_name};
pub use mcp::{
    MAX_MCP_ENV_VARS, MAX_MCP_SERVERS, McpServerSettings, is_mcp_executable, split_command_line,
};
pub use providers::{MAX_HIDDEN_MODELS_PER_PROVIDER, MAX_MODELS_PER_PROVIDER, ProviderSettings};
pub use proxy::{PROXY_SCHEMES, ProxyPreference};
pub use skills::{MAX_DISABLED_SKILLS, SkillsSettings};
pub use subagent_roles::{SUBAGENT_CONCURRENCY, SubagentRoleSettings, SubagentSettings};

use serde::{Deserialize, Serialize};

use skills::skills_are_default;
use subagent_roles::subagents_are_default;

use crate::ConfigError;
use crate::authority::AuthorityRevision;
use crate::error::ConfigErrorKind;
use crate::secure_fs::owned_file::locked_update_owned_file;

/// Settings document path below the owned home.
pub const SETTINGS_PATH: &str = "settings.json";
/// Maximum encoded authority document size: 256 KiB.
///
/// The cap is domain-neutral on purpose: the compaction authority bounds
/// its documents with the same limit.
pub const MAX_AUTHORITY_DOCUMENT_BYTES: usize = 256 * 1024;
/// Settings format version.
pub const SETTINGS_FORMAT_VERSION: u32 = 1;
/// Settings kind tag.
const SETTINGS_KIND: &str = "mycode-app-settings";
/// Maximum string field length in bytes.
pub(crate) const MAX_FIELD_BYTES: usize = 8 * 1024;
/// Base URL maximum length.
pub(super) const MAX_URL_BYTES: usize = 2 * 1024;

/// Painted appearance: dark or light.
pub const VALID_THEMES: [&str; 2] = ["dark", "light"];

/// UI language ids: follow the system, English, or Simplified Chinese.
pub const VALID_LANGUAGES: [&str; 3] = ["auto", "en", "zh"];

/// Interface font-size ids, smallest to largest.
///
/// `s` / `m` / `l` / `xl` are 12 / 14 / 16 / 18 px of body text.
pub const VALID_FONT_SIZES: [&str; 4] = ["s", "m", "l", "xl"];

fn default_language() -> String {
    "auto".to_owned()
}

fn default_font_size() -> String {
    "m".to_owned()
}

/// Appearance settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppearanceSettings {
    /// `dark` or `light`. Any other stored value fails validation, and
    /// startup resets the document (keeping what still decodes).
    pub theme: String,
    /// `auto`, `en`, or `zh`.
    #[serde(default = "default_language")]
    pub language: String,
    /// `s`, `m`, `l`, or `xl`. Missing values read as medium.
    #[serde(default = "default_font_size")]
    pub font_size: String,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "dark".to_owned(),
            language: default_language(),
            font_size: default_font_size(),
        }
    }
}

/// The complete settings document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AppSettings {
    /// Outbound proxy: empty to follow the system proxy, or a proxy URL.
    /// See [`ProxyPreference`].
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proxy: String,
    /// The local magpie gateway, written by magpie's agent plugin.
    #[serde(default, skip_serializing_if = "MagpieSettings::is_default")]
    pub magpie: MagpieSettings,
    /// Appearance.
    pub appearance: AppearanceSettings,
    /// Requested reasoning effort; absent keeps the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Subagent delegation settings.
    #[serde(default, skip_serializing_if = "subagents_are_default")]
    pub subagents: SubagentSettings,
    /// Skills the user switched off.
    #[serde(default, skip_serializing_if = "skills_are_default")]
    pub skills: SkillsSettings,
}

impl AppSettings {
    /// Returns the effective appearance. Unknown values read as dark.
    #[must_use]
    pub fn effective_theme(&self) -> &'static str {
        VALID_THEMES
            .into_iter()
            .find(|id| *id == self.appearance.theme)
            .unwrap_or("dark")
    }

    /// Returns the effective interface font size. Unknown values read as `m`.
    #[must_use]
    pub fn effective_font_size(&self) -> &'static str {
        VALID_FONT_SIZES
            .into_iter()
            .find(|id| *id == self.appearance.font_size)
            .unwrap_or("m")
    }

    /// Validates the complete document.
    ///
    /// Family-specific bounds, grammar, and cross-field rules live in the
    /// owning submodule and are invoked in a fixed order.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for any bound,
    /// grammar, or cross-field violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid =
            |detail: &str| ConfigError::authority_rejection().with_detail(detail.to_owned());
        self.validate_proxy().map_err(invalid)?;
        if let Some(level) = self.reasoning_effort.as_deref()
            && (level == "default" || crate::RoleThinking::parse(level).is_none())
        {
            return Err(invalid(
                "reasoningEffort: must be a models.dev option (off, none, on, minimal, low, medium, high, xhigh, max)",
            ));
        }
        self.validate_magpie()?;
        if !VALID_THEMES.contains(&self.appearance.theme.as_str()) {
            return Err(invalid("appearance.theme: must be dark or light"));
        }
        if !VALID_LANGUAGES.contains(&self.appearance.language.as_str()) {
            return Err(invalid("appearance.language: must be auto, en, or zh"));
        }
        if !VALID_FONT_SIZES.contains(&self.appearance.font_size.as_str()) {
            return Err(invalid("appearance.fontSize: must be s, m, l, or xl"));
        }
        self.validate_subagent_roles()?;
        self.validate_skills()?;
        Ok(())
    }
}

/// Reads and validates `settings.json`.
///
/// A missing document yields the defaults. A valid magpie bootstrap is
/// imported with a canonical envelope. Other documents that cannot be parsed
/// or validated are backed up and replaced, keeping whatever still decodes;
/// see [`read_app_settings_with_repair`].
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when an import or
/// repair cannot be published.
pub fn read_app_settings(home: &crate::HomeLayout) -> Result<AppSettings, ConfigError> {
    Ok(read_app_settings_with_repair(home)?.0)
}

/// Reads `settings.json`, repairing a damaged document.
///
/// The second value is set when the previous bytes were copied to a
/// `settings.json.broken-*` file and the original path was replaced. A valid
/// document does not report a repair. A supported settings object with an
/// active magpie binding and no envelope fields is imported as revision 1
/// through the same owned-file transaction, without a backup or repair report.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or when the backup or the
/// replacement cannot be published.
pub fn read_app_settings_with_repair(
    home: &crate::HomeLayout,
) -> Result<(AppSettings, Option<crate::DocumentRepair>), ConfigError> {
    let mut bootstrap = false;
    let loaded = crate::document_repair::load_or_reset(
        home,
        SETTINGS_PATH,
        MAX_AUTHORITY_DOCUMENT_BYTES,
        |bytes| match decode_settings(bytes) {
            Ok(document) => Ok(document.settings),
            Err(error) => {
                let settings = decode_magpie_bootstrap(bytes).ok_or(error)?;
                bootstrap = true;
                Ok(settings)
            }
        },
        AppSettings::default,
        salvage_app_settings,
        |settings| publish_settings(home, settings),
    )?;
    // Publication failures are I/O/update errors, not damage to the valid
    // bootstrap bytes. Keep them outside load_or_reset's decode callback.
    let settings = if bootstrap {
        import_magpie_bootstrap(home)?
    } else {
        loaded.value
    };
    Ok((settings, loaded.repair))
}

/// The plugin's SetJSON first-create writes binding keys but no envelope.
/// Direct AppSettings decoding rejects every envelope field (including a
/// partial or wrong envelope), unknown key, malformed value, and duplicate
/// field. Its ordinary validators still apply to every supported setting.
fn decode_magpie_bootstrap(bytes: &[u8]) -> Option<AppSettings> {
    let settings: AppSettings = serde_json::from_slice(bytes).ok()?;
    (settings.magpie.is_active() && settings.validate().is_ok()).then_some(settings)
}

fn import_magpie_bootstrap(home: &crate::HomeLayout) -> Result<AppSettings, ConfigError> {
    let mut imported = None;
    locked_update_owned_file(
        home,
        SETTINGS_PATH,
        MAX_AUTHORITY_DOCUMENT_BYTES,
        |current| {
            let bytes =
                current.ok_or_else(|| ConfigError::new(ConfigErrorKind::RevisionConflict))?;
            // Re-read under the existing lock and trust checks. Another
            // reader may already have imported the bootstrap; keep its
            // canonical bytes and revision instead of replacing its value.
            let (settings, replacement) = match decode_settings(bytes) {
                Ok(document) => (document.settings, bytes.to_vec()),
                Err(error) => {
                    let settings = decode_magpie_bootstrap(bytes).ok_or(error)?;
                    let bytes =
                        encode_settings(AuthorityRevision::ABSENT.checked_next()?, &settings)?;
                    (settings, bytes)
                }
            };
            imported = Some(settings);
            Ok(replacement)
        },
    )?;
    imported.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

fn publish_settings(home: &crate::HomeLayout, settings: &AppSettings) -> Result<(), ConfigError> {
    settings.validate()?;
    let bytes = encode_settings(AuthorityRevision::ABSENT.checked_next()?, settings)?;
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
            let bytes = encode_settings(revision, settings)?;
            if bytes.len() > MAX_AUTHORITY_DOCUMENT_BYTES {
                return Err(ConfigError::new(ConfigErrorKind::Oversized));
            }
            published_revision = Some(revision);
            Ok(bytes)
        },
    )?;
    published_revision.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

fn encode_settings(
    revision: AuthorityRevision,
    settings: &AppSettings,
) -> Result<Vec<u8>, ConfigError> {
    let document = SerializedSettings {
        format_version: SETTINGS_FORMAT_VERSION,
        kind: SETTINGS_KIND,
        revision: revision.get(),
        settings,
    };
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
    bytes.push(b'\n');
    Ok(bytes)
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

/// Envelope plus the settings fields captured as a map.
///
/// `deny_unknown_fields` on [`AppSettings`] is silently ignored when the
/// struct is deserialized through `#[serde(flatten)]`, so the envelope keeps
/// the settings keys in a map and [`AppSettings`] decodes from that value,
/// where an unknown key is rejected again instead of being silently dropped.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeserializedSettings {
    format_version: u32,
    kind: String,
    revision: u64,
    #[serde(flatten)]
    fields: serde_json::Map<String, serde_json::Value>,
}

/// A decoded and validated settings document.
struct ParsedSettings {
    settings: AppSettings,
    revision: AuthorityRevision,
}

fn parse_document_header(bytes: &[u8]) -> Result<AuthorityRevision, ConfigError> {
    Ok(decode_settings(bytes)?.revision)
}

fn decode_settings(bytes: &[u8]) -> Result<ParsedSettings, ConfigError> {
    let rejection = || {
        ConfigError::authority_rejection()
            .with_detail("settings.json: unknown field, wrong type, or malformed JSON".to_owned())
    };
    let document =
        serde_json::from_slice::<DeserializedSettings>(bytes).map_err(|_| rejection())?;
    if document.format_version != SETTINGS_FORMAT_VERSION || document.kind != SETTINGS_KIND {
        return Err(ConfigError::authority_rejection()
            .with_detail("settings.json: formatVersion or kind does not match this build"));
    }
    let revision = AuthorityRevision::new(document.revision)?;
    let settings =
        serde_json::from_value::<AppSettings>(serde_json::Value::Object(document.fields))
            .map_err(|_| rejection())?;
    // Canonical documents must validate without per-field upgrades. The
    // reader recognizes magpie bootstrap objects separately.
    settings.validate()?;
    Ok(ParsedSettings { settings, revision })
}

/// Extracts the still-valid sections of a document the strict read rejected,
/// so a reset does not make the user re-enter healthy configuration.
///
/// Every family decodes and validates on its own: one bad member (a removed
/// key, a malformed entry) drops only its own section. `None` when the bytes
/// are not a JSON object or the result still fails validation.
#[must_use]
pub fn salvage_app_settings(document: &serde_json::Value) -> Option<AppSettings> {
    let object = document.as_object()?;
    let valid = |probe: &AppSettings| probe.validate().is_ok();
    let mut settings = AppSettings::default();
    if let Some(proxy) = crate::document_repair::salvage_field::<String>(object, "proxy") {
        crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
            probe.proxy = proxy;
        });
    }
    if let Some(appearance) = object
        .get("appearance")
        .and_then(serde_json::Value::as_object)
    {
        // Field-level salvage: a bad theme drops only the theme, not a
        // healthy language or font size sitting beside it.
        if let Some(theme) = crate::document_repair::salvage_field::<String>(appearance, "theme") {
            crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
                probe.appearance.theme = theme;
            });
        }
        if let Some(language) =
            crate::document_repair::salvage_field::<String>(appearance, "language")
        {
            crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
                probe.appearance.language = language;
            });
        }
        if let Some(font_size) =
            crate::document_repair::salvage_field::<String>(appearance, "fontSize")
        {
            crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
                probe.appearance.font_size = font_size;
            });
        }
    }
    if let Some(reasoning) =
        crate::document_repair::salvage_field::<Option<String>>(object, "reasoningEffort")
    {
        crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
            probe.reasoning_effort = reasoning;
        });
    }
    if let Some(magpie) = crate::document_repair::salvage_field::<MagpieSettings>(object, "magpie")
    {
        crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
            probe.magpie = magpie;
        });
    }
    // Role routes name configured provider ids, so subagents salvage only
    // against the providers already adopted above.
    if let Some(subagents) = object
        .get("subagents")
        .and_then(serde_json::Value::as_object)
    {
        for role in crate::document_repair::salvage_list::<SubagentRoleSettings>(subagents, "roles")
        {
            crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
                probe.subagents.roles.push(role);
            });
        }
    }
    if let Some(skills) = object.get("skills").and_then(serde_json::Value::as_object) {
        for slug in crate::document_repair::salvage_list::<String>(skills, "disabled") {
            if settings.skills.disabled.len() >= MAX_DISABLED_SKILLS {
                break;
            }
            crate::document_repair::salvage_adopt(&mut settings, valid, |probe| {
                probe.skills.disabled.push(slug);
            });
        }
    }
    valid(&settings).then_some(settings)
}

pub(crate) fn bounded_text(value: &str, max: usize) -> Result<(), ConfigError> {
    if value.len() <= max && !value.contains(['\0', '\r', '\n']) {
        Ok(())
    } else {
        Err(ConfigError::authority_rejection())
    }
}

pub(crate) fn is_https_url(value: &str) -> bool {
    if bounded_text(value, MAX_URL_BYTES).is_err() {
        return false;
    }
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    // Graphic ASCII already excludes whitespace, NUL, and other controls.
    !host.is_empty() && host.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether `value` is an `http://` URL on this machine.
///
/// magpie's gateway listens on loopback only, and its agent plugin writes
/// `http://127.0.0.1:<port>/v1`. Nothing else may use plain http.
pub(crate) fn is_local_http_url(value: &str) -> bool {
    if bounded_text(value, MAX_URL_BYTES).is_err() {
        return false;
    }
    let Some(rest) = value.strip_prefix("http://") else {
        return false;
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, port)) = rest.split_once(']') else {
            return false;
        };
        (format!("[{host}]"), port.strip_prefix(':'))
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        (host.to_owned(), Some(port))
    } else {
        (authority.to_owned(), None)
    };
    let host_ok = matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]");
    let port_ok = port.is_none_or(|port| {
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
    });
    host_ok && port_ok
}

/// Whether `value` can name one gateway model (`provider/model`).
pub(crate) fn is_portable_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, decode_settings, encode_settings, salvage_app_settings};
    use crate::authority::AuthorityRevision;

    fn settings_home(label: &str) -> (TempDir, crate::HomeLayout) {
        let parent = std::env::temp_dir().join(format!(
            "mycode-settings-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let root = parent.join("home");
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = crate::HomeLayout::from_root(&root).unwrap();
        (TempDir(parent), home)
    }

    fn bootstrap_fixture() -> serde_json::Value {
        serde_json::json!({
            "magpie": {
                "enabled": true,
                "baseUrl": "http://127.0.0.1:3425/v1",
                "apiKey": "synthetic-test-value",
                "model": "fixture/model-0"
            }
        })
    }

    #[test]
    fn magpie_bootstrap_import_preserves_supported_settings_and_starts_revision_cas() {
        let (_guard, home) = settings_home("bootstrap");
        let mut document = bootstrap_fixture();
        document["appearance"] =
            serde_json::json!({"theme":"light","language":"zh","fontSize":"l"});
        document["reasoningEffort"] = "high".into();
        document["skills"] = serde_json::json!({"disabled":["fixture-skill"]});
        let bytes = serde_json::to_vec(&document).unwrap();
        // The ordinary strict decoder continues to require its envelope.
        assert!(decode_settings(&bytes).is_err());
        crate::write_owned_test_bytes(&home, "settings.json", &bytes).unwrap();
        let (settings, repair) = super::read_app_settings_with_repair(&home).unwrap();
        assert!(repair.is_none(), "a recognized bootstrap is an import");
        assert!(settings.magpie.is_active());
        assert_eq!(settings.magpie.api_key, "synthetic-test-value");
        assert_eq!(settings.magpie.model, "fixture/model-0");
        assert_eq!(settings.appearance.theme, "light");
        assert_eq!(settings.appearance.language, "zh");
        assert_eq!(settings.appearance.font_size, "l");
        assert_eq!(settings.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(settings.skills.disabled, ["fixture-skill"]);
        let canonical = std::fs::read(home.root().join("settings.json")).unwrap();
        let parsed = decode_settings(&canonical).unwrap();
        assert_eq!(parsed.revision.get(), 1);
        assert_eq!(parsed.settings, settings);
        assert_eq!(
            super::read_app_settings_with_repair(&home).unwrap(),
            (settings.clone(), None)
        );
        assert_eq!(
            std::fs::read(home.root().join("settings.json")).unwrap(),
            canonical
        );
        assert!(
            !std::fs::read_dir(home.root())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".broken-"))
        );

        let error =
            super::replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).unwrap_err();
        assert_eq!(error.kind(), crate::ConfigErrorKind::RevisionConflict);
        let next = super::replace_app_settings(&home, parsed.revision, &settings).unwrap();
        assert_eq!(next.get(), 2);
    }

    #[test]
    fn bootstrap_publication_failure_keeps_the_original_without_a_damage_backup() {
        let (_guard, home) = settings_home("bootstrap-publish-error");
        let mut document = bootstrap_fixture();
        document["magpie"]["hiddenModels"] = (0..super::MAX_HIDDEN_MODELS_PER_PROVIDER)
            .map(|index| format!("fixture/{index:04}-{}", "m".repeat(107)))
            .collect::<Vec<_>>()
            .into();
        let bytes = serde_json::to_vec(&document).unwrap();
        let settings: AppSettings = serde_json::from_value(document).unwrap();
        settings.validate().unwrap();
        // Compact input fits the read limit, but canonical indentation puts
        // the replacement over it. Publication must fail without treating
        // the valid source as a damaged document or creating a backup.
        assert!(bytes.len() <= super::MAX_AUTHORITY_DOCUMENT_BYTES);
        assert!(
            encode_settings(AuthorityRevision::new(1).unwrap(), &settings)
                .unwrap()
                .len()
                > super::MAX_AUTHORITY_DOCUMENT_BYTES
        );
        crate::write_owned_test_bytes(&home, "settings.json", &bytes).unwrap();
        let error = super::read_app_settings_with_repair(&home).unwrap_err();
        assert_eq!(error.kind(), crate::ConfigErrorKind::Oversized);
        assert_eq!(
            std::fs::read(home.root().join("settings.json")).unwrap(),
            bytes
        );
        assert!(
            !std::fs::read_dir(home.root())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".broken-"))
        );
    }

    #[test]
    fn bootstrap_compatibility_does_not_hide_partial_invalid_or_unrelated_documents() {
        let mut rejected = vec![
            b"{not-json".to_vec(),
            b"[]".to_vec(),
            b"{}".to_vec(),
            br#"{"appearance":{"theme":"light"}}"#.to_vec(),
        ];
        let envelope = [
            ("formatVersion", serde_json::json!(1)),
            ("kind", serde_json::json!("mycode-app-settings")),
            ("revision", serde_json::json!(7)),
        ];
        // Every proper nonempty subset remains damage, even when each
        // present envelope field has its otherwise-correct value.
        for mask in 1..7 {
            let mut document = bootstrap_fixture();
            for (index, (key, value)) in envelope.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    document[*key] = value.clone();
                }
            }
            rejected.push(serde_json::to_vec(&document).unwrap());
        }
        for (key, value) in [
            ("formatVersion", serde_json::json!(2)),
            ("kind", serde_json::json!("another-format")),
            ("revision", serde_json::json!("7")),
            ("revision", serde_json::json!(u64::MAX)),
            ("formatVersion", serde_json::Value::Null),
            ("kind", serde_json::Value::Null),
            ("revision", serde_json::Value::Null),
        ] {
            let mut document = bootstrap_fixture();
            for (field, original) in &envelope {
                document[*field] = original.clone();
            }
            document[key] = value;
            rejected.push(serde_json::to_vec(&document).unwrap());
        }
        for (key, value) in [
            ("providers", serde_json::json!({})),
            ("web", serde_json::json!({})),
            ("tools", serde_json::json!({"shell":{}})),
            ("appearance", serde_json::json!({"theme":"nope"})),
            (
                "appearance",
                serde_json::json!({"theme":"dark","unknown":true}),
            ),
            ("reasoningEffort", serde_json::json!("default")),
            ("skills", serde_json::json!({"disabled":[""]})),
        ] {
            let mut document = bootstrap_fixture();
            document[key] = value;
            rejected.push(serde_json::to_vec(&document).unwrap());
        }
        for (key, value) in [
            ("enabled", serde_json::json!(false)),
            ("baseUrl", serde_json::json!("http://example.com/v1")),
            ("apiKey", serde_json::json!("")),
            ("model", serde_json::json!("")),
            ("model", serde_json::json!("invalid model")),
            ("unknown", serde_json::json!(true)),
        ] {
            let mut document = bootstrap_fixture();
            document["magpie"][key] = value;
            rejected.push(serde_json::to_vec(&document).unwrap());
        }
        for field in ["enabled", "baseUrl", "apiKey", "model"] {
            let mut document = bootstrap_fixture();
            document["magpie"].as_object_mut().unwrap().remove(field);
            rejected.push(serde_json::to_vec(&document).unwrap());
        }
        for (index, bytes) in rejected.into_iter().enumerate() {
            let (_guard, home) = settings_home("bootstrap-rejected");
            crate::write_owned_test_bytes(&home, "settings.json", &bytes).unwrap();
            let (_, repair) = super::read_app_settings_with_repair(&home).unwrap();
            let repair =
                repair.unwrap_or_else(|| panic!("invalid bootstrap case {index} bypassed repair"));
            assert_eq!(
                std::fs::read(home.root().join(repair.backup)).unwrap(),
                bytes
            );
            assert!(
                super::read_app_settings_with_repair(&home)
                    .unwrap()
                    .1
                    .is_none()
            );
        }
    }

    #[test]
    fn unknown_settings_field_is_rejected_not_ignored() {
        // `deny_unknown_fields` is inert under `#[serde(flatten)]`, so a
        // misspelled field must still be rejected by the map-based decode.
        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 1,
            "appearance": { "theme": "dark", "language": "auto" },
            "appearances": { "theme": "light" }
        }"#;
        let error = decode_settings(bytes)
            .err()
            .expect("unknown field is document damage");
        assert_eq!(error.kind(), crate::ConfigErrorKind::AuthorityValidation);
        assert!(
            error.summary().contains("unknown field"),
            "{}",
            error.summary()
        );

        let nested = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 1,
            "appearance": { "theme": "dark", "language": "auto", "humeur": "x" }
        }"#;
        let error = decode_settings(nested)
            .err()
            .expect("nested unknown field is rejected");
        assert_eq!(error.kind(), crate::ConfigErrorKind::AuthorityValidation);

        // Keys from older documents are equally unknown: there is no
        // per-field compatibility shim anymore.
        for document in [
            &br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"usage":{"enabled":true}}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"tools":{"shell":{"kind":"pwsh","program":"C:\\pwsh.exe"},"osSandbox":true}}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"userAgent":"custom-agent"}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"appearance":{"theme":"dark","palette":"ocean"}}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"appearance":{"theme":"dark","fontFamily":"Comic Sans"}}"#[..],
            &br#"{"formatVersion":1,"kind":"mycode-app-settings","revision":1,"subagents":{"maxConcurrent":2}}"#[..],
        ] {
            let error = decode_settings(document)
                .err()
                .expect("a removed field is document damage");
            assert_eq!(error.kind(), crate::ConfigErrorKind::AuthorityValidation);
        }
    }

    #[test]
    fn salvage_keeps_the_healthy_sections_of_a_damaged_document() {
        let document: serde_json::Value = serde_json::from_slice(
            br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 6,
            "appearance": { "theme": "nope", "language": "zh" },
            "userAgent": "custom-agent",
            "usage": { "enabled": true },
            "web": { "backends": [
                { "id": "exa", "kind": "exa", "endpoint": "https://api.exa.ai", "enabled": true }
            ]},
            "magpie": {
                "enabled": true,
                "baseUrl": "http://127.0.0.1:3425/v1",
                "apiKey": "magpie-mycode",
                "model": "deepseek/deepseek-chat"
            },
            "subagents": {
                "maxConcurrent": 4,
                "roles": [
                    { "role": "scout", "enabled": true, "provider": "magpie", "model": "gpt-4o" },
                    { "role": "artisan", "enabled": true, "provider": "missing", "model": "x" }
                ]
            },
            "skills": {
                "extra": true,
                "disabled": ["review", "", "review"]
            }
        }"#,
        )
        .expect("fixture json");
        let settings = salvage_app_settings(&document).expect("salvage succeeds");
        // The invalid theme fell back to the default; its language survived.
        assert_eq!(settings.appearance.theme, "dark");
        assert_eq!(settings.appearance.language, "zh");
        // A repair keeps magpie's binding and drops the retired web family.
        assert!(settings.magpie.enabled);
        assert_eq!(settings.magpie.model, "deepseek/deepseek-chat");
        // Healthy subagent role routes and disabled skills survive retired
        // sibling keys and bad entries.
        assert_eq!(settings.subagents.roles.len(), 1);
        assert_eq!(settings.subagents.roles[0].role, "scout");
        assert_eq!(settings.skills.disabled, ["review"]);
        // Removed keys are simply not read by the salvage.
        let encoded = serde_json::to_string(&settings).expect("encode");
        for removed in [
            "userAgent",
            "usage",
            "tools",
            "palette",
            "fontFamily",
            "web",
        ] {
            assert!(!encoded.contains(removed), "{encoded}");
        }
    }

    #[test]
    fn salvage_returns_none_for_a_non_object_document() {
        assert!(salvage_app_settings(&serde_json::json!([1, 2])).is_none());
        assert!(salvage_app_settings(&serde_json::json!("text")).is_none());
        assert!(salvage_app_settings(&serde_json::json!({})).is_some());
    }

    #[test]
    fn appearance_without_font_size_defaults_to_medium() {
        let bytes = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 2,
            "appearance": { "theme": "dark", "language": "zh" }
        }"#;
        let parsed = decode_settings(bytes).expect("settings without fontSize");
        assert_eq!(parsed.settings.appearance.font_size, "m");
        assert_eq!(parsed.settings.effective_font_size(), "m");
        assert_eq!(parsed.settings.appearance.language, "zh");
    }

    #[test]
    fn font_size_round_trips_through_the_settings_document() {
        let mut settings = AppSettings::default();
        settings.appearance.font_size = "xl".to_owned();
        assert!(settings.validate().is_ok());
        let revision = AuthorityRevision::new(4).expect("revision");
        let bytes = encode_settings(revision, &settings).expect("encode");
        let parsed = decode_settings(&bytes).expect("decode");
        assert_eq!(parsed.settings.appearance.font_size, "xl");
        assert_eq!(parsed.settings.effective_font_size(), "xl");
        assert!(bytes.windows(10).any(|window| window == br#""fontSize""#));

        settings.appearance.font_size = "huge".to_owned();
        let error = settings.validate().expect_err("unknown size");
        assert!(error.summary().contains("fontSize"), "{}", error.summary());
    }

    #[test]
    fn appearance_round_trips_through_read_and_replace() {
        let parent = std::env::temp_dir().join(format!(
            "mycode-appearance-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
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
        settings.appearance.font_size = "l".to_owned();
        let revision =
            super::replace_app_settings(&home, crate::AuthorityRevision::ABSENT, &settings)
                .expect("publish");

        let loaded = super::read_app_settings(&home).expect("read");
        assert_eq!(loaded.appearance.font_size, "l");
        settings.appearance.theme = "light".to_owned();
        assert!(settings.validate().is_ok());
        assert_eq!(settings.effective_theme(), "light");
        settings.appearance.theme = "sepia".to_owned();
        let error = settings.validate().expect_err("unknown theme");
        assert!(
            error.summary().contains("appearance.theme"),
            "{}",
            error.summary()
        );
        settings.appearance.theme = "dark".to_owned();

        assert_eq!(loaded.effective_font_size(), "l");

        let bytes = std::fs::read(home.root().join("settings.json")).expect("settings.json");
        let text = String::from_utf8(bytes).expect("utf-8");
        assert!(!text.contains("palette"), "{text}");
        assert!(text.contains("\"fontSize\": \"l\""), "{text}");
        assert!(!text.contains("fontFamily"), "{text}");

        settings.appearance.font_size = "m".to_owned();
        super::replace_app_settings(&home, revision, &settings).expect("publish medium");
        let restored = super::read_app_settings(&home).expect("read medium");
        assert_eq!(restored.appearance.font_size, "m");
    }

    #[test]
    fn a_malformed_settings_document_is_reset_and_healthy_fields_survive() {
        let parent = std::env::temp_dir().join(format!(
            "mycode-settings-comma-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
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
        // Valid JSON carrying a removed field: the strict read rejects the
        // file, the bytes move aside, and the repair keeps what still
        // decodes and validates.
        let body = br#"{
            "formatVersion": 1,
            "kind": "mycode-app-settings",
            "revision": 9,
            "appearance": { "theme": "light", "language": "zh" },
            "tools": { "shell": { "kind": "pwsh" } }
        }"#;
        // The fixture goes through the secure transaction: a plain fs::write
        // file inherits the temp directory's DACL and the Windows read path
        // rejects it before the revision check this test exercises.
        crate::write_owned_test_bytes(&home, "settings.json", body).expect("write");
        let (settings, repair) = super::read_app_settings_with_repair(&home).expect("read resets");
        assert_eq!(settings.appearance.theme, "light");
        assert_eq!(settings.appearance.language, "zh");
        let repair = repair.expect("repair reported");
        let backup = std::fs::read(home.root().join(&repair.backup)).expect("backup");
        assert!(
            backup.windows(5).any(|window| window == b"tools"),
            "{backup:?}"
        );
        // The republished document is canonical and parses cleanly.
        assert!(super::read_app_settings(&home).is_ok());
    }

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
