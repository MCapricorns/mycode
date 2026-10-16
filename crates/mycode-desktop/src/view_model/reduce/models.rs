//! Model-picker and reasoning transitions: keeping the selection on a model
//! that exists, clamping the stored thinking pick to the catalog, and the
//! preset form's pre-checked model list.

use crate::i18n::t;
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
    // As in `model_selected`: the stored effort survives the switch. The
    // projection falls back to Default for rungs the new provider does not
    // advertise, and the pick returns when a supporting model comes back.
    remember_active_session_model(state);
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
            "{}{}{}",
            t("this provider is at its ", "该服务商已达到 "),
            mycode_config::MAX_MODELS_PER_PROVIDER,
            t(
                "-model limit \u{2014} remove one in Settings \u{2192} Models before switching to an unlisted model",
                " 个模型的上限 \u{2014} 请先在设置 \u{2192} 模型中移除一个，再切换到未列出的模型",
            ),
        ));
        return;
    }
    if let Some(provider) = state.selected_provider.clone() {
        mycode_config::remember_model(&mut state.recent_models, &provider, &model);
    }
    state.selected_model = Some(model);
    state.model_menu_open = false;
    if !selected_model_supports_reasoning(state) {
        state.reasoning_menu_open = false;
    }
    // The stored effort is deliberately not reset here: a rung the new
    // model does not advertise still projects as Default (see
    // `selected_reasoning_level`), and switching back to a model that
    // supports it restores the pick without asking the user again.
    remember_active_session_model(state);
}

/// The `ActivePresetChanged` transition: a preset form opened or closed.
pub(super) fn active_preset_changed(state: &mut WorkspaceState, preset: Option<String>) {
    state.active_preset = preset.clone();
    state.preset_model_query.clear();
    let listed: Vec<String> = preset
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
    // OpenDesign is added in place, so its form starts from the models
    // already configured; checking everything would replace that list on
    // the next Add. Every other preset adds a new row and pre-checks the
    // whole list, strongest first, so a long catalog does not bury o3 /
    // gpt-5 under the cap.
    let configured: Vec<String> = preset
        .as_deref()
        .filter(|id| *id == mycode_providers::catalog::opendesign::PROVIDER_ID)
        .and_then(|id| {
            state
                .settings
                .as_ref()
                .and_then(|settings| settings.providers.iter().find(|p| p.id == id))
        })
        .map(|provider| {
            listed
                .iter()
                .filter(|model| provider.models.contains(model))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    state.preset_models = if configured.is_empty() {
        listed
    } else {
        configured
    };
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
    let Some(provider_id) = state.selected_provider.as_deref() else {
        return true;
    };
    let base_url = endpoint_base_url(state, provider_id).map(str::to_owned);
    let Some(catalog) = state.catalog.as_ref() else {
        return true;
    };
    match state.selected_model.as_deref() {
        Some(model_id) => catalog
            .model_for_endpoint(provider_id, base_url.as_deref(), model_id)
            .map(|model| model.reasoning)
            .unwrap_or(true),
        None => catalog
            .provider(provider_id)
            .map(|provider| provider.models.iter().any(|model| model.reasoning))
            .unwrap_or(true),
    }
}

fn endpoint_base_url<'a>(state: &'a WorkspaceState, provider_id: &str) -> Option<&'a str> {
    state.settings.as_ref().and_then(|settings| {
        settings
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .map(|provider| provider.base_url.as_str())
    })
}

