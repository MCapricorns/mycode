//! Model ranking, display names, context-window lookup, and the
//! reasoning-effort projection shared by the composer chip and the reasoning
//! menu.

use super::reduce;
use super::state::WorkspaceState;

/// Catalog display name for one provider, or the id when the catalog has none.
#[must_use]
pub fn provider_display_name(state: &WorkspaceState, id: &str) -> String {
    state
        .catalog
        .as_ref()
        .map(|catalog| catalog.display_name(id))
        .unwrap_or_else(|| id.to_owned())
}

/// Canonical catalog id for a model, preserving unlisted custom ids.
#[must_use]
pub fn canonical_model_id(state: &WorkspaceState, provider: &str, model_id: &str) -> String {
    state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.model(provider, model_id))
        .map(|model| model.id.clone())
        .unwrap_or_else(|| model_id.to_owned())
}

/// Model label with its upstream source, retaining distinct real routes.
#[must_use]
pub fn model_display_name(state: &WorkspaceState, provider: &str, model_id: &str) -> String {
    let Some(provider) = state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.provider(provider))
    else {
        return unlisted_model_label(model_id);
    };
    let Some(model) = provider.model(model_id) else {
        return unlisted_model_label(model_id);
    };
    let name = if model.name.is_empty() {
        &model.id
    } else {
        &model.name
    };
    let source = model_source(&model.source, &model.id);
    let ambiguous = provider.models.iter().any(|other| {
        other.id != model.id
            && (if other.name.is_empty() {
                &other.id
            } else {
                &other.name
            }) == name
            && model_source(&other.source, &other.id) == source
    });
    if ambiguous {
        format!("{source} · {name} ({})", model.id)
    } else {
        format!("{source} · {name}")
    }
}

fn model_source<'a>(published: &'a str, model_id: &'a str) -> &'a str {
    if !published.is_empty() {
        published
    } else {
        model_id
            .split_once('/')
            .filter(|(source, _)| !source.is_empty())
            .map_or(
                crate::i18n::t("Source unknown", "来源未提供"),
                |(source, _)| source,
            )
    }
}

fn unlisted_model_label(model_id: &str) -> String {
    let source = model_source("", model_id);
    let name = model_id
        .split_once('/')
        .filter(|(prefix, _)| *prefix == source)
        .map_or(model_id, |(_, name)| name);
    format!("{source} · {name}")
}

/// Effective context window (in tokens) for the selected provider and model,
/// preferring a configured provider override over the catalog limit.
#[must_use]
pub fn model_context_window(vm: &WorkspaceState) -> u64 {
    let shown = vm.selected_model.as_deref();
    let provider_id = vm.selected_provider.as_deref();
    let provider_row = provider_id.and_then(|id| {
        vm.settings
            .as_ref()
            .and_then(|settings| settings.providers.iter().find(|provider| provider.id == id))
    });
    if let Some(limit) = provider_row
        .and_then(|provider| provider.context_limit)
        .filter(|context| *context > 0)
    {
        return limit;
    }
    let base_url = provider_row.map(|provider| provider.base_url.as_str());
    vm.catalog
        .as_ref()
        .and_then(|catalog| {
            let provider_id = provider_id?;
            if let Some(model_id) = shown {
                return catalog
                    .model_for_endpoint(provider_id, base_url, model_id)
                    .map(|model| model.context)
                    .filter(|context| *context > 0);
            }
            catalog
                .provider(provider_id)?
                .models
                .first()
                .map(|model| model.context)
                .filter(|context| *context > 0)
        })
        .unwrap_or(0)
}

/// The effective thinking level: the stored pick while the catalog still
/// advertises it, otherwise the strongest advertised rung, otherwise
/// `default` (a model that publishes no rungs). One projection shared by the
/// composer chip, the reasoning menu, and the turn request so they can never
/// disagree.
#[must_use]
pub fn selected_reasoning_level(state: &WorkspaceState) -> String {
    let levels = reduce::selected_reasoning_levels(state);
    effective_reasoning_level(
        &levels,
        state
            .settings
            .as_ref()
            .and_then(|settings| settings.reasoning.as_deref()),
    )
}

/// The stored pick when `levels` advertises it, else the strongest rung,
/// else `default`.
#[must_use]
pub fn effective_reasoning_level(levels: &[String], stored: Option<&str>) -> String {
    stored
        .filter(|stored| levels.iter().any(|level| level == stored))
        .or_else(|| reduce::strongest_reasoning_level(levels))
        .unwrap_or("default")
        .to_owned()
}
