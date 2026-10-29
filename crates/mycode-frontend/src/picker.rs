//! The two-step model picker's data layer: provider rows, catalog and
//! configured model lists, filtering, recents, and highlight preferences.
//!
//! Rendering stays in each frontend; the decisions about which rows exist
//! and which one Enter activates live here so every surface agrees.
use crate::view_model::{
    WorkspaceState, canonical_model_id, model_display_name, provider_display_name,
};

/// One model the picker can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    /// Model id sent to the provider.
    pub id: String,
    /// Catalog display name, or the id when the catalog has none.
    pub name: String,
}

impl AsRef<str> for ModelChoice {
    fn as_ref(&self) -> &str {
        &self.id
    }
}

/// Who a pick is for. Session picks change the open chat; role picks only
/// change that role's route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPickerTarget {
    /// Composer and the settings default model.
    Session,
    /// One subagent role. The provider step includes a session-model row.
    Role(String),
}

/// A keyboard-selectable row in the provider step, in visual order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderChoice {
    /// Keep the session's own model (roles only).
    Inherit,
    /// A recently used provider+model pin.
    Pin(mycode_app::ModelPin),
    /// A configured provider; picking it opens the model step.
    Provider(String),
}

/// Provider-step rows: the inherit row (roles), recent pins, then every
/// enabled provider.
pub fn provider_choices(vm: &WorkspaceState, target: &ModelPickerTarget) -> Vec<ProviderChoice> {
    let enabled = enabled_providers(vm);
    let recent = recent_pins(vm);
    let mut choices = Vec::new();
    if matches!(target, ModelPickerTarget::Role(_)) {
        choices.push(ProviderChoice::Inherit);
    }
    choices.extend(recent.into_iter().map(ProviderChoice::Pin));
    choices.extend(
        enabled
            .into_iter()
            .map(|(id, _)| ProviderChoice::Provider(id)),
    );
    choices
}

/// Include the non-selectable group headings in a scroll-handle item index.
pub fn provider_scroll_index(
    vm: &WorkspaceState,
    target: &ModelPickerTarget,
    index: usize,
) -> usize {
    let enabled = enabled_providers(vm);
    let recent = recent_pins(vm);
    let inherited = usize::from(matches!(target, ModelPickerTarget::Role(_)));
    index
        + usize::from(!recent.is_empty() && index >= inherited)
        + usize::from(!enabled.is_empty() && index >= inherited + recent.len())
}

/// Catalog models first, then configured ids the catalog does not list.
///
/// Nothing is dropped for length. Selecting an unlisted id still goes
/// through the reducer, which enforces the provider row cap.
pub fn merge_model_choices(
    catalog: &[(String, String)],
    configured: &[String],
) -> Vec<ModelChoice> {
    let mut seen: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(catalog.len() + configured.len());
    let mut choices = Vec::with_capacity(catalog.len() + configured.len());
    for (id, name) in catalog {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let name = if name.is_empty() {
            id.clone()
        } else {
            name.clone()
        };
        choices.push(ModelChoice {
            id: id.clone(),
            name,
        });
    }
    for id in configured {
        if !seen.insert(id.as_str()) {
            continue;
        }
        choices.push(ModelChoice {
            id: id.clone(),
            name: id.clone(),
        });
    }
    choices
}

/// Case-insensitive filter on id and display name. Every word of the query
/// has to match, and separators are ignored, so `gpt5` finds `gpt-5` and
/// `sonnet 4.6` finds `claude-sonnet-4-6`. An empty query keeps every choice.
pub fn filter_model_choices(choices: &[ModelChoice], query: &str) -> Vec<ModelChoice> {
    if query.trim().is_empty() {
        return choices.to_vec();
    }
    choices
        .iter()
        .filter(|choice| query_matches(query, &[&choice.id, &choice.name]))
        .cloned()
        .collect()
}

