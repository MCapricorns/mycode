//! Settings-editor transitions: the projection of `settings.json`, the
//! dirty/saving handshake with the store, and MCP probe bookkeeping.

use super::super::{SettingsState, WorkspaceState};

pub(super) fn settings_loaded(state: &mut WorkspaceState, settings: SettingsState) {
    match state.settings.as_mut() {
        Some(current) if current.dirty || current.saving => {
            // Key badges live outside the settings document. Refresh them
            // even while a save is in flight. New provider rows are adopted
            // only when nothing is saving: merging them into the snapshot
            // already on the wire would mark the editor clean while memory
            // and disk disagree.
            current.providers_with_keys = settings.providers_with_keys;
            current.mcp_with_keys = settings.mcp_with_keys;
            if !current.saving {
                current.revision = settings.revision;
                for provider in settings.providers {
                    if current.providers.len() >= mycode_config::MAX_PROVIDERS {
                        break;
                    }
                    if !current
                        .providers
                        .iter()
                        .any(|existing| existing.id == provider.id)
                    {
                        current.providers.push(provider);
                        mark_settings_dirty(current);
                    }
                }
            }
        }
        _ => {
            crate::i18n::apply_language(&settings.language);
            state.settings = Some(settings);
        }
    }
}

pub(super) fn language_selected(state: &mut WorkspaceState, language: String) {
    if !mycode_config::VALID_LANGUAGES.contains(&language.as_str()) {
        return;
    }
    crate::i18n::apply_language(&language);
    if let Some(settings) = state.settings.as_mut() {
        settings.language = language;
        mark_settings_dirty(settings);
    }
}

pub(super) fn theme_selected(state: &mut WorkspaceState, theme: &str) {
    let theme = crate::ui::desk::normalize_theme(theme).to_owned();
    if let Some(settings) = state.settings.as_mut() {
        settings.theme = theme;
        mark_settings_dirty(settings);
    }
}

pub(super) fn font_size_selected(state: &mut WorkspaceState, font_size: String) {
    if !mycode_config::VALID_FONT_SIZES.contains(&font_size.as_str()) {
        return;
    }
    if let Some(settings) = state.settings.as_mut() {
        settings.font_size = font_size;
        mark_settings_dirty(settings);
    }
}

pub(super) fn models_shown(
    state: &mut WorkspaceState,
    provider: &str,
    models: Vec<String>,
    shown: bool,
) {
    edit_settings(state, |settings| {
        settings
            .providers
            .iter_mut()
            .find(|entry| entry.id == provider)
            .is_some_and(|entry| set_models_shown(entry, models, shown))
    });
}

pub(super) fn provider_base_url_changed(state: &mut WorkspaceState, id: &str, base_url: String) {
    edit_settings(state, |settings| {
        settings
            .providers
            .iter_mut()
            .find(|provider| provider.id == id)
            .is_some_and(|provider| {
                if provider.base_url == base_url {
                    false
                } else {
                    provider.base_url = base_url;
                    true
                }
            })
    });
}

pub(super) fn provider_added(
    state: &mut WorkspaceState,
    provider: mycode_config::ProviderSettings,
) {
    edit_settings(state, |settings| {
        if let Some(existing) = settings
            .providers
            .iter_mut()
            .find(|existing| existing.id == provider.id)
        {
            *existing = provider;
            true
        } else if settings.providers.len() < mycode_config::MAX_PROVIDERS {
            settings.providers.push(provider);
            true
        } else {
            false
        }
    });
}

pub(super) fn provider_removed(state: &mut WorkspaceState, index: usize) {
    edit_settings(state, |settings| {
        if index < settings.providers.len() {
            settings.providers.remove(index);
            true
        } else {
            false
        }
    });
}

/// Enabling one web backend disables the others; at most one is active.
pub(super) fn backend_toggled(state: &mut WorkspaceState, index: usize, enabled: bool) {
    edit_settings(state, |settings| {
        if index >= settings.web_backends.len() {
            return false;
        }
        if enabled {
            for (slot, backend) in settings.web_backends.iter_mut().enumerate() {
                backend.enabled = slot == index;
            }
        } else {
            settings.web_backends[index].enabled = false;
        }
        true
    });
}

pub(super) fn settings_saved(state: &mut WorkspaceState, revision: u64, edit_epoch: u64) {
    if let Some(settings) = state.settings.as_mut() {
        settings.revision = revision;
        settings.saving = false;
        if settings.edit_epoch == edit_epoch {
            settings.dirty = false;
        }
        settings.effective_user_agent = settings.to_settings().effective_user_agent();
    }
}

pub(super) fn mcp_added(state: &mut WorkspaceState, server: mycode_config::McpServerSettings) {
    edit_settings(state, |settings| {
        if settings.mcp_servers.len() < mycode_config::MAX_MCP_SERVERS {
            settings.mcp_servers.push(server);
            true
        } else {
            false
        }
    });
}

pub(super) fn mcp_removed(state: &mut WorkspaceState, index: usize) {
    if let Some(settings) = state.settings.as_mut()
        && index < settings.mcp_servers.len()
    {
        let removed = settings.mcp_servers.remove(index);
        mark_settings_dirty(settings);
        // A stale listing for a deleted row must not resurface if a server
        // with the same id is added again later.
        state.mcp_tools.retain(|(id, _)| *id != removed.id);
        state.mcp_probing.retain(|id| *id != removed.id);
    }
}

pub(super) fn mcp_tools_listed(state: &mut WorkspaceState, server_id: String, tools: Vec<String>) {
    state.mcp_probing.retain(|id| *id != server_id);
    if let Some(entry) = state.mcp_tools.iter_mut().find(|(id, _)| *id == server_id) {
        entry.1 = tools;
    } else {
        state.mcp_tools.push((server_id, tools));
    }
}

/// Moves `models` out of or into `provider.hidden_models`. Returns whether
/// the list changed. Hiding stops at the document's entry cap.
pub(super) fn set_models_shown(
    provider: &mut mycode_config::ProviderSettings,
    models: Vec<String>,
    shown: bool,
) -> bool {
    let before = provider.hidden_models.len();
    if shown {
        let models: std::collections::HashSet<String> = models.into_iter().collect();
        provider
            .hidden_models
            .retain(|hidden| !models.contains(hidden));
        return provider.hidden_models.len() != before;
    }
    let mut hidden: std::collections::HashSet<String> =
        provider.hidden_models.iter().cloned().collect();
    for model in models {
        if hidden.len() >= mycode_config::MAX_HIDDEN_MODELS_PER_PROVIDER {
            break;
        }
        if hidden.insert(model.clone()) {
            provider.hidden_models.push(model);
        }
    }
    provider.hidden_models.len() != before
}

/// Applies one edit to the settings projection and marks the document dirty
/// when the edit reports it changed something. Collapses the
/// borrow-guard-mark boilerplate the settings arms share.
pub(super) fn edit_settings(
    state: &mut WorkspaceState,
    edit: impl FnOnce(&mut SettingsState) -> bool,
) {
    if let Some(settings) = state.settings.as_mut()
        && edit(settings)
    {
        mark_settings_dirty(settings);
    }
}

/// Records one local settings edit so an in-flight save cannot clear it.
pub(super) fn mark_settings_dirty(settings: &mut SettingsState) {
    settings.dirty = true;
    settings.edit_epoch = settings.edit_epoch.wrapping_add(1);
}
