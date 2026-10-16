//! Model ranking, display names, context-window lookup, and the
//! reasoning-effort projection shared by the composer chip and the reasoning
//! menu.

use super::reduce;
use super::state::WorkspaceState;

/// Catalog display name for one provider, or the id when the catalog has none.
#[must_use]
pub(crate) fn provider_display_name(state: &WorkspaceState, id: &str) -> String {
    state
        .catalog
        .as_ref()
        .map(|catalog| catalog.display_name(id))
        .unwrap_or_else(|| id.to_owned())
}

/// Catalog display name for one model, or the id when the catalog has none.
#[must_use]
pub(crate) fn model_display_name(state: &WorkspaceState, provider: &str, model_id: &str) -> String {
    state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.model(provider, model_id))
        .map(|model| {
            if model.name.is_empty() {
                model.id.clone()
            } else {
                model.name.clone()
            }
        })
        .unwrap_or_else(|| model_id.to_owned())
}

/// Effective context window (in tokens) for the selected provider and model,
/// preferring a configured provider override over the catalog limit.
#[must_use]
pub(crate) fn model_context_window(vm: &WorkspaceState) -> u64 {
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
pub(crate) fn selected_reasoning_level(state: &WorkspaceState) -> String {
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
pub(crate) fn effective_reasoning_level(levels: &[String], stored: Option<&str>) -> String {
    stored
        .filter(|stored| levels.iter().any(|level| level == stored))
        .or_else(|| reduce::strongest_reasoning_level(levels))
        .unwrap_or("default")
        .to_owned()
}

/// Higher scores are stronger general-purpose models such as o3 and gpt-5.
#[must_use]
fn model_strength(id: &str) -> u32 {
    let id = id.to_ascii_lowercase();
    let mut score = 0u32;
    if id.contains("o3-pro") {
        score += 100;
    } else if id.contains("o3") {
        score += 90;
    }
    if id.contains("gpt-6") || id.contains("opus") {
        score += 85;
    }
    if id.contains("gpt-5") {
        score += 70;
    }
    if id.contains("o1") {
        score += 60;
    }
    if id.contains("sonnet") {
        score += 50;
    }
    if id.contains("codex") {
        score += 15;
    }
    if compact_model(&id) {
        score = score.saturating_sub(30);
    }
    score
}

fn compact_model(id: &str) -> bool {
    ["mini", "nano", "flash", "haiku", "spark"]
        .iter()
        .any(|tag| id.contains(tag))
}

/// Stable strongest-first order. Equal scores keep the original order.
#[must_use]
pub(crate) fn rank_model_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut indexed: Vec<(usize, String)> = ids.into_iter().enumerate().collect();
    indexed.sort_by(|left, right| {
        model_strength(&right.1)
            .cmp(&model_strength(&left.1))
            .then(left.0.cmp(&right.0))
    });
    indexed.into_iter().map(|(_, id)| id).collect()
}
