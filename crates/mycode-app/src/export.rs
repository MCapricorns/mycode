//! Product data export/import: one JSON bundle carrying settings, UI state,
//! and session ledgers between machines. Secrets never travel — the vault
//! stays local and keys must be re-entered after an import.

use std::path::{Path, PathBuf};

use mycode_config::{
    AppSettings, AuthorityRevision, HomeLayout, UiState, read_app_settings, read_ui_state,
    replace_app_settings, replace_ui_state,
};
use serde::{Deserialize, Serialize};

/// Current bundle schema version.
pub const EXPORT_FORMAT_VERSION: u32 = 1;
/// Bundle kind marker.
pub const EXPORT_KIND: &str = "mycode-export";
/// Sessions carried by one bundle.
pub const MAX_EXPORT_SESSIONS: usize = 64;
/// Files per exported session directory.
const MAX_SESSION_FILES: usize = 16;
/// Bytes per exported session.
const MAX_SESSION_BYTES: usize = 512 * 1024;
/// Largest accepted bundle file.
const MAX_BUNDLE_BYTES: u64 = 48 * 1024 * 1024;

/// One session directory's text files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportedSession {
    /// Session id (directory name).
    pub session_id: String,
    /// File name to file body pairs.
    pub files: Vec<(String, String)>,
}

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
    /// Session ledger directories.
    pub sessions: Vec<ExportedSession>,
    /// Bundles exported before 0.7.0 carried a top-level todo list. Imports
    /// ignore it so those files still load.
    #[serde(default, rename = "todos", skip_serializing)]
    _todos: Vec<serde_json::Value>,
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSummary {
    pub sessions: usize,
    pub bytes: usize,
}

/// What an import applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSummary {
    pub sessions: usize,
}

fn sessions_root(home: &HomeLayout) -> Result<PathBuf, String> {
    home.owned_join(mycode_config::SESSIONS_DIR)
        .map_err(|_| "sessions directory unavailable".to_owned())
}

/// A safe bundle file name: plain, non-empty, no separators.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains(['/', '\\', ':', '\0'])
        && name != "."
        && name != ".."
        && !name.chars().any(char::is_control)
}

/// Session file names are top-level, or `payloads/<id>.bin`.
fn session_file_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("payloads/") {
        return rest.ends_with(".bin")
            && !rest.contains('/')
            && !rest.contains('\\')
            && plain_name(rest);
    }
    plain_name(name)
}

fn push_session_file(
    files: &mut Vec<(String, String)>,
    budget: &mut usize,
    path: &Path,
    name: String,
) {
    if files.len() >= MAX_SESSION_FILES || *budget == 0 || !session_file_name(&name) {
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if !meta.is_file() || meta.len() > *budget as u64 {
        return;
    }
    if let Ok(body) = std::fs::read_to_string(path) {
        *budget -= body.len();
        files.push((name, body));
    }
}

fn collect_session_files(dir: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut budget = MAX_SESSION_BYTES;
    let mut listed = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        listed = entries.flatten().collect();
        listed.sort_by_key(|entry| entry.file_name());
    }
    // JSONL logs win the file cap: they are the event source of truth.
    for entry in &listed {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.ends_with(".jsonl") {
            continue;
        }
        push_session_file(&mut files, &mut budget, &entry.path(), name);
    }
    let payloads = dir.join("payloads");
    if let Ok(entries) = std::fs::read_dir(&payloads) {
        let mut bins: Vec<_> = entries.flatten().collect();
        bins.sort_by_key(|entry| entry.file_name());
        for entry in bins {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.ends_with(".bin") {
                continue;
            }
            push_session_file(
                &mut files,
                &mut budget,
                &entry.path(),
                format!("payloads/{name}"),
            );
        }
    }
    for entry in &listed {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.ends_with(".jsonl") {
            continue;
        }
        push_session_file(&mut files, &mut budget, &entry.path(), name);
    }
    files
}

fn settings_have_mcp_command(settings: &AppSettings) -> bool {
    settings.mcp_servers.iter().any(|server| {
        server
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty())
    })
}

/// Collects the product data into one bundle.
///
/// # Errors
///
/// Returns a rendered message for unreadable or oversized inputs.
pub fn build_bundle(home: &HomeLayout) -> Result<ExportBundle, String> {
    let settings = read_app_settings(home).map_err(|error| format!("settings: {error}"))?;
    let ui_state = read_ui_state(home).map_err(|error| format!("ui state: {error}"))?;
    let mut sessions = Vec::new();
    let root = sessions_root(home)?;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if sessions.len() >= MAX_EXPORT_SESSIONS {
                break;
            }
            let Some(session_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !plain_name(&session_id) {
                continue;
            }
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let files = collect_session_files(&dir);
            if !files.is_empty() {
                sessions.push(ExportedSession { session_id, files });
            }
        }
    }
    Ok(ExportBundle {
        format_version: EXPORT_FORMAT_VERSION,
        kind: EXPORT_KIND.to_owned(),
        exported_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        settings,
        ui_state,
        sessions,
        _todos: Vec::new(),
    })
}

/// Writes one bundle to a user-chosen path.
///
/// # Errors
///
/// Returns a rendered message when the bundle cannot be serialized or
/// written.
pub fn export_to_file(home: &HomeLayout, path: &Path) -> Result<ExportSummary, String> {
    let bundle = build_bundle(home)?;
    let summary = ExportSummary {
        sessions: bundle.sessions.len(),
        bytes: 0,
    };
    let mut body =
        serde_json::to_vec_pretty(&bundle).map_err(|error| format!("encode: {error}"))?;
    body.push(b'\n');
    std::fs::write(path, &body).map_err(|error| format!("write: {error}"))?;
    Ok(ExportSummary {
        bytes: body.len(),
        ..summary
    })
}