/// Thinking choices advertised for one catalog model.
///
/// A missing catalog row offers Default only. The stored pick is kept so a
/// custom endpoint does not look unset, but the menu does not invent
/// off/on/low/medium/high/xhigh/max.
#[must_use]
pub(crate) fn reasoning_levels_for(
    state: &WorkspaceState,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Vec<String> {
    let (Some(provider_id), Some(model_id)) = (provider_id, model_id) else {
        return unpublished_reasoning_levels(state);
    };
    let base_url = endpoint_base_url(state, provider_id).map(str::to_owned);
    let Some(catalog) = state.catalog.as_ref() else {
        return unpublished_reasoning_levels(state);
    };
    match catalog.model_for_endpoint(provider_id, base_url.as_deref(), model_id) {
        Some(model) => model.reasoning_levels(),
        None => unpublished_reasoning_levels(state),
    }
}

/// Default, plus the stored effort when models.dev has no list for this row.
fn unpublished_reasoning_levels(state: &WorkspaceState) -> Vec<String> {
    let mut levels = vec!["default".to_owned()];
    if let Some(stored) = state
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.clone())
        && stored != "default"
        && !levels.iter().any(|level| level == &stored)
    {
        levels.push(stored);
    }
    levels
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

/// Stores the picker on the active session so the next session switch
/// restores this chat's model instead of whatever the other chat last used.
pub(super) fn remember_active_session_model(state: &mut WorkspaceState) {
    let Some(session_id) = state
        .active
        .as_ref()
        .map(|active| active.session_id.clone())
    else {
        return;
    };
    remember_session_model(state, &session_id);
}

/// Stores the current picker on `session_id`.
pub(super) fn remember_session_model(state: &mut WorkspaceState, session_id: &str) {
    let (Some(provider), Some(model)) = (
        state.selected_provider.clone(),
        state.selected_model.clone(),
    ) else {
        return;
    };
    let reasoning = state
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.clone());
    mycode_config::upsert_session_model(
        &mut state.session_models,
        session_id,
        &provider,
        &model,
        reasoning,
    );
}

/// Restores `session_id`'s pin onto the picker. Returns false when this
/// session has not chosen a model yet.
pub(super) fn apply_session_model(state: &mut WorkspaceState, session_id: &str) -> bool {
    let Some(pin) = mycode_config::session_model(&state.session_models, session_id).cloned() else {
        return false;
    };
    state.selected_provider = Some(pin.provider);
    state.selected_model = Some(pin.model);
    if let Some(settings) = state.settings.as_mut() {
        settings.reasoning = pin.reasoning;
    }
    true
}

/// Gives a session that has no pin the first enabled model, then freezes
/// that choice on the session. It does not copy the previous chat's pick.
pub(super) fn assign_fresh_session_model(state: &mut WorkspaceState, session_id: &str) {
    state.selected_provider = None;
    state.selected_model = None;
    if let Some(settings) = state.settings.as_mut() {
        settings.reasoning = None;
    }
    ensure_model_selection(state);
    // Provider and model stay per-session picks, but the reasoning effort
    // is a preference: resolve the composer's last pick against the rungs
    // the fresh model advertises, clamping to the strongest fit.
    let levels = selected_reasoning_levels(state);
    let seeded = state
        .default_reasoning
        .as_deref()
        .and_then(|preferred| resolve_remembered_effort(&levels, preferred));
    if let Some(settings) = state.settings.as_mut() {
        settings.reasoning = seeded;
    }
    remember_session_model(state, session_id);
}

/// Rank of one reasoning rung, weakest to strongest. Toggle-only and
/// custom spellings have no rank and only match exactly.
fn effort_rank(token: &str) -> Option<usize> {
    match token {
        "off" | "none" => Some(0),
        "minimal" => Some(1),
        "low" => Some(2),
        "medium" => Some(3),
        "high" => Some(4),
        "xhigh" => Some(5),
        "max" => Some(6),
        _ => None,
    }
}

/// The strongest rung one model advertises: its highest effort, else the
/// toggle's On. `None` when it offers only Default or nothing.
#[must_use]
pub(crate) fn strongest_reasoning_level(levels: &[String]) -> Option<&str> {
    levels
        .iter()
        .filter_map(|level| {
            effort_rank(level)
                .filter(|rank| *rank > 0)
                .map(|rank| (rank, level))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, level)| level.as_str())
        .or_else(|| {
            levels
                .iter()
                .find(|level| level.as_str() == "on")
                .map(String::as_str)
        })
}

