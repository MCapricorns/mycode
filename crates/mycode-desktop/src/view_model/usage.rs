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

/// Share of prompt tokens served from cache, as a whole percent.
///
/// Providers report cache reads as a subset of the input count, so the ratio
/// is only meaningful once input tokens exist.
#[must_use]
pub(crate) fn cache_percent(cache: u64, input: u64) -> Option<u64> {
    (input > 0 && cache > 0).then(|| (cache.min(input) * 100) / input)
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
