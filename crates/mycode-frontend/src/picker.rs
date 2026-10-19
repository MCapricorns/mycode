//! The two-step model picker's data layer: provider rows, catalog and
//! configured model lists, filtering, recents, and highlight preferences.
//!
//! Rendering stays in each frontend; the decisions about which rows exist
//! and which one Enter activates live here so every surface agrees.
use crate::view_model::{WorkspaceState, model_display_name, provider_display_name};

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
    Pin(mycode_config::ModelPin),
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
        seen.insert(id.as_str());
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
        settings
            .providers
            .iter()
            .any(|entry| entry.id == provider && entry.enabled && entry.shows_model(model))
    })
}

/// How many recently used models the picker suggests above the providers.
pub const RECENT_SHOWN: usize = 3;

/// The first [`RECENT_SHOWN`] pins `offered` accepts, newest first. A
/// disabled provider or a hidden model is skipped, not counted.
pub fn visible_recent(
    recent: &[mycode_config::ModelPin],
    offered: impl Fn(&mycode_config::ModelPin) -> bool,
) -> Vec<&mycode_config::ModelPin> {
    recent
        .iter()
        .filter(|pin| offered(pin))
        .take(RECENT_SHOWN)
        .collect()
}

/// The recent models this picker suggests.
pub fn recent_pins(vm: &WorkspaceState) -> Vec<mycode_config::ModelPin> {
    visible_recent(&vm.recent_models, |pin| {
        offers_model(vm, &pin.provider, &pin.model)
    })
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

/// Composer and default-model label: the model name only.
pub fn selected_model_label(vm: &WorkspaceState) -> String {
    match (
        vm.selected_provider.as_deref(),
        vm.selected_model.as_deref(),
    ) {
        (Some(provider), Some(model)) => model_display_name(vm, provider, model),
        (_, Some(model)) => model.to_owned(),
        _ => crate::i18n::t("Select model", "选择模型").to_owned(),
    }
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
        choices.retain(|choice| provider.shows_model(&choice.id));
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
        .map(|provider| provider.models.clone())
        .unwrap_or_default();
    merge_model_choices(&catalog, &configured)
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
    match target {
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
    }
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
        visible_recent,
    };
    use mycode_config::ModelPin;

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
        let catalog: Vec<(String, String)> = (0..300)
            .map(|index| (format!("m{index}"), format!("Name {index}")))
            .collect();
        let configured = vec!["m0".to_owned(), "custom-only".to_owned()];
        let merged = merge_model_choices(&catalog, &configured);
        assert_eq!(merged.len(), 301);
        assert_eq!(merged[0].name, "Name 0");
        assert_eq!(
            merged.last().map(|choice| choice.id.as_str()),
            Some("custom-only")
        );
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
