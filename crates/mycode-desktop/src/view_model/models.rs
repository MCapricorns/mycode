//! Model ranking and the reasoning-effort projection shared by the composer
//! chip and the reasoning menu.

use super::reduce;
use super::state::WorkspaceState;

/// The effective thinking level: the stored pick while the catalog still
/// advertises it, otherwise `default`. One projection shared by the composer
/// chip and the reasoning menu so the two can never disagree.
#[must_use]
pub(crate) fn selected_reasoning_level(state: &WorkspaceState) -> &str {
    state
        .settings
        .as_ref()
        .and_then(|settings| settings.reasoning.as_deref())
        .filter(|level| {
            reduce::selected_reasoning_levels(state)
                .iter()
                .any(|item| item == level)
        })
        .unwrap_or("default")
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

/// The strongest non-compact models, capped for a suggestion row.
#[must_use]
pub fn suggested_model_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    rank_model_ids(ids)
        .into_iter()
        .filter(|id| model_strength(id) > 0 && !compact_model(&id.to_ascii_lowercase()))
        .take(8)
        .collect()
}
