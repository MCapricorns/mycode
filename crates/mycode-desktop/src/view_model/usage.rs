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

/// Compact token-count spelling: 12.3k / 1.2M.
#[must_use]
pub(crate) fn compact_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// The pieces of a context meter. Any string may be empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextMeterParts {
    /// `12.0k / 1.0M`. Empty when the context window is unknown.
    pub ratio: String,
    /// `11.8k cached`. Empty when the latest prompt reported no cache read.
    pub cache: String,
    /// `98%`. Empty when [`cache_percent`] has no share to report.
    ///
    /// The share is `cache_percent(cached, used)` for this prompt, not the
    /// lifetime token sum.
    pub hit: String,
}

/// Splits the latest prompt into a window ratio, a cache-hit percent, and a
/// cache-read count.
///
/// A window of zero omits the ratio. A cache read of zero omits the cache
/// piece. The composer hides the whole meter when both the prompt and the
/// cache read are zero; the inspector still shows `0 / window`.
#[must_use]
pub(crate) fn context_meter_parts(
    used: u64,
    window: u64,
    cached: u64,
    cached_word: &str,
    hit_word: &str,
) -> ContextMeterParts {
    let ratio = if window > 0 {
        format!("{} / {}", compact_count(used), compact_count(window))
    } else {
        String::new()
    };
    let cache = if cached > 0 {
        format!("{} {cached_word}", compact_count(cached))
    } else {
        String::new()
    };
    let hit = cache_percent(cached, used)
        .map(|share| format!("{hit_word} {share}%"))
        .unwrap_or_default();
    ContextMeterParts { ratio, cache, hit }
}

/// Context-meter text. `cached_word` is the localized "cached" label.
///
/// A window of zero omits the ratio. A cache read of zero omits the cache
/// suffix. Empty when there is neither a ratio nor a cache read.
#[cfg(test)]
#[must_use]
fn format_context_meter(
    used: u64,
    window: u64,
    cached: u64,
    cached_word: &str,
    hit_word: &str,
) -> String {
    let parts = context_meter_parts(used, window, cached, cached_word, hit_word);
    [parts.ratio, parts.hit, parts.cache]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Composer label for the latest prompt. Hidden until the session has a
/// prompt size or a cache read, so an empty chat does not show `0 / 1.0M`.
#[cfg(test)]
#[must_use]
fn context_meter_label(
    used: u64,
    window: u64,
    cached: u64,
    cached_word: &str,
    hit_word: &str,
) -> Option<String> {
    if used == 0 && cached == 0 {
        return None;
    }
    let label = format_context_meter(used, window, cached, cached_word, hit_word);
    (!label.is_empty()).then_some(label)
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
    use super::{
        cache_percent, context_meter_label, context_meter_parts, format_context_meter,
        parse_context_cache, parse_context_tokens, parse_usage_text,
    };

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

    #[test]
    fn context_meter_shows_cache_reads_and_hides_an_empty_session() {
        assert_eq!(
            context_meter_label(0, 1_000_000, 0, "cached", "cache hit"),
            None
        );
        assert_eq!(
            context_meter_label(12_000, 1_000_000, 0, "cached", "cache hit").as_deref(),
            Some("12.0k / 1.0M")
        );
        assert_eq!(
            format_context_meter(12_000, 1_000_000, 11_800, "cached", "cache hit"),
            "12.0k / 1.0M · cache hit 98% · 11.8k cached"
        );
        let parts = context_meter_parts(12_000, 1_000_000, 11_800, "cached", "cache hit");
        assert_eq!(parts.ratio, "12.0k / 1.0M");
        assert_eq!(parts.cache, "11.8k cached");
        assert_eq!(parts.hit, "cache hit 98%");
        let uncached = context_meter_parts(12_000, 1_000_000, 0, "cached", "cache hit");
        assert!(uncached.cache.is_empty());
        assert!(uncached.hit.is_empty());
        assert_eq!(
            context_meter_parts(545, 1_000_000, 11_800, "cached", "cache hit").hit,
            "cache hit 95%"
        );
        assert_eq!(
            context_meter_label(12_000, 1_000_000, 11_800, "缓存", "缓存命中").as_deref(),
            Some("12.0k / 1.0M · 缓存命中 98% · 11.8k 缓存")
        );
        assert_eq!(
            context_meter_label(0, 0, 11_800, "cached", "cache hit").as_deref(),
            Some("cache hit 100% · 11.8k cached")
        );
    }
}
