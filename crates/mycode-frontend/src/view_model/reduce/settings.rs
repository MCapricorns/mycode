//! Settings-editor transitions: the projection of `settings.json`, the
//! dirty/saving handshake with the store, and MCP probe bookkeeping.

use super::super::{SettingsState, WorkspaceState};

pub(super) fn settings_loaded(state: &mut WorkspaceState, mut settings: SettingsState) {
    let empty_catalog = mycode_app::catalog::CatalogDocument::default();
    settings.providers = mycode_app::magpie_provider_row(
        &settings.magpie,
        state.catalog.as_deref().unwrap_or(&empty_catalog),
    )
    .into_iter()
    .collect();
    // A session's model is its own choice. An unpinned composer that still
    // follows the previous gateway default follows magpie's new default.
    let pinned = state.active.as_ref().is_some_and(|active| {
        mycode_app::session_model(&state.session_models, &active.session_id).is_some()
    });
    if !pinned
        && let Some(current) = state.settings.as_ref()
        && current.magpie.model != settings.magpie.model
        && state.selected_provider.as_deref() == Some(mycode_app::MAGPIE_PROVIDER_ID)
        && state.selected_model.as_deref() == Some(current.magpie.model.as_str())
    {
        state.selected_model = Some(settings.magpie.model.clone());
    }
    match state.settings.as_mut() {
        Some(current) if current.dirty || current.saving => {
            // The binding and model rows are managed by magpie, never local
            // editor fields. Refresh them even while saving, preserving the
            // draft, edit epoch, live effort, and save handshake.
            current.magpie = settings.magpie;
            current.providers = settings.providers;
            current.mcp_with_keys = settings.mcp_with_keys;
            if !current.saving {
                current.revision = settings.revision;
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
    if !mycode_app::VALID_LANGUAGES.contains(&language.as_str()) {
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
    if !mycode_app::VALID_FONT_SIZES.contains(&font_size.as_str()) {
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
