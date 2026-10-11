//! Product data export/import: one JSON bundle carrying settings, UI state,
//! and provider API keys between machines. Conversation ledgers never travel;
//! they stay local to each machine.

use std::collections::BTreeMap;
use std::path::Path;

use mycode_config::{
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

/// The full bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    /// Version 1 bundles carried session directories. Imports ignore them;
    /// conversations never travel.
    #[serde(default, rename = "sessions", skip_serializing)]
    _sessions: Vec<serde_json::Value>,
    /// Bundles exported before 0.7.0 carried a top-level todo list. Imports
    /// ignore it so those files still load.
    #[serde(default, rename = "todos", skip_serializing)]
    _todos: Vec<serde_json::Value>,
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

fn settings_have_mcp_command(settings: &AppSettings) -> bool {
    settings.mcp_servers.iter().any(|server| {
        server
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty())
    })
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
        exported_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        settings,
        ui_state,
        secrets: bundle_secrets(&secrets),
        _sessions: Vec::new(),
        _todos: Vec::new(),
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
    std::fs::write(path, &body).map_err(|error| format!("write: {error}"))?;
    Ok(ExportSummary {
        secrets,
        bytes: body.len(),
    })
}

/// Applies one bundle: settings, UI state, and provider keys replace local
/// values. Conversation data is never imported.
///
/// Refuses the whole import when local settings contain an MCP command and
/// `confirm_mcp_commands` is false. The desktop import path leaves that flag
/// false; there is no separate confirm dialog.
///
/// # Errors
///
/// Returns a rendered message for a malformed bundle, a refused MCP-command
/// replacement, or a failed write.
pub fn import_from_file(home: &HomeLayout, path: &Path) -> Result<ImportSummary, String> {
    import_from_file_with(home, path, false)
}

