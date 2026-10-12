//! Token-usage metrics: the context-meter helpers and one turn's timing.

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
    /// Whole-percent cache hit, when there is one to draw.
    pub hit_percent: Option<u64>,
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
    let hit_percent = cache_percent(cached, used);
    let hit = hit_percent
        .map(|share| format!("{hit_word} {share}%"))
        .unwrap_or_default();
    ContextMeterParts {
        ratio,
        cache,
        hit,
        hit_percent,
    }
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

/// Metrics for one turn's token accounting: `live_turn` while the turn runs,
/// promoted to `last_turn` when it commits.
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
    use super::{cache_percent, context_meter_label, context_meter_parts, format_context_meter};

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
        assert_eq!(parts.hit_percent, Some(98));
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
        assert_eq!(cache_percent(11_800, 545), Some(95));
        assert_eq!(cache_percent(100, 400), Some(25));
    }
}
