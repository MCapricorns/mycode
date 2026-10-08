//! Token-usage metrics: per-model totals, the projected usage-line parser,
//! and one completed turn's timing.

/// Cumulative token usage for one `provider/model` pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UsageTotal {
    /// `provider/model` spelling this row accounts for.
    pub key: String,
    /// Input (prompt) tokens summed across turns.
    pub input: u64,
    /// Output (completion) tokens summed across turns.
    pub output: u64,
    /// Prompt tokens served from the provider cache, summed across turns.
    pub cache: u64,
    /// Completed turns folded into this row.
    pub requests: u64,
}

/// Share of the prompt served from cache, as a whole percent.
///
/// OpenAI-style usage counts cache reads inside `input`. Anthropic's billed
/// input excludes them, so a cache read larger than `input` is measured
/// against `input + cache`.
#[must_use]
pub(crate) fn cache_percent(cache: u64, input: u64) -> Option<u64> {
    if cache == 0 {
        return None;
    }
    let base = if cache > input {
        input.saturating_add(cache)
    } else {
        input
    };
    (base > 0).then_some((cache.min(base) * 100) / base)
}

/// Whether a `provider/model` usage key belongs to `model`.
#[must_use]
pub(crate) fn usage_key_matches(key: &str, model: &str) -> bool {
    key == model || key.rsplit_once('/').is_some_and(|(_, id)| id == model)
}

/// Parses a projected usage line: `key: N in / M out [· cache K] …`, where
/// `key` is `provider/model` for newer events and a bare model id for older
/// ones. Trailing display suffixes (tok/s, % cached) are ignored.
#[must_use]
pub(crate) fn parse_usage_text(text: &str) -> Option<(String, u64, u64, Option<u64>)> {
    let (key, rest) = text.split_once(':')?;
    let rest = rest.trim();
    let (input, rest) = rest.split_once(" in / ")?;
    let output = rest.split_whitespace().next()?;
    let cache = rest
        .split('\u{b7}')
        .find_map(|part| part.trim().strip_prefix("cache "))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok());
    Some((
        key.trim().to_owned(),
        input.trim().parse().ok()?,
        output.trim().parse().ok()?,
        cache,
    ))
}

/// Latest prompt size stored on a usage line (`· ctx N`). Older lines that
/// only carry the billed sum have no context figure.
#[must_use]
pub(crate) fn parse_context_tokens(text: &str) -> Option<u64> {
    text.split('\u{b7}')
        .find_map(|part| part.trim().strip_prefix("ctx "))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .filter(|tokens| *tokens > 0)
}

/// Cache-read tokens on the latest prompt (`· hit N`). Absent on older lines.
#[must_use]
pub(crate) fn parse_context_cache(text: &str) -> Option<u64> {
    text.split('\u{b7}')
        .find_map(|part| part.trim().strip_prefix("hit "))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// Metrics for one completed model turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnStats {
    /// Model id the turn ran on.
    pub model: String,
    /// Input (prompt) tokens reported by the provider.
    pub input: u64,
    /// Output (completion) tokens reported by the provider.
    pub output: u64,
    /// Prompt tokens served from the provider cache, when reported.
    pub cache: Option<u64>,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{cache_percent, parse_context_cache, parse_context_tokens, parse_usage_text};

    #[test]
    fn usage_line_round_trips_context_and_cache_read() {
        let text = "zai/glm-5.3: 100 in / 20 out · ctx 12000 · hit 11800 · cache 11800 · 40 tok/s · 95% cached";
        let (key, input, output, cache) = parse_usage_text(text).unwrap();
        assert_eq!(key, "zai/glm-5.3");
        assert_eq!(input, 100);
        assert_eq!(output, 20);
        assert_eq!(cache, Some(11800));
        assert_eq!(parse_context_tokens(text), Some(12000));
        assert_eq!(parse_context_cache(text), Some(11800));
        assert_eq!(cache_percent(11800, 545), Some(95));
        assert_eq!(cache_percent(100, 400), Some(25));
    }
}
