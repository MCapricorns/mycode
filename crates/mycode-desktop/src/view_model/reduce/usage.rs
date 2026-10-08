//! Token accounting: the rebuild that replays durable usage records when a
//! recovered conversation opens.

use std::collections::HashMap;

use crate::view_model::{EntryKind, TurnStats, UsageTotal, WorkspaceState, parse_usage_text};

/// Folds usage rows from a newly loaded page into the running totals.
///
/// Does not reset [`WorkspaceState::last_turn`]: paging backward must not
/// wipe the timing of the turn the user just watched.
pub(super) fn include_usage_entries(
    state: &mut WorkspaceState,
    entries: &[crate::view_model::ConversationEntry],
) {
    for entry in entries {
        if entry.kind != EntryKind::Usage {
            continue;
        }
        let Some((key, input, output, cache)) = parse_usage_text(&entry.text) else {
            continue;
        };
        let cache_value = cache.unwrap_or_default();
        let row = match state
            .usage_totals
            .iter_mut()
            .find(|row| row.key == key || crate::view_model::usage_key_matches(&row.key, &key))
        {
            Some(row) => row,
            None => {
                state.usage_totals.push(UsageTotal {
                    key: key.clone(),
                    ..UsageTotal::default()
                });
                state.usage_totals.last_mut().expect("just pushed")
            }
        };
        row.input = row.input.saturating_add(input);
        row.output = row.output.saturating_add(output);
        row.cache = row.cache.saturating_add(cache_value);
        row.requests = row.requests.saturating_add(1);
    }
}

pub(super) fn rebuild_session_usage(state: &mut WorkspaceState) {
    let Some(entries) = state
        .active
        .as_ref()
        .map(|conversation| &conversation.entries)
    else {
        state.usage_totals.clear();
        state.last_turn = None;
        return;
    };
    let mut totals: Vec<UsageTotal> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut last = None;
    for entry in entries {
        if entry.kind != EntryKind::Usage {
            continue;
        }
        let Some((key, input, output, cache)) = parse_usage_text(&entry.text) else {
            continue;
        };
        // Older events carry a bare model key; newer ones `provider/model`.
        // Fold into whichever row either spelling matches so the panel keeps
        // one row per model instead of a stale orphan beside the live one.
        let row = by_key
            .get(&key)
            .copied()
            .or_else(|| totals.iter().position(|row| usage_row_matches(row, &key)));
        let cache_value = cache.unwrap_or_default();
        if let Some(index) = row {
            let row = &mut totals[index];
            row.input = row.input.saturating_add(input);
            row.output = row.output.saturating_add(output);
            row.cache = row.cache.saturating_add(cache_value);
            row.requests = row.requests.saturating_add(1);
            by_key.insert(key.clone(), index);
        } else {
            by_key.insert(key.clone(), totals.len());
            totals.push(UsageTotal {
                key: key.clone(),
                input,
                output,
                cache: cache_value,
                requests: 1,
            });
        }
        last = Some(TurnStats {
            model: key,
            input,
            output,
            cache,
            elapsed_ms: 0,
        });
    }
    state.usage_totals = totals;
    state.last_turn = last;
    state.context_used = entries
        .iter()
        .rev()
        .find_map(|entry| {
            if entry.kind != EntryKind::Usage {
                return None;
            }
            crate::view_model::parse_context_tokens(&entry.text)
        })
        .unwrap_or(0);
}

/// Whether a usage row's key and `key` name the same model under possibly
/// different providers.
fn usage_row_matches(row: &UsageTotal, key: &str) -> bool {
    crate::view_model::usage_key_matches(&row.key, key)
        || crate::view_model::usage_key_matches(key, &row.key)
}
