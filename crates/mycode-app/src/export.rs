//! Product data export/import: one JSON bundle carrying settings, UI state,
//! and provider API keys between machines. Conversation ledgers never travel;
//! they stay local to each machine.

use std::collections::BTreeMap;
use std::path::Path;

use mycode_agent::{
    AppSettings, AuthorityRevision, HomeLayout, ProviderSecrets, UiState, read_app_settings,
    read_provider_secrets, read_ui_state, replace_app_settings, replace_provider_secrets,
    replace_ui_state,
};
use serde::{Deserialize, Serialize};

/// Current bundle schema version.
pub const EXPORT_FORMAT_VERSION: u32 = 2;
/// Bundle kind marker.
pub const EXPORT_KIND: &str = "mycode-export";
/// Largest accepted bundle file.
const MAX_BUNDLE_BYTES: u64 = 48 * 1024 * 1024;
/// Bundle format versions this build imports. Version 1 carried session
/// ledgers and no secrets; its settings and UI state still apply.
const IMPORTABLE_FORMAT_VERSIONS: [u32; 2] = [1, 2];

/// The full bundle (the write shape; reads go through `read_bundle`, which
/// keeps members as JSON until each document's own strict-then-salvage pass
/// accepts them).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportBundle {
    pub format_version: u32,
    pub kind: String,
    pub exported_at_unix: u64,
    /// Strict settings document.
    pub settings: AppSettings,
    /// UI state document.
    pub ui_state: UiState,
    /// Provider API keys from the local vault, as a sorted `id -> key` map.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSummary {
    /// Provider keys written into the bundle.
    pub secrets: usize,
    /// Encoded bundle size in bytes.
    pub bytes: usize,
}

/// What an import applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSummary {
    /// Provider keys written into the local vault. Zero when the bundle
    /// carried no secrets section, in which case the vault was untouched.
    pub secrets: usize,
}

fn bundle_secrets(secrets: &ProviderSecrets) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for id in secrets.provider_ids() {
        if let Some(key) = secrets.key(id) {
            map.insert(id.to_owned(), key.to_owned());
        }
    }
    map
}

/// Collects the product configuration into one bundle.
///
/// # Errors
///
/// Returns a rendered message for unreadable inputs.
pub fn build_bundle(home: &HomeLayout) -> Result<ExportBundle, String> {
    let settings = read_app_settings(home).map_err(|error| format!("settings: {error}"))?;
    let ui_state = read_ui_state(home).map_err(|error| format!("ui state: {error}"))?;
    let secrets = read_provider_secrets(home).map_err(|error| format!("secrets: {error}"))?;
    Ok(ExportBundle {
        format_version: EXPORT_FORMAT_VERSION,
        kind: EXPORT_KIND.to_owned(),
        exported_at_unix: crate::unix_now().as_secs(),
        settings,
        ui_state,
        secrets: bundle_secrets(&secrets),
    })
}

/// Writes one bundle to a user-chosen path.
///
/// The file contains provider API keys as plaintext JSON.
///
/// # Errors
///
/// Returns a rendered message when the bundle cannot be serialized or
/// written.
pub fn export_to_file(home: &HomeLayout, path: &Path) -> Result<ExportSummary, String> {
    let bundle = build_bundle(home)?;
    let secrets = bundle.secrets.len();
    let mut body =
        serde_json::to_vec_pretty(&bundle).map_err(|error| format!("encode: {error}"))?;
    body.push(b'\n');
    write_private(path, &body).map_err(|error| format!("write: {error}"))?;
    Ok(ExportSummary {
        secrets,
        bytes: body.len(),
    })
}

