//! Model-picker and reasoning transitions: keeping the selection on a model
//! that exists, clamping the stored thinking pick to the catalog, and the
//! preset form's pre-checked model list.

use crate::view_model::{WorkspaceState, rank_model_ids};

/// The `ProviderSelected` transition: the model picker selected a provider.
pub(super) fn provider_selected(state: &mut WorkspaceState, provider: String) {
    state.selected_provider = Some(provider.clone());
    state.selected_model = state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(&provider))
        .and_then(|provider| provider.models.first())
        .map(|model| model.id.clone())
        .or_else(|| {
            state
                .settings
                .as_ref()
                .and_then(|settings| settings.providers.iter().find(|p| p.id == provider))
                .and_then(|provider| provider.models.first().cloned())
        });
    state.model_menu_open = false;
    if !selected_model_supports_reasoning(state) {
        state.reasoning_menu_open = false;
    }
    clamp_reasoning_to_catalog(state);
}

/// The `ModelSelected` transition: the model picker selected a model; an
/// unknown model joins the provider row so the next turn can use it.
pub(super) fn model_selected(state: &mut WorkspaceState, model: String) {
    // Picking a catalog model the provider row does not carry yet
    // appends it to the row: turn resolution validates against that
    // list, so an unpersisted selection would silently fall back to
    // the first model.
    let provider_id = state.selected_provider.clone();
    let mut capped = false;
    if let Some(settings) = state.settings.as_mut()
        && let Some(provider) = provider_id
            .as_deref()
            .and_then(|id| settings.providers.iter_mut().find(|p| p.id == id))
    {
        let known = provider.models.contains(&model);
        if !known {
            if provider.models.len() < mycode_config::MAX_MODELS_PER_PROVIDER {
                provider.models.push(model.clone());
                super::mark_settings_dirty(settings);
            } else {
                capped = true;
            }
        }
    }
    if capped {
        // The row is at the settings cap: storing the pick would
        // leave a selection the next turn silently drops, so refuse
        // the pick and say why.
        state.error = Some(format!(
            "this provider is at its {}-model limit \u{2014} remove one in \
             Settings \u{2192} Models before switching to an unlisted model",
            mycode_config::MAX_MODELS_PER_PROVIDER
        ));
        return;
    }
    state.selected_model = Some(model);
    state.model_menu_open = false;
    if !selected_model_supports_reasoning(state) {
        state.reasoning_menu_open = false;
    }
    clamp_reasoning_to_catalog(state);
}

/// The `ActivePresetChanged` transition: a preset form opened or closed.
pub(super) fn active_preset_changed(state: &mut WorkspaceState, preset: Option<String>) {
    state.active_preset = preset.clone();
    state.preset_model_menu_open = false;
    // Opening a provider pre-checks its model list, strongest first,
    // so a long catalog does not bury o3 / gpt-5 under the cap.
    state.preset_models = preset
        .as_ref()
        .and_then(|id| {
            state
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.provider(id))
        })
        .map(|provider| {
            rank_model_ids(provider.models.iter().map(|model| model.id.clone()))
                .into_iter()
                .take(mycode_config::MAX_MODELS_PER_PROVIDER)
                .collect()
        })
        .unwrap_or_default();
}

/// Keeps the model picker on a provider/model that actually exists.
pub(super) fn ensure_model_selection(state: &mut WorkspaceState) {
    let Some(settings) = state.settings.as_ref() else {
        return;
    };
    let selected_valid = state
        .selected_provider
        .as_ref()
        .and_then(|provider_id| {
            settings
                .providers
                .iter()
                .find(|provider| &provider.id == provider_id)
        })
        .is_some_and(|provider| {
            provider.enabled
                && state
                    .selected_model
                    .as_ref()
                    .is_some_and(|model| provider.models.contains(model))
        });
    if selected_valid {
        return;
    }
    let fallback = settings
        .providers
        .iter()
        .find(|provider| provider.enabled && !provider.models.is_empty());
    state.selected_provider = fallback.map(|provider| provider.id.clone());
    state.selected_model = fallback.and_then(|provider| provider.models.first().cloned());
}

/// Whether the selected catalog model advertises reasoning.
///
/// Unknown catalog rows keep the control visible so custom providers are
/// not locked out; the menu itself is built from `reasoning_options`.
#[must_use]
pub(crate) fn selected_model_supports_reasoning(state: &WorkspaceState) -> bool {
    let Some(catalog) = state.catalog.as_ref() else {
        return true;
    };
    let Some(provider) = state
        .selected_provider
        .as_deref()
        .and_then(|id| catalog.provider(id))
    else {
        return true;
    };
    match state.selected_model.as_deref() {
        Some(model_id) => provider
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.reasoning)
            .unwrap_or(true),
        None => provider.models.iter().any(|model| model.reasoning),
    }
}

/// Thinking choices advertised for one catalog model.
///
/// Missing catalog rows yield only `default` so the UI never invents
/// low/medium/high.
#[must_use]
pub(crate) fn reasoning_levels_for(
    state: &WorkspaceState,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Vec<String> {
    let Some(catalog) = state.catalog.as_ref() else {
        return vec!["default".to_owned()];
    };
    let (Some(provider_id), Some(model_id)) = (provider_id, model_id) else {
        return vec!["default".to_owned()];
    };
    match catalog.model(provider_id, model_id) {
        Some(model) => model.reasoning_levels(),
        None => vec!["default".to_owned()],
    }
}

/// Thinking choices for the composer chip's selected model.
#[must_use]
pub(crate) fn selected_reasoning_levels(state: &WorkspaceState) -> Vec<String> {
    reasoning_levels_for(
        state,
        state.selected_provider.as_deref(),
        state.selected_model.as_deref(),
    )
}

pub(super) fn clamp_reasoning_to_catalog(state: &mut WorkspaceState) {
    let levels = selected_reasoning_levels(state);
    let Some(settings) = state.settings.as_mut() else {
        return;
    };
    let Some(current) = settings.reasoning.as_deref() else {
        return;
    };
    if !levels.iter().any(|level| level == current) {
        settings.reasoning = None;
        super::mark_settings_dirty(settings);
    }
}