/// Whether every word of `query` appears in `fields`, ignoring case and the
/// separators model ids and names mix. An empty query matches everything.
pub fn query_matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields
        .iter()
        .map(|field| squash_separators(field))
        .collect::<Vec<_>>()
        .join(" ");
    query
        .split_whitespace()
        .map(squash_separators)
        .filter(|word| !word.is_empty())
        .all(|word| haystack.contains(&word))
}

/// Lowercases `text` and drops the separators model ids and names mix.
fn squash_separators(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '-' | '_' | '.' | '/' | ':' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether the chat picker offers `model` from `provider`: the provider is
/// enabled and settings do not hide the model.
pub fn offers_model(vm: &WorkspaceState, provider: &str, model: &str) -> bool {
    vm.settings.as_ref().is_some_and(|settings| {
        settings.providers.iter().any(|entry| {
            entry.id == provider && entry.enabled && provider_shows_model(vm, entry, model)
        })
    })
}

fn provider_shows_model(
    vm: &WorkspaceState,
    provider: &mycode_app::ProviderSettings,
    model: &str,
) -> bool {
    let catalog_model = vm
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.model(&provider.id, model));
    catalog_model.map_or_else(
        || provider.shows_model(model),
        |model| {
            provider.shows_model(&model.id)
                && model
                    .aliases
                    .iter()
                    .all(|alias| provider.shows_model(alias))
        },
    )
}

/// How many recently used models the picker suggests above the providers.
pub const RECENT_SHOWN: usize = 3;

/// The first [`RECENT_SHOWN`] pins `offered` accepts, newest first. A
/// disabled provider or a hidden model is skipped, not counted.
pub fn visible_recent(
    recent: &[mycode_app::ModelPin],
    offered: impl Fn(&mycode_app::ModelPin) -> bool,
) -> Vec<&mycode_app::ModelPin> {
    recent
        .iter()
        .filter(|pin| offered(pin))
        .take(RECENT_SHOWN)
        .collect()
}

/// The recent models this picker suggests.
pub fn recent_pins(vm: &WorkspaceState) -> Vec<mycode_app::ModelPin> {
    let mut seen = std::collections::HashSet::new();
    let recent: Vec<_> = vm
        .recent_models
        .iter()
        .map(|pin| {
            mycode_app::ModelPin::new(
                &pin.provider,
                canonical_model_id(vm, &pin.provider, &pin.model),
            )
        })
        .filter(|pin| seen.insert((pin.provider.clone(), pin.model.clone())))
        .collect();
    visible_recent(&recent, |pin| offers_model(vm, &pin.provider, &pin.model))
        .into_iter()
        .cloned()
        .collect()
}

/// Row Enter should activate.
///
/// A non-empty query selects the first match. An empty query selects the
/// current model when it is still in the list, otherwise the first row.
pub fn preferred_index<T: AsRef<str>>(ids: &[T], query: &str, selected: Option<&str>) -> usize {
    if ids.is_empty() {
        return 0;
    }
    if !query.trim().is_empty() {
        return 0;
    }
    selected
        .and_then(|id| ids.iter().position(|item| item.as_ref() == id))
        .unwrap_or(0)
}

/// Composer and default-model label: `Source · Model`, including unlisted ids.
pub fn selected_model_label(vm: &WorkspaceState) -> String {
    vm.selected_model
        .as_deref()
        .map(|model| {
            model_display_name(
                vm,
                vm.selected_provider.as_deref().unwrap_or_default(),
                model,
            )
        })
        .unwrap_or_else(|| crate::i18n::t("Select model", "选择模型").to_owned())
}

/// Every model the chat picker shows for one configured provider: the
/// provider's full list without the models settings hide.
pub fn provider_model_choices(vm: &WorkspaceState, provider_id: &str) -> Vec<ModelChoice> {
    let mut choices = all_provider_model_choices(vm, provider_id);
    if let Some(provider) = vm.settings.as_ref().and_then(|settings| {
        settings
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
    }) {
        choices.retain(|choice| provider_shows_model(vm, provider, &choice.id));
    }
    choices
}