/// Writes the bundle readable by its owner only on Unix: it carries API keys
/// in plaintext, and a default 0644 file is readable by every local account.
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode` only applies to a new file; an older export keeps its bits.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(body)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, body)
    }
}

/// Applies one bundle: settings, UI state, and provider keys replace local
/// values. Conversation data is never imported. MCP servers come from
/// magpie's Library, so a bundle cannot smuggle in a command to run.
///
/// # Errors
///
/// Returns a rendered message for a malformed bundle or a failed write.
pub fn import_from_file(home: &HomeLayout, path: &Path) -> Result<ImportSummary, String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("read: {error}"))?;
    if meta.len() > MAX_BUNDLE_BYTES {
        return Err("bundle exceeds the size limit".to_owned());
    }
    let body = std::fs::read(path).map_err(|error| format!("read: {error}"))?;
    let bundle = read_bundle(&body)?;
    if !IMPORTABLE_FORMAT_VERSIONS.contains(&bundle.format_version) || bundle.kind != EXPORT_KIND {
        return Err("not a MYCode export bundle".to_owned());
    }
    bundle
        .settings
        .validate()
        .map_err(|error| format!("settings: {error}"))?;
    bundle
        .ui_state
        .validate()
        .map_err(|error| format!("ui state: {error}"))?;

    // Secrets go first and through the secret-store CAS: the store validates
    // the whole key map before touching the file, so an invalid map fails
    // before any settings or UI state is replaced. An empty section (a v1
    // bundle) leaves the local vault untouched.
    let mut imported = ProviderSecrets::new();
    for (id, key) in &bundle.secrets {
        imported = imported.with_key(id, Some(key));
    }
    if !bundle.secrets.is_empty() {
        let expected = read_current_secrets_revision(home)?;
        replace_provider_secrets(home, expected, &imported)
            .map_err(|error| format!("secrets: {}", error.summary()))?;
    }

    // Settings replace under CAS against the current revision.
    let current = read_current_revision(home)?;
    replace_app_settings(home, current, &bundle.settings)
        .map_err(|error| format!("settings: {error}"))?;
    replace_ui_state(home, &bundle.ui_state).map_err(|error| format!("ui state: {error}"))?;

    Ok(ImportSummary {
        secrets: bundle.secrets.len(),
    })
}

/// Decodes a bundle. Each member document first tries the strict current
/// decode; a document a newer schema no longer accepts falls back to its
/// salvage copy, so an older export's still-valid fields apply instead of
/// refusing the whole file. Sections a newer build retired (`sessions`,
/// `todos`) never decode at all.
fn read_bundle(body: &[u8]) -> Result<ExportBundle, String> {
    /// Loose bundle shell; the document members stay `Value` here.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Shell {
        format_version: u32,
        kind: String,
        exported_at_unix: u64,
        settings: serde_json::Value,
        ui_state: serde_json::Value,
        #[serde(default)]
        secrets: BTreeMap<String, String>,
    }
    let shell: Shell = serde_json::from_slice(body).map_err(|error| format!("bundle: {error}"))?;
    Ok(ExportBundle {
        format_version: shell.format_version,
        kind: shell.kind,
        exported_at_unix: shell.exported_at_unix,
        settings: document_or_salvage(&shell.settings, "settings", |value| {
            mycode_agent::salvage_app_settings(value)
        })?,
        ui_state: document_or_salvage(&shell.ui_state, "ui state", |value| {
            mycode_agent::salvage_ui_state(value)
        })?,
        secrets: shell.secrets,
    })
}

/// Strictly decodes one bundle member, or falls back to its salvage copy.
fn document_or_salvage<T>(
    value: &serde_json::Value,
    label: &str,
    salvage: impl FnOnce(&serde_json::Value) -> Option<T>,
) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    match serde_json::from_value::<T>(value.clone()) {
        Ok(document) => Ok(document),
        Err(_) => salvage(value).ok_or_else(|| format!("bundle: {label} does not decode")),
    }
}

/// Reads the current revision of one authority document for the import's CAS
/// write. An absent document is [`AuthorityRevision::ABSENT`].
fn stored_authority_revision(
    home: &HomeLayout,
    path: &str,
    max_bytes: usize,
    format_version: u32,
    label: &str,
) -> Result<AuthorityRevision, String> {
    let Some(body) = mycode_agent::read_owned_file(home, path, max_bytes)
        .map_err(|error| format!("{label}: {}", error.summary()))?
    else {
        return Ok(AuthorityRevision::ABSENT);
    };
    // Shared header decoder; distinct messages keep the import's wording.
    match crate::settings_io::revision_header(body.as_slice(), format_version) {
        Ok(revision) => Ok(revision),
        Err(crate::settings_io::RevisionHeaderError::Unreadable) => {
            Err(format!("{label}: unreadable"))
        }
        Err(crate::settings_io::RevisionHeaderError::UnknownFormat) => {
            Err(format!("{label}: unknown format"))
        }
        Err(crate::settings_io::RevisionHeaderError::BadRevision) => {
            Err(format!("{label}: bad revision"))
        }
    }
}

/// Reads the current settings revision for the import's CAS write.
fn read_current_revision(home: &HomeLayout) -> Result<AuthorityRevision, String> {
    stored_authority_revision(
        home,
        mycode_agent::SETTINGS_PATH,
        mycode_agent::MAX_AUTHORITY_DOCUMENT_BYTES,
        mycode_agent::SETTINGS_FORMAT_VERSION,
        "settings",
    )
}

/// Reads the current secrets revision for the import's CAS write.
fn read_current_secrets_revision(home: &HomeLayout) -> Result<AuthorityRevision, String> {
    stored_authority_revision(
        home,
        mycode_agent::SECRETS_PATH,
        mycode_agent::MAX_SECRETS_BYTES,
        mycode_agent::SECRETS_FORMAT_VERSION,
        "secrets",
    )
}

#[cfg(test)]
mod tests {
    use super::{build_bundle, export_to_file, import_from_file};
    use mycode_agent::{
        AppSettings, AuthorityRevision, HomeLayout, ProviderSecrets, replace_app_settings,
        replace_provider_secrets,
    };
    use std::collections::BTreeMap;

    fn scratch(label: &str) -> (std::path::PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-export-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = HomeLayout::from_root(&root).unwrap();
        (root, home)
    }

    #[test]
    fn v2_bundle_carries_secrets_and_never_sessions() {
        let (root, home) = scratch("v2");
        let settings = AppSettings {
            proxy: "direct".to_owned(),
            ..AppSettings::default()
        };
        replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).unwrap();
        replace_provider_secrets(
            &home,
            AuthorityRevision::ABSENT,
            &ProviderSecrets::new()
                .with_key("zai", Some("zai-key"))
                .with_key("mcp-local", Some("mcp-key")),
        )
        .unwrap();
        // A session directory on disk must not travel.
        let session = root.join("sessions").join("sess");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("main.jsonl"), "event-body").unwrap();

        let bundle = build_bundle(&home).unwrap();
        assert_eq!(bundle.format_version, 2);
        assert_eq!(
            bundle.secrets,
            BTreeMap::from([
                ("zai".to_owned(), "zai-key".to_owned()),
                ("mcp-local".to_owned(), "mcp-key".to_owned()),
            ])
        );
        let path = root.join("bundle.json");
        let summary = export_to_file(&home, &path).unwrap();
        assert_eq!(summary.secrets, 2);
        assert!(summary.bytes > 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("zai-key"), "{text}");
        assert!(!text.contains("sessions"), "no session collection: {text}");
        assert!(!text.contains("event-body"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_writes_the_vault_through_the_store() {
        let (root, home) = scratch("vault");
        let bundle_path = root.join("bundle.json");
        let bundle = super::ExportBundle {
            format_version: super::EXPORT_FORMAT_VERSION,
            kind: super::EXPORT_KIND.to_owned(),
            exported_at_unix: 0,
            settings: AppSettings::default(),
            ui_state: mycode_agent::UiState::default(),
            secrets: BTreeMap::from([("zai".to_owned(), "imported-key".to_owned())]),
        };
        std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let summary = import_from_file(&home, &bundle_path).unwrap();
        assert_eq!(summary.secrets, 1);
        let stored = mycode_agent::read_provider_secrets(&home).unwrap();
        assert_eq!(stored.key("zai"), Some("imported-key"));

        // The vault lands as a proper secrets.json document, not a hand
        // written file: it round-trips through the strict reader.
        let secrets_text =
            std::fs::read_to_string(root.join("secrets.json")).expect("secrets.json");
        assert!(
            secrets_text.contains("mycode-provider-secrets"),
            "{secrets_text}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v1_bundle_applies_settings_and_ignores_sessions() {
        let (root, home) = scratch("v1");
        // A version 1 bundle: sessions, todos, no secrets.
        let v1 = serde_json::json!({
            "formatVersion": 1,
            "kind": super::EXPORT_KIND,
            "exportedAtUnix": 7,
            "settings": {
                "userAgent": "v1-agent",
                "providers": [],
                "web": { "backends": [] },
                "usage": { "enabled": false },
                "appearance": {
                    "theme": "dark",
                    "palette": "ocean",
                    "language": "auto",
                    "fontSize": "m",
                    "fontFamily": "system"
                }
            },
            "uiState": mycode_agent::UiState::default(),
            "sessions": [
                {
                    "sessionId": "sess",
                    "files": [["main.jsonl", "line\n"]]
                }
            ],
            "todos": []
        });
        let bundle_path = root.join("bundle.json");
        std::fs::write(&bundle_path, serde_json::to_vec(&v1).unwrap()).unwrap();
        let summary = import_from_file(&home, &bundle_path).unwrap();
        assert_eq!(summary.secrets, 0);

        let settings = mycode_agent::read_app_settings(&home).unwrap();
        // The bundle still names retired fields (`usage`, `palette`,
        // `fontFamily`, `userAgent`); salvage keeps what still decodes.
        assert_eq!(settings.appearance.theme, "dark");
        assert_eq!(settings.appearance.font_size, "m");
        // No conversation data is imported, and the vault stays untouched.
        assert!(!root.join("sessions").join("sess").exists());
        assert!(
            mycode_agent::read_provider_secrets(&home)
                .unwrap()
                .provider_ids()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_format_versions_are_rejected() {
        let (root, home) = scratch("future");
        let future = serde_json::json!({
            "formatVersion": 3,
            "kind": super::EXPORT_KIND,
            "exportedAtUnix": 0,
            "settings": AppSettings::default(),
            "uiState": mycode_agent::UiState::default(),
        });
        let bundle_path = root.join("bundle.json");
        std::fs::write(&bundle_path, serde_json::to_vec(&future).unwrap()).unwrap();
        let refused = import_from_file(&home, &bundle_path).unwrap_err();
        assert!(refused.contains("not a MYCode export bundle"), "{refused}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_ui_state_rejects_the_whole_import_before_mutating_settings_or_secrets() {
        let (root, home) = scratch("bad-ui");
        let mut bad_ui = mycode_agent::UiState::default();
        bad_ui
            .recent_projects
            .push("relative/not-absolute".to_owned());
        let bundle_path = root.join("bundle.json");
        let bundle = super::ExportBundle {
            format_version: super::EXPORT_FORMAT_VERSION,
            kind: super::EXPORT_KIND.to_owned(),
            exported_at_unix: 0,
            settings: AppSettings {
                proxy: "direct".to_owned(),
                ..AppSettings::default()
            },
            ui_state: bad_ui,
            secrets: BTreeMap::from([("zai".to_owned(), "should-not-land-key".to_owned())]),
        };
        std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let refused = import_from_file(&home, &bundle_path).unwrap_err();
        assert!(refused.starts_with("ui state:"), "{refused}");
        assert_ne!(
            mycode_agent::read_app_settings(&home).unwrap().proxy,
            "direct"
        );
        assert!(
            mycode_agent::read_provider_secrets(&home)
                .unwrap()
                .provider_ids()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