/// Resolves a remembered effort against the rungs one model advertises.
///
/// The strongest advertised rung at or below the preference wins, so a
/// remembered `max` lands on `high` for a model without it. Nothing
/// qualifying leaves the effort unset (the provider default).
fn resolve_remembered_effort(levels: &[String], preferred: &str) -> Option<String> {
    if let Some(preferred_rank) = effort_rank(preferred) {
        return levels
            .iter()
            .filter_map(|level| effort_rank(level).map(|rank| (rank, level)))
            .filter(|(rank, _)| *rank <= preferred_rank)
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, level)| level.clone());
    }
    levels.iter().find(|level| level == &preferred).cloned()
}

#[cfg(test)]
mod tests {
    use super::selected_reasoning_levels;
    use crate::view_model::{SettingsState, WorkspaceState};

    #[test]
    fn the_opendesign_page_opens_its_form_on_the_configured_models() {
        use mycode_providers::catalog::opendesign;

        let mut document = mycode_providers::catalog::CatalogDocument::default();
        opendesign::attach(&mut document, None);
        let settings = mycode_config::AppSettings {
            providers: vec![mycode_config::ProviderSettings {
                id: opendesign::PROVIDER_ID.to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: opendesign::BASE_URL.to_owned(),
                models: vec!["glm-5.3-flash".to_owned(), "retired-model".to_owned()],
                enabled: true,
                context_limit: None,
                max_output: None,
                hidden_models: Vec::new(),
            }],
            ..mycode_config::AppSettings::default()
        };
        let mut state = WorkspaceState {
            catalog: Some(std::sync::Arc::new(document)),
            settings: Some(SettingsState::from_settings(&settings, 1, Vec::new())),
            ..WorkspaceState::default()
        };
        crate::view_model::reduce(
            &mut state,
            crate::view_model::DesktopAction::ShowModelsSubview(
                crate::view_model::ModelsSubview::OpenDesign,
            ),
        );
        assert_eq!(
            state.active_preset.as_deref(),
            Some(opendesign::PROVIDER_ID)
        );
        assert_eq!(
            state.preset_models,
            ["glm-5.3-flash"],
            "configured models stay checked; ids the list lacks are not invented"
        );

        // Nothing configured yet: the whole list starts checked.
        state.settings = Some(SettingsState::from_settings(
            &mycode_config::AppSettings::default(),
            1,
            Vec::new(),
        ));
        super::active_preset_changed(&mut state, Some(opendesign::PROVIDER_ID.to_owned()));
        assert_eq!(
            state.preset_models.len(),
            opendesign::curated_models().len()
        );
    }