/// Catalog and configured models for one provider, hidden ones included.
/// The settings page lists these so a hidden model can be shown again.
pub fn all_provider_model_choices(vm: &WorkspaceState, provider_id: &str) -> Vec<ModelChoice> {
    let catalog = vm
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(provider_id))
        .map(|provider| {
            provider
                .models
                .iter()
                .map(|model| (model.id.clone(), model.name.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let configured = vm
        .settings
        .as_ref()
        .and_then(|settings| {
            settings
                .providers
                .iter()
                .find(|provider| provider.id == provider_id)
        })
        .map(|provider| {
            provider
                .models
                .iter()
                .map(|id| canonical_model_id(vm, provider_id, id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut choices = merge_model_choices(&catalog, &configured);
    for choice in &mut choices {
        choice.name = model_display_name(vm, provider_id, &choice.id);
    }
    choices
}

/// The provider the pick target currently resolves to.
pub fn current_provider(vm: &WorkspaceState, target: &ModelPickerTarget) -> Option<String> {
    match target {
        ModelPickerTarget::Session => vm.selected_provider.clone(),
        ModelPickerTarget::Role(role) => vm
            .settings
            .as_ref()
            .and_then(|settings| settings.subagents.role(role))
            .and_then(|entry| entry.provider.clone()),
    }
}

/// The model a pick on this provider step would replace, when one is set.
pub fn selected_in_provider(
    vm: &WorkspaceState,
    target: &ModelPickerTarget,
    provider_id: &str,
) -> Option<String> {
    let selected = match target {
        ModelPickerTarget::Session => vm
            .selected_provider
            .as_deref()
            .filter(|provider| *provider == provider_id)
            .and_then(|_| vm.selected_model.clone()),
        ModelPickerTarget::Role(role) => vm
            .settings
            .as_ref()
            .and_then(|settings| settings.subagents.role(role))
            .filter(|entry| entry.provider.as_deref() == Some(provider_id))
            .and_then(|entry| entry.model.clone()),
    };
    selected.map(|model| canonical_model_id(vm, provider_id, &model))
}

/// Enabled providers as `(id, display name)` rows.
pub fn enabled_providers(vm: &WorkspaceState) -> Vec<(String, String)> {
    vm.settings
        .as_ref()
        .map(|settings| {
            settings
                .providers
                .iter()
                .filter(|provider| provider.enabled)
                .map(|provider| {
                    let name = provider_display_name(vm, &provider.id);
                    (provider.id.clone(), name)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        ModelChoice, RECENT_SHOWN, filter_model_choices, merge_model_choices, preferred_index,
        selected_model_label, visible_recent,
    };
    use mycode_app::ModelPin;

    #[test]
    fn the_selected_model_label_names_the_source_even_without_a_catalog() {
        let mut state = crate::view_model::WorkspaceState::default();
        assert_eq!(
            selected_model_label(&state),
            crate::i18n::t("Select model", "选择模型")
        );
        state.selected_provider = Some("magpie".to_owned());
        state.selected_model = Some("gpt-5".to_owned());
        let unknown = format!("{} · gpt-5", crate::i18n::t("Source unknown", "来源未提供"));
        assert_eq!(selected_model_label(&state), unknown);
        state.catalog = Some(std::sync::Arc::new(mycode_app::catalog::CatalogDocument {
            providers: vec![mycode_app::catalog::CatalogProvider {
                id: "magpie".to_owned(),
                name: "Magpie".to_owned(),
                models: vec![mycode_app::catalog::CatalogModel {
                    id: "gpt-5".to_owned(),
                    source: "openai".to_owned(),
                    name: "GPT-5".to_owned(),
                    ..mycode_app::catalog::CatalogModel::default()
                }],
                ..mycode_app::catalog::CatalogProvider::default()
            }],
        }));
        assert_eq!(selected_model_label(&state), "openai · GPT-5");
        // A missing gateway identity cannot establish the upstream source.
        state.selected_provider = None;
        assert_eq!(selected_model_label(&state), unknown);
    }

    #[test]
    fn filter_keeps_the_whole_catalog_and_matches_name() {
        let choices: Vec<ModelChoice> = (0..300)
            .map(|index| ModelChoice {
                id: format!("m{index}"),
                name: if index == 42 {
                    "Flagship".to_owned()
                } else {
                    format!("Model {index}")
                },
            })
            .collect();
        assert_eq!(filter_model_choices(&choices, "").len(), 300);
        assert_eq!(filter_model_choices(&choices, "   ").len(), 300);
        let matched = filter_model_choices(&choices, "flag");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "m42");
        assert_eq!(filter_model_choices(&choices, "m299").len(), 1);

        let spelled = vec![
            ModelChoice {
                id: "claude-sonnet-4-6".to_owned(),
                name: "Claude Sonnet 4.6".to_owned(),
            },
            ModelChoice {
                id: "gpt-5.6".to_owned(),
                name: "GPT-5.6".to_owned(),
            },
        ];
        for query in ["sonnet 4.6", "sonnet46", "claude 4-6"] {
            let matched = filter_model_choices(&spelled, query);
            assert_eq!(matched.len(), 1, "{query}");
            assert_eq!(matched[0].id, "claude-sonnet-4-6", "{query}");
        }
        assert_eq!(filter_model_choices(&spelled, "gpt5")[0].id, "gpt-5.6");
        assert!(filter_model_choices(&spelled, "sonnet gpt").is_empty());
    }

    #[test]
    fn merge_appends_configured_ids_without_a_cap() {
        let mut catalog: Vec<(String, String)> = (0..300)
            .map(|index| (format!("m{index}"), format!("Name {index}")))
            .collect();
        catalog.push(("m42".to_owned(), "Duplicate entry".to_owned()));
        let configured = vec![
            "m0".to_owned(),
            "custom-only".to_owned(),
            "custom-only".to_owned(),
        ];
        let merged = merge_model_choices(&catalog, &configured);
        assert_eq!(merged.len(), 301);
        assert_eq!(merged[0].name, "Name 0");
        assert_eq!(
            merged.last().map(|choice| choice.id.as_str()),
            Some("custom-only")
        );
    }

    fn gateway_alias_state() -> crate::view_model::WorkspaceState {
        let provider = mycode_app::catalog::magpie::provider_from_models(
            br#"{"data":[
            {"id":"copilot/claude-opus-5","display_name":"Claude Opus 5","owned_by":"copilot"},
            {"id":"claude-opus-5","display_name":"Claude Opus 5","owned_by":"copilot"},
            {"id":"copilot/gpt-6.1-terra","display_name":"GPT-6.1 Terra","owned_by":"copilot"},
            {"id":"codex/gpt-6.1-terra","display_name":"GPT-6.1 Terra","owned_by":"codex"},
            {"id":"gpt-6.1-terra","display_name":"GPT-6.1 Terra","owned_by":"copilot"}
        ]}"#,
            "http://127.0.0.1:3425/v1",
        )
        .expect("gateway catalog");
        let catalog = mycode_app::catalog::CatalogDocument {
            providers: vec![provider],
        };
        let document = mycode_app::AppSettings {
            magpie: mycode_app::MagpieSettings {
                enabled: true,
                base_url: "http://127.0.0.1:3425/v1".into(),
                api_key: "fixture-key".into(),
                model: "gpt-6.1-terra".into(),
                hidden_models: Vec::new(),
            },
            ..Default::default()
        };
        let mut settings = crate::view_model::SettingsState::from_settings(&document, 1);
        settings.providers = vec![
            mycode_app::magpie_provider_row(&document.magpie, &catalog).expect("active provider"),
        ];
        crate::view_model::WorkspaceState {
            settings: Some(settings),
            catalog: Some(std::sync::Arc::new(catalog)),
            selected_provider: Some("magpie".into()),
            selected_model: Some("gpt-6.1-terra".into()),
            recent_models: vec![
                ModelPin::new("magpie", "gpt-6.1-terra"),
                ModelPin::new("magpie", "copilot/gpt-6.1-terra"),
                ModelPin::new("magpie", "claude-opus-5"),
                ModelPin::new("magpie", "codex/gpt-6.1-terra"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn gateway_aliases_do_not_duplicate_search_recents_or_palette() {
        use crate::view_model::{PaletteCommand, PaletteGroupId, palette_catalog};
        let state = gateway_alias_state();
        let choices = super::provider_model_choices(&state, "magpie");
        assert_eq!(choices.len(), 3);
        let matched = filter_model_choices(&choices, "opus 5");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "copilot/claude-opus-5");
        assert_eq!(matched[0].name, "copilot · Claude Opus 5");
        let selected =
            super::selected_in_provider(&state, &super::ModelPickerTarget::Session, "magpie");
        assert_eq!(selected.as_deref(), Some("copilot/gpt-6.1-terra"));
        assert_eq!(
            choices[preferred_index(&choices, "", selected.as_deref())].id,
            "copilot/gpt-6.1-terra"
        );
        let recent = super::recent_pins(&state);
        assert_eq!(
            recent
                .iter()
                .map(|pin| pin.model.as_str())
                .collect::<Vec<_>>(),
            [
                "copilot/gpt-6.1-terra",
                "copilot/claude-opus-5",
                "codex/gpt-6.1-terra"
            ]
        );
        let palette = palette_catalog(&state);
        let models = &palette
            .groups
            .iter()
            .find(|group| group.id == PaletteGroupId::Models)
            .expect("model group")
            .items;
        assert_eq!(models.len(), 3);
        assert_eq!(models.iter().filter(|item| item.checked).count(), 1);
        assert!(models.iter().any(|item| item.checked && matches!(&item.command,
            PaletteCommand::SelectModel { provider, model } if provider == "magpie" && model == "copilot/gpt-6.1-terra"
        )));
        assert_eq!(
            state.selected_model.as_deref(),
            Some("gpt-6.1-terra"),
            "projection preserves stored selections"
        );
    }

    #[test]
    fn real_same_name_routes_remain_distinct_and_show_the_source() {
        let state = gateway_alias_state();
        let choices = super::provider_model_choices(&state, "magpie");
        let matched = filter_model_choices(&choices, "gpt 6.1 terra");
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].id, "codex/gpt-6.1-terra");
        assert_eq!(matched[1].id, "copilot/gpt-6.1-terra");
        assert_eq!(matched[0].name, "codex · GPT-6.1 Terra");
        assert_eq!(matched[1].name, "copilot · GPT-6.1 Terra");
        assert!(selected_model_label(&state).contains("copilot · GPT-6.1 Terra"));
    }

    #[test]
    fn unlisted_configured_models_show_source_fallbacks_everywhere() {
        use crate::view_model::{PaletteCommand, PaletteGroupId, palette_catalog};
        for (id, label) in [
            (
                "custom-only",
                format!(
                    "{} · custom-only",
                    crate::i18n::t("Source unknown", "来源未提供")
                ),
            ),
            ("source-b/custom-only", "source-b · custom-only".to_owned()),
        ] {
            let mut state = gateway_alias_state();
            let settings = state.settings.as_mut().expect("settings");
            settings.magpie.model = id.into();
            settings.providers = vec![
                mycode_app::magpie_provider_row(
                    &settings.magpie,
                    state.catalog.as_ref().expect("catalog"),
                )
                .expect("provider"),
            ];
            state.selected_model = Some(id.into());
            state.recent_models = vec![ModelPin::new("magpie", id)];
            let choices = super::provider_model_choices(&state, "magpie");
            let matches = filter_model_choices(&choices, "custom-only");
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].name, label);
            assert_eq!(selected_model_label(&state), label);
            let recent = super::recent_pins(&state);
            assert_eq!(
                crate::view_model::model_display_name(
                    &state,
                    &recent[0].provider,
                    &recent[0].model
                ),
                label
            );
            let palette = palette_catalog(&state);
            let models = palette
                .groups
                .iter()
                .find(|group| group.id == PaletteGroupId::Models)
                .expect("models");
            let item = models.items.iter().find(|item| matches!(&item.command, PaletteCommand::SelectModel { model, .. } if model == id)).expect("custom model");
            assert!(item.checked);
            assert_eq!(item.label, format!("{label} · Magpie"));
        }
    }

    #[test]
    fn bare_model_names_show_the_published_source_and_are_searchable_by_it() {
        let provider = mycode_app::catalog::magpie::provider_from_models(
            br#"{"data":[
                {"id":"bare-model","display_name":"Single Model","owned_by":"source-a"},
                {"id":"unknown-model","display_name":"Unknown Model"}
            ]}"#,
            "http://127.0.0.1:3425/v1",
        )
        .expect("catalog");
        let state = crate::view_model::WorkspaceState {
            catalog: Some(std::sync::Arc::new(mycode_app::catalog::CatalogDocument {
                providers: vec![provider],
            })),
            selected_provider: Some("magpie".into()),
            selected_model: Some("bare-model".into()),
            ..Default::default()
        };
        let choices = super::all_provider_model_choices(&state, "magpie");
        let matches = filter_model_choices(&choices, "source-a");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "bare-model");
        assert_eq!(matches[0].name, "source-a · Single Model");
        assert_eq!(selected_model_label(&state), "source-a · Single Model");
        assert_eq!(
            crate::view_model::model_display_name(&state, "magpie", "unknown-model"),
            format!(
                "{} · Unknown Model",
                crate::i18n::t("Source unknown", "来源未提供")
            )
        );
    }

    #[test]
    fn hiding_either_spelling_hides_the_model_without_hiding_another_source() {
        for hidden in ["gpt-6.1-terra", "copilot/gpt-6.1-terra"] {
            let mut state = gateway_alias_state();
            state.settings.as_mut().expect("settings").providers[0].hidden_models =
                vec![hidden.into()];
            let choices = super::provider_model_choices(&state, "magpie");
            assert_eq!(super::all_provider_model_choices(&state, "magpie").len(), 3);
            let matched = filter_model_choices(&choices, "gpt 6.1 terra");
            assert_eq!(matched.len(), 1, "{hidden}");
            assert_eq!(matched[0].id, "codex/gpt-6.1-terra");
            assert!(!super::offers_model(&state, "magpie", "gpt-6.1-terra"));
            assert!(!super::offers_model(
                &state,
                "magpie",
                "copilot/gpt-6.1-terra"
            ));
            assert!(
                super::recent_pins(&state)
                    .iter()
                    .all(|pin| pin.model != "copilot/gpt-6.1-terra")
            );
            let palette = crate::view_model::palette_catalog(&state);
            let models = palette
                .groups
                .iter()
                .find(|group| group.id == crate::view_model::PaletteGroupId::Models)
                .expect("models");
            assert_eq!(models.items.len(), 2);
        }
    }

    #[test]
    fn recent_suggestions_skip_unoffered_pins_and_stop_at_three() {
        let recent = vec![
            ModelPin::new("off", "disabled"),
            ModelPin::new("openai", "gpt"),
            ModelPin::new("openai", "hidden"),
            ModelPin::new("openai", "o3"),
            ModelPin::new("openai", "o4"),
            ModelPin::new("openai", "o5"),
        ];
        let shown = visible_recent(&recent, |pin| {
            pin.provider == "openai" && pin.model != "hidden"
        });
        let models: Vec<_> = shown.iter().map(|pin| pin.model.as_str()).collect();
        assert_eq!(models, ["gpt", "o3", "o4"]);
        assert_eq!(shown.len(), RECENT_SHOWN);
    }

    #[test]
    fn preferred_row_follows_the_query() {
        let ids = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(preferred_index(&ids, "", Some("c")), 2);
        assert_eq!(preferred_index(&ids, "", Some("missing")), 0);
        assert_eq!(preferred_index(&ids, "bee", Some("c")), 0);
        assert_eq!(preferred_index::<String>(&[], "", None), 0);
    }
}