/// Applies one bundle: settings and UI state replace local values; sessions
/// only fill gaps, never overwrite.
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
    if bundle.format_version != EXPORT_FORMAT_VERSION || bundle.kind != EXPORT_KIND {
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

    // Settings replace under CAS against the current revision.
    let current = read_current_revision(home)?;
    replace_app_settings(home, current, &bundle.settings)
        .map_err(|error| format!("settings: {error}"))?;
    replace_ui_state(home, &bundle.ui_state).map_err(|error| format!("ui state: {error}"))?;

    let mut sessions_applied = 0usize;
    let root = sessions_root(home)?;
    for session in bundle.sessions.iter().take(MAX_EXPORT_SESSIONS) {
        if !plain_name(&session.session_id) {
            continue;
        }
        let dir = root.join(&session.session_id);
        if dir.exists() || session.files.is_empty() {
            continue;
        }
        let mut written = false;
        if std::fs::create_dir_all(&dir).is_ok() {
            written = true;
            for (name, body) in session.files.iter().take(MAX_SESSION_FILES) {
                if !session_file_name(name)
                    || body.len() > MAX_SESSION_BYTES
                    || write_session_file(&dir, name, body).is_err()
                {
                    written = false;
                    break;
                }
            }
        }
        if written {
            match mycode_agent::session::index_imported_session(home, &session.session_id) {
                Ok(_) => sessions_applied += 1,
                Err(_) => {
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
        } else {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    Ok(ImportSummary {
        sessions: sessions_applied,
    })
}

fn write_session_file(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    if let Some(rest) = name.strip_prefix("payloads/") {
        if !rest.ends_with(".bin") || rest.contains("..") || !plain_name(rest) {
            return Err(format!("unsafe session file name: {name}"));
        }
        let payloads = dir.join("payloads");
        std::fs::create_dir_all(&payloads).map_err(|error| error.to_string())?;
        return std::fs::write(payloads.join(rest), body).map_err(|error| error.to_string());
    }
    if !plain_name(name) {
        return Err(format!("unsafe session file name: {name}"));
    }
    std::fs::write(dir.join(name), body).map_err(|error| error.to_string())
}

/// Reads the current settings revision for the import's CAS write.
fn read_current_revision(home: &HomeLayout) -> Result<AuthorityRevision, String> {
    let path = home.root().join(mycode_config::SETTINGS_PATH);
    if !path.exists() {
        return Ok(AuthorityRevision::ABSENT);
    }
    let body = std::fs::read(&path).map_err(|_| "settings: unreadable".to_owned())?;
    // Shared header decoder; distinct messages keep the import's wording.
    match crate::settings_io::revision_header(&body, mycode_config::SETTINGS_FORMAT_VERSION) {
        Ok(revision) => Ok(revision),
        Err(crate::settings_io::RevisionHeaderError::Unreadable) => {
            Err("settings: unreadable".to_owned())
        }
        Err(crate::settings_io::RevisionHeaderError::UnknownFormat) => {
            Err("settings: unknown format".to_owned())
        }
        Err(crate::settings_io::RevisionHeaderError::BadRevision) => {
            Err("settings: bad revision".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_bundle, import_from_file, import_from_file_with};
    use mycode_config::{
        AppSettings, AuthorityRevision, HomeLayout, McpServerSettings, replace_app_settings,
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
    fn export_includes_jsonl_ahead_of_the_file_cap() {
        let (root, home) = scratch("events");
        let session = root.join("sessions").join("sess");
        std::fs::create_dir_all(&session).unwrap();
        for index in 0..16 {
            std::fs::write(session.join(format!("file-{index}.txt")), "x").unwrap();
        }
        std::fs::write(session.join("main.jsonl"), "event-body").unwrap();
        let bundle = build_bundle(&home).unwrap();
        let files = &bundle.sessions[0].files;
        assert!(
            files
                .iter()
                .any(|(name, body)| { name == "main.jsonl" && body == "event-body" }),
            "branch log missing from {files:?}"
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
            sessions: Vec::new(),
            _todos: Vec::new(),
        };
        std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let refused = import_from_file(&home, &bundle_path).unwrap_err();
        assert!(refused.contains("confirm before replacing"), "{refused}");
        let kept = mycode_config::read_app_settings(&home).unwrap();
        assert_eq!(kept.mcp_servers[0].command.as_deref(), Some("local-tool"));
        import_from_file_with(&home, &bundle_path, true).unwrap();
        let replaced = mycode_config::read_app_settings(&home).unwrap();
        assert_eq!(
            replaced.mcp_servers[0].command.as_deref(),
            Some("replaced-tool")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_without_mcp_commands_still_applies() {
        let (root, home) = scratch("plain");
        let bundle_path = root.join("bundle.json");
        let bundle = super::ExportBundle {
            format_version: super::EXPORT_FORMAT_VERSION,
            kind: super::EXPORT_KIND.to_owned(),
            exported_at_unix: 0,
            settings: AppSettings::default(),
            ui_state: mycode_config::UiState::default(),
            sessions: vec![super::ExportedSession {
                session_id: "sess".to_owned(),
                files: vec![("foo.jsonl".to_owned(), "line\n".to_owned())],
            }],
            _todos: Vec::new(),
        };
        std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        let summary = import_from_file(&home, &bundle_path).unwrap();
        assert_eq!(summary.sessions, 1);
        let log = root.join("sessions").join("sess").join("foo.jsonl");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "line\n");
        let _ = std::fs::remove_dir_all(&root);
    }
}