/// Same as [`import_from_file`] with an explicit MCP-command confirm flag.
///
/// # Errors
///
/// Returns a rendered message for a malformed bundle, a refused MCP-command
/// replacement, or a failed write.
pub fn import_from_file_with(
    home: &HomeLayout,
    path: &Path,
    confirm_mcp_commands: bool,
) -> Result<ImportSummary, String> {
    let meta = std::fs::metadata(path).map_err(|error| format!("read: {error}"))?;
    if meta.len() > MAX_BUNDLE_BYTES {
        return Err("bundle exceeds the size limit".to_owned());
    }
    let body = std::fs::read(path).map_err(|error| format!("read: {error}"))?;
    let bundle: ExportBundle =
        serde_json::from_slice(&body).map_err(|error| format!("bundle: {error}"))?;
    if !IMPORTABLE_FORMAT_VERSIONS.contains(&bundle.format_version) || bundle.kind != EXPORT_KIND {
        return Err("not a MYCode export bundle".to_owned());
    }
    bundle
        .settings
        .validate()
        .map_err(|error| format!("settings: {error}"))?;
    let current_settings = read_app_settings(home).map_err(|error| format!("settings: {error}"))?;
    if settings_have_mcp_command(&current_settings) && !confirm_mcp_commands {
        return Err(
            "import refused: this settings file contains MCP commands; confirm before replacing them"
                .to_owned(),
        );
    }

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

/// Reads the current revision of one authority document for the import's CAS
/// write. An absent document is [`AuthorityRevision::ABSENT`].
fn stored_authority_revision(
    path: &std::path::Path,
    format_version: u32,
    label: &str,
) -> Result<AuthorityRevision, String> {
    if !path.exists() {
        return Ok(AuthorityRevision::ABSENT);
    }
    let body = std::fs::read(path).map_err(|_| format!("{label}: unreadable"))?;
    // Shared header decoder; distinct messages keep the import's wording.
    match crate::settings_io::revision_header(&body, format_version) {
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
        &home.root().join(mycode_config::SETTINGS_PATH),
        mycode_config::SETTINGS_FORMAT_VERSION,
        "settings",
    )
}

/// Reads the current secrets revision for the import's CAS write.
fn read_current_secrets_revision(home: &HomeLayout) -> Result<AuthorityRevision, String> {
    stored_authority_revision(
        &home.root().join(mycode_config::SECRETS_PATH),
        mycode_config::SECRETS_FORMAT_VERSION,
        "secrets",
    )
}

#[cfg(test)]
mod tests {
    use super::{build_bundle, export_to_file, import_from_file, import_from_file_with};
    use mycode_config::{
        AppSettings, AuthorityRevision, HomeLayout, McpServerSettings, ProviderSecrets,
        replace_app_settings, replace_provider_secrets,
    };
    use std::collections::BTreeMap;

    fn scratch(label: &str) -> (std::path::PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-export-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
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

    fn stdio_server(command: &str) -> McpServerSettings {
        McpServerSettings {
            id: "local".to_owned(),
            enabled: true,
            transport: "stdio".to_owned(),
            command: Some(command.to_owned()),
            args: Vec::new(),
            env: BTreeMap::new(),
            endpoint: None,
            key_header: None,
        }
    }

    #[test]
    fn v2_bundle_carries_secrets_and_never_sessions() {
        let (root, home) = scratch("v2");
        let settings = AppSettings {
            user_agent: "export-agent".to_owned(),
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
            ui_state: mycode_config::UiState::default(),
            secrets: BTreeMap::from([("zai".to_owned(), "imported-key".to_owned())]),
            _sessions: Vec::new(),
            _todos: Vec::new(),
        };
        std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let summary = import_from_file(&home, &bundle_path).unwrap();
        assert_eq!(summary.secrets, 1);
        let stored = mycode_config::read_provider_secrets(&home).unwrap();
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
                "mcpServers": [],
                "appearance": {
                    "theme": "dark",
                    "palette": "ocean",
                    "language": "auto",
                    "fontSize": "m",
                    "fontFamily": "system"
                }
            },
            "uiState": mycode_config::UiState::default(),
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

        let settings = mycode_config::read_app_settings(&home).unwrap();
        assert_eq!(settings.user_agent, "v1-agent");
        assert_eq!(settings.appearance.palette, "ocean");
        // No conversation data is imported, and the vault stays untouched.
        assert!(!root.join("sessions").join("sess").exists());
        assert!(
            mycode_config::read_provider_secrets(&home)
                .unwrap()
                .provider_ids()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_refuses_to_replace_mcp_commands_without_confirm() {
        let (root, home) = scratch("mcp");
        let current = AppSettings {
            mcp_servers: vec![stdio_server("local-tool")],
            ..AppSettings::default()
        };
        replace_app_settings(&home, AuthorityRevision::ABSENT, &current).unwrap();
        let incoming = AppSettings {
            mcp_servers: vec![stdio_server("replaced-tool")],
            ..AppSettings::default()
        };
        let bundle_path = root.join("bundle.json");
        let bundle = super::ExportBundle {
            format_version: super::EXPORT_FORMAT_VERSION,
            kind: super::EXPORT_KIND.to_owned(),
            exported_at_unix: 0,
            settings: incoming,
            ui_state: mycode_config::UiState::default(),
            secrets: BTreeMap::from([("zai".to_owned(), "refused-key".to_owned())]),
            _sessions: Vec::new(),
            _todos: Vec::new(),
        };
        std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let refused = import_from_file(&home, &bundle_path).unwrap_err();
        assert!(refused.contains("confirm before replacing"), "{refused}");
        let kept = mycode_config::read_app_settings(&home).unwrap();
        assert_eq!(kept.mcp_servers[0].command.as_deref(), Some("local-tool"));
        // The refusal covers the whole import: the vault is untouched too.
        assert!(
            mycode_config::read_provider_secrets(&home)
                .unwrap()
                .provider_ids()
                .is_empty()
        );
        import_from_file_with(&home, &bundle_path, true).unwrap();
        let replaced = mycode_config::read_app_settings(&home).unwrap();
        assert_eq!(
            replaced.mcp_servers[0].command.as_deref(),
            Some("replaced-tool")
        );
        assert_eq!(
            mycode_config::read_provider_secrets(&home)
                .unwrap()
                .key("zai"),
            Some("refused-key")
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
            "uiState": mycode_config::UiState::default(),
        });
        let bundle_path = root.join("bundle.json");
        std::fs::write(&bundle_path, serde_json::to_vec(&future).unwrap()).unwrap();
        let refused = import_from_file(&home, &bundle_path).unwrap_err();
        assert!(refused.contains("not a MYCode export bundle"), "{refused}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
