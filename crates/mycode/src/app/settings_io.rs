//! Durable settings and secrets I/O: reads with revision tracking and CAS
//! saves. A document that fails strict validation is quarantined and reset
//! by the repair layer rather than edited here.

use crate::config::{
    AppSettings, AuthorityRevision, DocumentRepair, HomeLayout, read_app_settings_snapshot,
    read_provider_secrets_with_repair, replace_app_settings,
};

/// Settings loaded for the UI, including any documents reset on this read.
pub(crate) struct LoadedSettings {
    pub settings: AppSettings,
    pub revision: AuthorityRevision,
    pub mcp_keys: Vec<String>,
    pub repairs: Vec<DocumentRepair>,
}

pub(crate) fn load_settings(home: &HomeLayout) -> Result<LoadedSettings, String> {
    let mut repairs = Vec::new();
    let (settings, revision, settings_repair) =
        read_app_settings_snapshot(home).map_err(|error| render_config_error(&error))?;
    if let Some(repair) = settings_repair {
        repairs.push(repair);
    }
    let (secrets, secrets_repair) =
        read_provider_secrets_with_repair(home).map_err(|error| render_config_error(&error))?;
    if let Some(repair) = secrets_repair {
        repairs.push(repair);
    }
    let mcp_keys = mcp_key_ids(&secrets);
    apply_network(&settings);
    Ok(LoadedSettings {
        settings,
        revision,
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
    apply_network(settings);
    Ok(revision)
}

/// Installs the document's proxy preference for every later request.
pub(crate) fn apply_network(settings: &AppSettings) {
    crate::providers::set_proxy_preference(settings.proxy_preference());
}

pub(crate) fn render_config_error(error: &crate::config::ConfigError) -> String {
    format!("settings error: {}", error.summary())
}

/// MCP server ids that have a stored key marker.
fn mcp_key_ids(secrets: &crate::config::ProviderSecrets) -> Vec<String> {
    secrets
        .provider_ids()
        .into_iter()
        .filter_map(|id| id.strip_prefix("mcp-"))
        .filter(|server_id| !server_id.is_empty())
        .map(str::to_owned)
        .collect()
}
