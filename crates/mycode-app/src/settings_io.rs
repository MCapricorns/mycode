//! Durable settings and secrets I/O: reads with revision tracking, CAS
//! saves, key storage, and the shell the runtime tools use.

use mycode_config::{
    AppSettings, AuthorityRevision, DocumentRepair, HomeLayout, MAX_AUTHORITY_DOCUMENT_BYTES,
    MAX_SECRETS_BYTES, SECRETS_FORMAT_VERSION, SECRETS_PATH, SETTINGS_FORMAT_VERSION,
    SETTINGS_PATH, read_app_settings_with_repair, read_owned_file,
    read_provider_secrets_with_repair, replace_app_settings, replace_provider_secrets,
};

/// Settings loaded for the UI, including any documents reset on this read.
pub(crate) struct LoadedSettings {
    pub settings: AppSettings,
    pub revision: AuthorityRevision,
    pub provider_keys: Vec<String>,
    pub mcp_keys: Vec<String>,
    pub repairs: Vec<DocumentRepair>,
}

/// Why a stored revision header could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RevisionHeaderError {
    /// The bytes are not the expected JSON header shape.
    Unreadable,
    /// The stored format version differs from the expected one.
    UnknownFormat,
    /// The revision counter is out of range.
    BadRevision,
}

/// Decodes the `{formatVersion, revision}` header shared by the settings and
/// secrets documents.
pub(crate) fn revision_header(
    bytes: &[u8],
    format_version: u32,
) -> Result<AuthorityRevision, RevisionHeaderError> {
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(rename = "formatVersion")]
        format_version: u32,
        revision: u64,
    }
    let header: Header =
        serde_json::from_slice(bytes).map_err(|_| RevisionHeaderError::Unreadable)?;
    if header.format_version != format_version {
        return Err(RevisionHeaderError::UnknownFormat);
    }
    AuthorityRevision::new(header.revision).map_err(|_| RevisionHeaderError::BadRevision)
}

/// Reads the stored revision of one authority document; an absent document
/// is [`AuthorityRevision::ABSENT`].
fn stored_revision(
    home: &HomeLayout,
    path: &str,
    max_bytes: usize,
    format_version: u32,
    label: &str,
) -> Result<AuthorityRevision, String> {
    read_owned_file(home, path, max_bytes)
        .map_err(|error| render_config_error(&error))?
        .map(|bytes| revision_header(bytes.as_slice(), format_version))
        .transpose()
        .map_err(|_| format!("stored {label} failed validation"))
        .map(|revision| revision.unwrap_or(AuthorityRevision::ABSENT))
}

pub(crate) fn load_settings(home: &HomeLayout) -> Result<LoadedSettings, String> {
    let mut repairs = Vec::new();
    let (mut settings, settings_repair) =
        read_app_settings_with_repair(home).map_err(|error| render_config_error(&error))?;
    if let Some(repair) = settings_repair {
        repairs.push(repair);
    }
    let mut revision = stored_revision(
        home,
        SETTINGS_PATH,
        MAX_AUTHORITY_DOCUMENT_BYTES,
        SETTINGS_FORMAT_VERSION,
        "settings",
    )?;
    let filled_shell = fill_detected_shell(&mut settings);
    let filled_agent = settings.user_agent.trim().is_empty();
    if filled_agent {
        settings.user_agent = mycode_config::default_user_agent();
    }
    if (filled_shell || filled_agent)
        && let Ok(next) = replace_app_settings(home, revision, &settings)
    {
        revision = next;
    }
    apply_runtime_shell(&settings);
    let (secrets, secrets_repair) =
        read_provider_secrets_with_repair(home).map_err(|error| render_config_error(&error))?;
    if let Some(repair) = secrets_repair {
        repairs.push(repair);
    }
    let (provider_keys, mcp_keys) = split_key_ids(&secrets);
    Ok(LoadedSettings {
        settings,
        revision,
        provider_keys,
        mcp_keys,
        repairs,
    })
}

pub(crate) fn save_settings(
    home: &HomeLayout,
    expected_revision: AuthorityRevision,
    settings: &AppSettings,
) -> Result<AuthorityRevision, String> {
    let revision = replace_app_settings(home, expected_revision, settings)
        .map_err(|error| render_config_error(&error))?;
    apply_runtime_shell(settings);
    Ok(revision)
}

pub(crate) fn render_config_error(error: &mycode_config::ConfigError) -> String {
    format!("settings error: {}", error.summary())
}

fn fill_detected_shell(settings: &mut AppSettings) -> bool {
    if settings
        .tools
        .shell
        .as_ref()
        .is_some_and(|shell| !shell.program.trim().is_empty())
    {
        return false;
    }
    let Some(detected) = mycode_tools::detect_default_shell() else {
        return false;
    };
    settings.tools.shell = Some(mycode_config::ShellSettings {
        kind: detected.kind.as_str().to_owned(),
        program: detected.program.to_string_lossy().into_owned(),
        source: "auto".to_owned(),
    });
    true
}

fn apply_runtime_shell(settings: &AppSettings) {
    let shell = settings.tools.shell.as_ref().and_then(|configured| {
        let program = configured.program.trim();
        if program.is_empty() {
            return None;
        }
        let kind = mycode_tools::ShellKind::parse(&configured.kind).unwrap_or_else(|| {
            mycode_tools::ShellKind::from_program(std::path::Path::new(program))
        });
        Some(mycode_tools::DetectedShell {
            kind,
            program: std::path::PathBuf::from(program),
        })
    });
    mycode_tools::set_runtime_shell(shell);
}

/// Splits stored secret ids into provider and MCP key markers.
fn split_key_ids(secrets: &mycode_config::ProviderSecrets) -> (Vec<String>, Vec<String>) {
    let mut provider_keys = Vec::new();
    let mut mcp_keys = Vec::new();
    for id in secrets.provider_ids() {
        if let Some(server_id) = id.strip_prefix("mcp-") {
            if !server_id.is_empty() {
                mcp_keys.push(server_id.to_owned());
            }
        } else {
            provider_keys.push(id.to_owned());
        }
    }
    (provider_keys, mcp_keys)
}

/// Stores or clears one provider key under the secret-store CAS, returning
/// the refreshed key-id lists so the UI updates its markers in place.
pub(crate) fn save_provider_key(
    home: &HomeLayout,
    provider_id: &str,
    api_key: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let secrets = read_provider_secrets_with_repair(home)
        .map(|(secrets, _repair)| secrets)
        .map_err(|error| render_config_error(&error))?;
    let expected = stored_revision(
        home,
        SECRETS_PATH,
        MAX_SECRETS_BYTES,
        SECRETS_FORMAT_VERSION,
        "secrets",
    )?;
    let api_key = mycode_config::normalize_api_key(api_key);
    let updated = secrets.with_key(
        provider_id,
        (!api_key.is_empty()).then_some(api_key.as_str()),
    );
    replace_provider_secrets(home, expected, &updated)
        .map_err(|error| render_config_error(&error))?;
    Ok(split_key_ids(&updated))
}
