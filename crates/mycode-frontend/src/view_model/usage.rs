//! Token-usage metrics: the context-meter helpers and one turn's timing.

/// Compact token-count spelling: 12.3k / 1.2M.
#[must_use]
pub fn compact_count(count: u64) -> String {
    if count >= 999_950 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// The pieces of a context meter. Any string may be empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextMeterParts {
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
    /// Whole-percent of the context window still free, when the window is
    /// known. Frontends draw this as the depleting meter; `hit_percent`
    /// stays the cache-read share for tooltips and labels.
    pub remaining_percent: Option<u64>,
}

/// Splits the latest prompt into a window ratio, a cache-hit percent, and a
/// cache-read count.
///
/// A window of zero omits the ratio. A cache read of zero omits the cache
/// piece. The composer hides the whole meter when both the prompt and the
/// cache read are zero; the inspector still shows `0 / window`.
#[must_use]
pub fn context_meter_parts(
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
        remaining_percent: context_remaining_percent(used, window),
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
pub fn cache_percent(cache: u64, input: u64) -> Option<u64> {
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

/// Share of the context window still free, as a whole percent.
///
/// `None` when the window is unknown (`0`), so callers can skip the meter
/// instead of drawing a fake full one. `used` past the window saturates at
/// `0`: the meter reads empty, never negative.
#[must_use]
pub fn context_remaining_percent(used: u64, window: u64) -> Option<u64> {
    if window == 0 {
        return None;
    }
    Some(100_u64.saturating_sub(used.saturating_mul(100) / window))
}

/// Lowest hit percent treated as a warm prompt cache.
const WARM_CACHE_HIT: u64 = 50;
/// Point drop required, along with a halving, before a dip is a collapse.
const CACHE_COLLAPSE_DROP: u64 = 30;

/// Whole-percent cache hit for one turn's summed usage.
///
/// `None` when the provider reported no cache read. A zero read is stored
/// as `None` on [`TurnStats::cache`], same as an omitted field, so a model
/// that never reports cache does not show a fake `0%`.
#[must_use]
pub fn turn_cache_hit(turn: &TurnStats) -> Option<u64> {
    cache_percent(turn.cache.unwrap_or(0), turn.input)
}

/// Why a prompt prefix was rewritten. `Both` is two rewrites before the
/// next usage snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheBreak {
    /// Compaction replaced the head of the prompt.
    Compaction,
    /// A steer was inserted into the running turn.
    Steer,
    /// Compaction and a steer both landed before the next prompt.
    Both,
}

impl CacheBreak {
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Compaction, Self::Compaction) => Self::Compaction,
            (Self::Steer, Self::Steer) => Self::Steer,
            _ => Self::Both,
        }
    }
}

/// Armed after compaction or a steer until the next prompt reports a size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheWatch {
    pub cause: CacheBreak,
    /// Hit percent of the prompt before the rewrite. `None` is cold.
    pub before: Option<u64>,
}

/// Shown when the prompt after a rewrite misses a previously warm cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheAlert {
    pub cause: CacheBreak,
    /// Hit percent before the rewrite.
    pub before: u64,
    /// Hit percent of the prompt after the rewrite. `0` is a total miss.
    pub after: u64,
}

/// True when a warm cache hit fell far enough to call it a collapse.
///
/// The earlier reading must be at least [`WARM_CACHE_HIT`]. The later one
/// must be at most half of that and at least [`CACHE_COLLAPSE_DROP`] points
/// lower. A missing later reading counts as 0%. A cold-to-colder change is
/// not a collapse.
#[must_use]
pub fn cache_hit_collapsed(before: Option<u64>, after: Option<u64>) -> bool {
    let Some(before) = before
        .map(|hit| hit.min(100))
        .filter(|hit| *hit >= WARM_CACHE_HIT)
    else {
        return false;
    };
    let after = after.unwrap_or(0).min(100);
    after.saturating_mul(2) <= before && before.saturating_sub(after) >= CACHE_COLLAPSE_DROP
}

/// The warning can leave once a later prompt is warm again.
#[must_use]
pub fn cache_hit_recovered(after: Option<u64>) -> bool {
    after.is_some_and(|hit| hit >= WARM_CACHE_HIT)
}

/// Sentence for the Details warning and the toast. Both languages stay
/// next to each other; the numbers fill `{before}` and `{after}`.
#[must_use]
pub fn cache_alert_text(alert: &CacheAlert) -> String {
    let template = match alert.cause {
        CacheBreak::Compaction => crate::i18n::t(
            "Cache hit fell from {before}% to {after}% after compaction.",
            "压缩后缓存命中从 {before}% 降到 {after}%。",
        ),
        CacheBreak::Steer => crate::i18n::t(
            "Cache hit fell from {before}% to {after}% after a steer.",
            "追加引导后缓存命中从 {before}% 降到 {after}%。",
        ),
        CacheBreak::Both => crate::i18n::t(
            "Cache hit fell from {before}% to {after}% after compaction and a steer.",
            "压缩和追加引导后缓存命中从 {before}% 降到 {after}%。",
        ),
    };
    template
        .replace("{before}", &alert.before.to_string())
        .replace("{after}", &alert.after.to_string())
}