    /// A catalog provider with one reasoning model advertising only `low`.
    fn low_rung_catalog() -> std::sync::Arc<mycode_providers::catalog::CatalogDocument> {
        use mycode_providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};
        std::sync::Arc::new(CatalogDocument {
            providers: vec![CatalogProvider {
                id: "zai".to_owned(),
                models: vec![CatalogModel {
                    id: "glm".to_owned(),
                    reasoning: true,
                    reasoning_efforts: vec!["low".to_owned()],
                    ..CatalogModel::default()
                }],
                ..CatalogProvider::default()
            }],
        })
    }

    #[test]
    fn a_custom_model_keeps_the_stored_effort() {
        let mut state = WorkspaceState::default();
        let document = mycode_config::AppSettings {
            reasoning_effort: Some("high".to_owned()),
            ..mycode_config::AppSettings::default()
        };
        state.settings = Some(SettingsState::from_settings(&document, 1, Vec::new()));
        state.selected_provider = Some("zhipu".to_owned());
        state.selected_model = Some("glm-4.7".to_owned());
        let settings = state.settings.as_ref().expect("settings");
        assert_eq!(settings.reasoning.as_deref(), Some("high"));
        let levels = selected_reasoning_levels(&state);
        assert!(levels.iter().any(|item| item == "default"));
        assert!(levels.iter().any(|item| item == "high"));
        assert!(!levels.iter().any(|item| item == "xhigh"));
    }

    fn state_with_low_rung_model(default_reasoning: Option<&str>) -> WorkspaceState {
        let document = mycode_config::AppSettings {
            providers: vec![mycode_config::ProviderSettings {
                id: "zai".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://zai.test".to_owned(),
                models: vec!["glm".to_owned()],
                enabled: true,
                context_limit: None,
                max_output: None,
                hidden_models: Vec::new(),
            }],
            ..mycode_config::AppSettings::default()
        };
        WorkspaceState {
            settings: Some(SettingsState::from_settings(&document, 1, Vec::new())),
            catalog: Some(low_rung_catalog()),
            default_reasoning: default_reasoning.map(str::to_owned),
            ..WorkspaceState::default()
        }
    }

    #[test]
    fn fresh_session_seeds_the_remembered_reasoning_default() {
        let mut state = state_with_low_rung_model(Some("low"));
        super::assign_fresh_session_model(&mut state, "ses1-seed");
        assert_eq!(
            state
                .settings
                .as_ref()
                .and_then(|settings| settings.reasoning.as_deref()),
            Some("low"),
            "the remembered default seeds a fresh session"
        );
    }

    #[test]
    fn a_remembered_rung_clamps_to_the_strongest_advertised_fit() {
        use std::sync::Arc;

        use mycode_providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

        let mut state = state_with_low_rung_model(Some("max"));
        super::assign_fresh_session_model(&mut state, "ses1-clamp");
        assert_eq!(
            state
                .settings
                .as_ref()
                .and_then(|settings| settings.reasoning.as_deref()),
            Some("low"),
            "max on a low-only model lands on low, not default"
        );

        let document = mycode_config::AppSettings {
            providers: vec![mycode_config::ProviderSettings {
                id: "zai".to_owned(),
                kind: "openai-completions".to_owned(),
                base_url: "https://zai.test".to_owned(),
                models: vec!["glm".to_owned()],
                enabled: true,
                context_limit: None,
                max_output: None,
                hidden_models: Vec::new(),
            }],
            ..mycode_config::AppSettings::default()
        };
        let mut multi_rung = WorkspaceState {
            settings: Some(SettingsState::from_settings(&document, 1, Vec::new())),
            catalog: Some(Arc::new(CatalogDocument {
                providers: vec![CatalogProvider {
                    id: "zai".to_owned(),
                    models: vec![CatalogModel {
                        id: "glm".to_owned(),
                        reasoning: true,
                        reasoning_efforts: vec![
                            "low".to_owned(),
                            "high".to_owned(),
                            "max".to_owned(),
                        ],
                        ..CatalogModel::default()
                    }],
                    ..CatalogProvider::default()
                }],
            })),
            default_reasoning: Some("xhigh".to_owned()),
            ..WorkspaceState::default()
        };
        super::assign_fresh_session_model(&mut multi_rung, "ses1-multi");
        assert_eq!(
            multi_rung
                .settings
                .as_ref()
                .and_then(|settings| settings.reasoning.as_deref()),
            Some("high"),
            "xhigh on a [low, high, max] model clamps to high, not low"
        );
    }

    #[test]
    fn an_unranked_remembered_rung_never_guesses() {
        let mut state = state_with_low_rung_model(Some("turbo"));
        super::assign_fresh_session_model(&mut state, "ses1-custom");
        assert_eq!(
            state
                .settings
                .as_ref()
                .and_then(|settings| settings.reasoning.as_deref()),
            None,
            "a custom rung only matches by exact spelling"
        );
    }

    #[test]
    fn switching_to_a_model_without_the_stored_rung_keeps_the_pick() {
        // The stored pick stays; only the projection falls back to the
        // strongest advertised rung. Wiping it here is what made the effort
        // "not save" across model switches.
        let document = mycode_config::AppSettings {
            reasoning_effort: Some("low".to_owned()),
            ..mycode_config::AppSettings::default()
        };
        let mut settings = SettingsState::from_settings(&document, 1, Vec::new());
        settings.reasoning = Some("high".to_owned());
        let mut state = WorkspaceState {
            settings: Some(settings),
            catalog: Some(low_rung_catalog()),
            selected_provider: Some("zai".to_owned()),
            selected_model: Some("glm".to_owned()),
            ..WorkspaceState::default()
        };
        assert_eq!(
            crate::view_model::selected_reasoning_level(&state),
            "low",
            "a rung the model does not advertise projects as its strongest rung"
        );
        let settings = state.settings.as_ref().expect("settings");
        assert_eq!(settings.reasoning.as_deref(), Some("high"));
        assert_eq!(settings.saved_reasoning.as_deref(), Some("low"));
        assert!(!settings.dirty);
        assert_eq!(
            settings.to_settings().reasoning_effort.as_deref(),
            Some("low"),
            "a session pick never rewrites the saved document"
        );

        // Back on a model that advertises the rung, the pick resurfaces.
        state.selected_model = Some("glm-4.7".to_owned());
        state.catalog = None;
        assert_eq!(crate::view_model::selected_reasoning_level(&state), "high");
    }

    #[test]
    fn published_efforts_are_the_menu_and_custom_rows_do_not_guess() {
        use std::sync::Arc;

        use mycode_providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

        let mut state = WorkspaceState {
            catalog: Some(Arc::new(CatalogDocument {
                providers: vec![CatalogProvider {
                    id: "zai".to_owned(),
                    models: vec![CatalogModel {
                        id: "glm-5.3-flash".to_owned(),
                        reasoning: true,
                        reasoning_efforts: vec![
                            "low".to_owned(),
                            "high".to_owned(),
                            "max".to_owned(),
                        ],
                        ..CatalogModel::default()
                    }],
                    ..CatalogProvider::default()
                }],
            })),
            selected_provider: Some("zai".to_owned()),
            selected_model: Some("glm-5.3-flash".to_owned()),
            ..WorkspaceState::default()
        };
        let listed = super::selected_reasoning_levels(&state);
        assert_eq!(
            listed,
            vec!["low".to_owned(), "high".to_owned(), "max".to_owned()]
        );
        // Nothing picked yet: the strongest published rung, not Default.
        assert_eq!(crate::view_model::selected_reasoning_level(&state), "max");

        state.selected_provider = Some("my-gateway".to_owned());
        state.selected_model = Some("glm-not-in-catalog".to_owned());
        let custom = super::selected_reasoning_levels(&state);
        assert_eq!(custom, vec!["default".to_owned()]);

        state.selected_model = Some("glm-4.7".to_owned());
        state.catalog = Some(Arc::new(CatalogDocument {
            providers: vec![CatalogProvider {
                id: "zai".to_owned(),
                models: vec![CatalogModel {
                    id: "glm-4.7".to_owned(),
                    reasoning: true,
                    reasoning_toggle: true,
                    output: 131_072,
                    context: 204_800,
                    ..CatalogModel::default()
                }],
                ..CatalogProvider::default()
            }],
        }));
        let borrowed = super::selected_reasoning_levels(&state);
        assert_eq!(borrowed, vec!["off".to_owned(), "on".to_owned()]);
        assert_eq!(crate::view_model::selected_reasoning_level(&state), "on");
        let row = state
            .catalog
            .as_ref()
            .unwrap()
            .model_for_endpoint("my-gateway", Some("https://glm.example/v1"), "glm-4.7")
            .expect("fallback");
        assert_eq!(row.output, 131_072);
        assert_eq!(row.context, 204_800);
    }
}
