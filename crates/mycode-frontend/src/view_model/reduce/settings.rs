//! Settings-editor transitions: the projection of `settings.json`, the
//! dirty/saving handshake with the store, and MCP probe bookkeeping.

use super::super::{SettingsState, WorkspaceState};

pub(super) fn settings_loaded(state: &mut WorkspaceState, settings: SettingsState) {
    match state.settings.as_mut() {
        Some(current) if current.dirty || current.saving => {
            // MCP key badges live outside the settings document. Refresh
            // them even while a save is in flight. A newer revision is
            // adopted so a save that lands afterwards does not trip CAS.
            current.mcp_with_keys = settings.mcp_with_keys;
            if !current.saving {
                current.revision = settings.revision;
                // A reload after a failed save also re-syncs the magpie
                // binding: magpie's plugin may have rewritten the document
                // underneath the editor. `providers` is derived from the
                // binding (never user-edited), so it moves with it —
                // without this, a connected gateway shows "no provider"
                // until the next full reload.
                if current.magpie.is_default() {
                    current.magpie = settings.magpie.clone();
                    current.providers = settings.providers.clone();
                }
            }
        }
        _ => {
            crate::i18n::apply_language(&settings.language);
            state.settings = Some(settings);
            // The swap dropped the live effort overlay (a session pin or
            // the composer's pick) along with the stale projection.
            super::models::restore_live_reasoning(state);
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
    let theme = crate::normalize_theme(theme).to_owned();
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

pub(super) fn settings_saved(state: &mut WorkspaceState, revision: u64, edit_epoch: u64) {
    if let Some(settings) = state.settings.as_mut() {
        settings.revision = revision;
        settings.saving = false;
        if settings.edit_epoch == edit_epoch {
            settings.dirty = false;
        }
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