/// Metrics for one turn's token accounting: `live_turn` while the turn runs,
/// promoted to `last_turn` when it commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnStats {
    /// Model id the turn ran on.
    pub model: String,
    /// Input (prompt) tokens reported by the provider.
    pub input: u64,
    /// Output (completion) tokens reported by the provider.
    pub output: u64,
    /// Prompt tokens served from the provider cache, when reported.
    pub cache: Option<u64>,
    /// Model requests the sums cover: one per tool round.
    pub requests: u32,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{
        CacheAlert, CacheBreak, TurnStats, cache_alert_text, cache_hit_collapsed,
        cache_hit_recovered, cache_percent, context_meter_label, context_meter_parts,
        context_remaining_percent, format_context_meter, turn_cache_hit,
    };

    #[test]
    fn compact_count_rolls_over_to_millions_before_four_digit_thousands() {
        assert_eq!(super::compact_count(999), "999");
        assert_eq!(super::compact_count(1_000), "1.0k");
        assert_eq!(super::compact_count(999_949), "999.9k");
        assert_eq!(super::compact_count(999_950), "1.0M");
        assert_eq!(super::compact_count(1_000_000), "1.0M");
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
        assert_eq!(parts.hit_percent, Some(98));
        assert_eq!(parts.remaining_percent, Some(99));
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

    #[test]
    fn context_remaining_percent_depletes_and_hides_an_unknown_window() {
        assert_eq!(context_remaining_percent(0, 1_000_000), Some(100));
        assert_eq!(context_remaining_percent(500_000, 1_000_000), Some(50));
        assert_eq!(context_remaining_percent(200_000, 1_000_000), Some(80));
        assert_eq!(context_remaining_percent(1_000_000, 1_000_000), Some(0));
        // Over-window saturates at empty rather than wrapping negative.
        assert_eq!(context_remaining_percent(1_200_000, 1_000_000), Some(0));
        assert_eq!(context_remaining_percent(12_000, 0), None);
        assert_eq!(context_remaining_percent(0, 0), None);
        // The parts carry it, so frontends never recompute the share.
        let parts = context_meter_parts(200_000, 1_000_000, 0, "cached", "cache hit");
        assert_eq!(parts.remaining_percent, Some(80));
        let unknown = context_meter_parts(200_000, 0, 0, "cached", "cache hit");
        assert_eq!(unknown.remaining_percent, None);
    }

    #[test]
    fn a_turn_cache_hit_uses_the_summed_tokens() {
        let openai = TurnStats {
            model: "m".to_owned(),
            input: 3_000,
            output: 40,
            cache: Some(900),
            requests: 1,
            elapsed_ms: 1,
        };
        assert_eq!(turn_cache_hit(&openai), Some(30));
        // Anthropic bills cache reads outside input. The share is against
        // the sum, same as one prompt.
        let anthropic = TurnStats {
            model: "m".to_owned(),
            input: 150,
            output: 40,
            cache: Some(1_300),
            requests: 1,
            elapsed_ms: 1,
        };
        assert_eq!(turn_cache_hit(&anthropic), Some(89));
        let uncached = TurnStats {
            cache: None,
            input: 3_000,
            ..openai
        };
        assert_eq!(turn_cache_hit(&uncached), None);
    }

    #[test]
    fn cache_collapse_requires_a_warm_prompt_and_a_sharp_drop() {
        assert!(cache_hit_collapsed(Some(92), Some(8)));
        assert!(cache_hit_collapsed(Some(92), None));
        assert!(cache_hit_collapsed(Some(80), Some(40)));
        assert!(cache_hit_collapsed(Some(50), Some(20)));
        assert!(!cache_hit_collapsed(Some(80), Some(50)));
        assert!(!cache_hit_collapsed(Some(50), Some(25)));
        assert!(!cache_hit_collapsed(Some(49), Some(0)));
        assert!(!cache_hit_collapsed(None, Some(0)));
        assert!(!cache_hit_collapsed(Some(100), Some(70)));
        assert!(cache_hit_recovered(Some(50)));
        assert!(cache_hit_recovered(Some(80)));
        assert!(!cache_hit_recovered(Some(49)));
        assert!(!cache_hit_recovered(None));
    }

    #[test]
    fn cache_alert_names_the_cause_and_both_percents() {
        let alert = CacheAlert {
            cause: CacheBreak::Compaction,
            before: 92,
            after: 8,
        };
        assert_eq!(
            cache_alert_text(&alert),
            "Cache hit fell from 92% to 8% after compaction."
        );
        assert_eq!(
            cache_alert_text(&CacheAlert {
                cause: CacheBreak::Steer,
                ..alert
            }),
            "Cache hit fell from 92% to 8% after a steer."
        );
        assert_eq!(
            cache_alert_text(&CacheAlert {
                cause: CacheBreak::Both,
                ..alert
            }),
            "Cache hit fell from 92% to 8% after compaction and a steer."
        );
        assert_eq!(
            CacheBreak::Compaction.merge(CacheBreak::Steer),
            CacheBreak::Both
        );
        assert_eq!(
            CacheBreak::Steer.merge(CacheBreak::Steer),
            CacheBreak::Steer
        );
    }
}
