//! Bounded fuzzy matching for [`super::EditOp::Fuzzy`].
//!
//! Needle and haystack are tokenized (whitespace collapsed; words vs
//! punctuation). Candidate windows are scored with a capped Levenshtein
//! distance on the space-joined token form. Only a unique best match whose
//! margin over the runner-up is large enough is committed.
use std::collections::BTreeSet;

use super::MAX_DIFF_SUMMARY_BYTES;
use super::engine::{Planned, reserve_planned, snippet};
use super::line::line_location;
use crate::builtin::fs_search::MAX_PATTERN_BYTES;
use crate::tool::ToolError;
use tokio_util::sync::CancellationToken;

/// Inclusive maximum `max_distance`. Values above this make short needles
/// match unrelated tokens; tests pin 1..=3.
pub(super) const MAX_FUZZY_DISTANCE: u32 = 3;
/// Inclusive minimum `max_distance`. Zero would be exact matching, which
/// already exists as `literal` and must not be a silent fuzzy fallback.
const MIN_FUZZY_DISTANCE: u32 = 1;
/// Normalized needle length cap in Unicode scalars.
///
/// Fuzzy is for a local snippet, not a whole-file rewrite. 256 scalars is
/// well above a typical unique fragment and keeps the DP band small.
const MAX_FUZZY_NORM_CHARS: usize = 256;
/// Extra uniqueness margin grows one step per this many normalized
/// characters so longer needles must beat the runner-up by more than a
/// single edit. Changing this changes rejection aggressiveness; tests pin 8.
const FUZZY_MARGIN_STEP: usize = 8;
/// Near-miss lines included in a rejection. Matches the bounded diff
/// summary philosophy (never the whole file).
const MAX_FUZZY_PREVIEW: usize = 3;
/// Maximum tokens retained from one fuzzy haystack.
///
/// At roughly 40 bytes per token, this keeps token metadata near 10 MiB even
/// for punctuation-heavy input while allowing large ordinary source files.
pub(super) const MAX_FUZZY_TOKENS: usize = 262_144;
/// Maximum candidate windows scored by one fuzzy operation.
///
/// The distance band can grow with the uniqueness margin, so this cap bounds
/// total CPU independently of how many windows happen to be near matches.
const MAX_FUZZY_WINDOWS: usize = 8_192;
/// Byte interval between cancellation checks while tokenizing.
const FUZZY_CANCEL_INTERVAL: usize = 4 * 1024;

pub(super) fn prepare(
    pattern: &str,
    replacement: &str,
    max_distance: u32,
) -> Result<super::engine::PreparedOp, ToolError> {
    if pattern.is_empty() {
        return Err(ToolError::InvalidArgs(
            "fuzzy pattern must not be empty".to_owned(),
        ));
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "fuzzy pattern exceeds {MAX_PATTERN_BYTES} bytes"
        )));
    }
    if replacement.len() > MAX_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "fuzzy replacement exceeds {MAX_PATTERN_BYTES} bytes"
        )));
    }
    if !(MIN_FUZZY_DISTANCE..=MAX_FUZZY_DISTANCE).contains(&max_distance) {
        return Err(ToolError::InvalidArgs(format!(
            "max_distance must be {MIN_FUZZY_DISTANCE}..={MAX_FUZZY_DISTANCE}"
        )));
    }
    let normalized = normalize(pattern, MAX_FUZZY_TOKENS, None)?;
    if normalized.tokens.is_empty() {
        return Err(ToolError::InvalidArgs(
            "fuzzy pattern is only whitespace after tokenization".to_owned(),
        ));
    }
    if normalized.norm.chars().count() > MAX_FUZZY_NORM_CHARS {
        return Err(ToolError::InvalidArgs(format!(
            "fuzzy pattern exceeds {MAX_FUZZY_NORM_CHARS} normalized characters"
        )));
    }
    Ok(super::engine::PreparedOp::Fuzzy {
        pattern: pattern.to_owned(),
        replacement: replacement.to_owned(),
        max_distance,
    })
}

pub(super) struct FuzzyPlan<'a> {
    pub pattern: &'a str,
    pub replacement: &'a str,
    pub max_distance: u32,
}

pub(super) fn plan_fuzzy(
    body: &str,
    op: FuzzyPlan<'_>,
    bom_len: usize,
    planned: &mut Vec<Planned>,
    replacement_bytes: &mut usize,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    let FuzzyPlan {
        pattern,
        replacement,
        max_distance,
    } = op;
    let needle = normalize(pattern, MAX_FUZZY_TOKENS, Some(cancel))?;
    let haystack = normalize(body, MAX_FUZZY_TOKENS, Some(cancel))?;
    let needle_chars = needle.norm.chars().count();
    let required_margin = required_margin(needle_chars);
    let ranking_distance = max_distance.saturating_add(required_margin.saturating_sub(1));

    let rank = score_windows(&needle, &haystack, ranking_distance, cancel)?;
    let Some(best) = rank
        .best
        .filter(|candidate| candidate.distance <= max_distance)
    else {
        return Err(distance_overflow_error(max_distance, &rank.preview, body));
    };
    let unique_with_margin = match rank.second {
        None => true,
        Some(second) => {
            second.distance > best.distance && second.distance - best.distance >= required_margin
        }
    };
    if !unique_with_margin {
        return Err(ambiguous_error(&rank.preview, body, best, required_margin));
    }

    reserve_planned(
        planned,
        replacement_bytes,
        best.start + bom_len,
        best.end + bom_len,
        replacement,
    )
}

/// Unique-best margin on the normalized needle length.
///
/// Always at least 1 (strictly better than the runner-up). One extra edit of
/// slack is required per [`FUZZY_MARGIN_STEP`] characters so two similar long
/// sites are not treated as a confident unique hit.
fn required_margin(normalized_len: usize) -> u32 {
    let extra = u32::try_from(normalized_len / FUZZY_MARGIN_STEP).unwrap_or(u32::MAX);
    1u32.saturating_add(extra)
}

#[derive(Clone, Copy)]
struct Candidate {
    start: usize,
    end: usize,
    distance: u32,
}

struct Token {
    orig_start: usize,
    orig_end: usize,
    norm_start: usize,
    norm_end: usize,
    is_word: bool,
}

struct Normalized {
    norm: String,
    tokens: Vec<Token>,
}

fn normalize(
    text: &str,
    max_tokens: usize,
    cancel: Option<&CancellationToken>,
) -> Result<Normalized, ToolError> {
    let mut norm = String::new();
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut next_cancel_check = FUZZY_CANCEL_INTERVAL;
    while let Some((index, ch)) = chars.next() {
        if index >= next_cancel_check {
            check_cancel(cancel)?;
            next_cancel_check = index.saturating_add(FUZZY_CANCEL_INTERVAL);
        }
        if ch.is_whitespace() {
            continue;
        }
        if tokens.len() >= max_tokens {
            return Err(ToolError::InvalidArgs(format!(
                "fuzzy input exceeds {max_tokens} tokens"
            )));
        }
        let is_word = ch.is_alphanumeric() || ch == '_';
        let start = index;
        let mut end = index + ch.len_utf8();
        if is_word {
            while let Some((next, next_ch)) = chars.peek().copied() {
                if next >= next_cancel_check {
                    check_cancel(cancel)?;
                    next_cancel_check = next.saturating_add(FUZZY_CANCEL_INTERVAL);
                }
                if next_ch.is_alphanumeric() || next_ch == '_' {
                    chars.next();
                    end = next + next_ch.len_utf8();
                } else {
                    break;
                }
            }
        }
        if !norm.is_empty() {
            norm.push(' ');
        }
        let norm_start = norm.len();
        norm.push_str(&text[start..end]);
        tokens.push(Token {
            orig_start: start,
            orig_end: end,
            norm_start,
            norm_end: norm.len(),
            is_word,
        });
    }
    check_cancel(cancel)?;
    Ok(Normalized { norm, tokens })
}

fn check_cancel(cancel: Option<&CancellationToken>) -> Result<(), ToolError> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        Err(ToolError::Execution(
            "file operation cancelled before completion".to_owned(),
        ))
    } else {
        Ok(())
    }
}

struct Rank {
    best: Option<Candidate>,
    second: Option<Candidate>,
    preview: Vec<PreviewCandidate>,
}

#[derive(Clone, Copy)]
struct PreviewCandidate {
    start: usize,
    end: usize,
    distance: PreviewDistance,
}

#[derive(Clone, Copy)]
enum PreviewDistance {
    Exact(u32),
    GreaterThan(u32),
}

impl PreviewDistance {
    fn lower_bound(self) -> u32 {
        match self {
            Self::Exact(distance) => distance,
            Self::GreaterThan(limit) => limit.saturating_add(1),
        }
    }
}

fn score_windows(
    needle: &Normalized,
    haystack: &Normalized,
    ranking_distance: u32,
    cancel: &CancellationToken,
) -> Result<Rank, ToolError> {
    let mut rank = Rank {
        best: None,
        second: None,
        preview: Vec::new(),
    };
    if haystack.tokens.is_empty() {
        return Ok(rank);
    }
    let t = needle.tokens.len();
    let k = ranking_distance as usize;
    let min_toks = t.saturating_sub(k).max(1);
    let max_toks = t.saturating_add(k);
    let starts = candidate_starts(needle, haystack, max_toks, k, cancel)?;
    let mut attempted = 0usize;
    for start in starts {
        for wlen in min_toks..=max_toks {
            if start + wlen > haystack.tokens.len() {
                break;
            }
            attempted += 1;
            if attempted > MAX_FUZZY_WINDOWS {
                return Err(ToolError::InvalidArgs(format!(
                    "fuzzy search exceeded {MAX_FUZZY_WINDOWS} candidate windows"
                )));
            }
            if attempted.is_multiple_of(256) {
                check_cancel(Some(cancel))?;
            }
            let first = &haystack.tokens[start];
            let last = &haystack.tokens[start + wlen - 1];
            let window_norm = &haystack.norm[first.norm_start..last.norm_end];
            let range = PreviewCandidate {
                start: first.orig_start,
                end: last.orig_end,
                distance: PreviewDistance::GreaterThan(ranking_distance),
            };
            let Some(distance) =
                bounded_levenshtein_str(&needle.norm, window_norm, ranking_distance)
            else {
                remember_preview(&mut rank.preview, range);
                continue;
            };
            consider(
                &mut rank,
                Candidate {
                    start: range.start,
                    end: range.end,
                    distance,
                },
            );
        }
    }
    if rank.preview.is_empty() {
        // An anchorless search has no candidate inside the ranking band, but
        // still needs one bounded diagnostic line without changing acceptance.
        let first = &haystack.tokens[0];
        let last = &haystack.tokens[max_toks.min(haystack.tokens.len()) - 1];
        remember_preview(
            &mut rank.preview,
            PreviewCandidate {
                start: first.orig_start,
                end: last.orig_end,
                distance: PreviewDistance::GreaterThan(ranking_distance),
            },
        );
    }
    Ok(rank)
}

fn consider(rank: &mut Rank, candidate: Candidate) {
    remember_preview(
        &mut rank.preview,
        PreviewCandidate {
            start: candidate.start,
            end: candidate.end,
            distance: PreviewDistance::Exact(candidate.distance),
        },
    );
    match rank.best {
        None => rank.best = Some(candidate),
        Some(best) if candidate.start == best.start && candidate.end == best.end => {
            if candidate.distance < best.distance {
                rank.best = Some(candidate);
            }
        }
        Some(best) if candidate.distance < best.distance => {
            rank.second = rank.best;
            rank.best = Some(candidate);
        }
        Some(best) if candidate.distance == best.distance => {
            rank.second = Some(candidate);
        }
        Some(_) => match rank.second {
            None => rank.second = Some(candidate),
            Some(second) if candidate.distance < second.distance => {
                rank.second = Some(candidate);
            }
            _ => {}
        },
    }
}

fn candidate_starts(
    needle: &Normalized,
    haystack: &Normalized,
    max_toks: usize,
    ranking_distance: usize,
    cancel: &CancellationToken,
) -> Result<Vec<usize>, ToolError> {
    let needle_words: Vec<&str> = needle
        .tokens
        .iter()
        .filter(|tok| tok.is_word)
        .map(|tok| &needle.norm[tok.norm_start..tok.norm_end])
        .collect();
    // One character edit can merge or split two adjacent word tokens, so an
    // exact anchor is guaranteed only when more than twice the edit band's
    // word count remains.
    if needle_words.len() > ranking_distance.saturating_mul(2) {
        let mut starts = BTreeSet::new();
        for (index, tok) in haystack.tokens.iter().enumerate() {
            if index.is_multiple_of(FUZZY_CANCEL_INTERVAL) {
                check_cancel(Some(cancel))?;
            }
            if !tok.is_word {
                continue;
            }
            let text = &haystack.norm[tok.norm_start..tok.norm_end];
            if !needle_words.contains(&text) {
                continue;
            }
            let from = index.saturating_sub(max_toks.saturating_sub(1));
            for start in from..=index {
                starts.insert(start);
                if starts.len() > MAX_FUZZY_WINDOWS {
                    return Err(ToolError::InvalidArgs(format!(
                        "fuzzy search exceeded {MAX_FUZZY_WINDOWS} candidate starts"
                    )));
                }
            }
        }
        Ok(starts.into_iter().collect())
    } else if haystack.tokens.len() > MAX_FUZZY_WINDOWS {
        Err(ToolError::InvalidArgs(format!(
            "fuzzy search exceeded {MAX_FUZZY_WINDOWS} candidate starts"
        )))
    } else {
        Ok((0..haystack.tokens.len()).collect())
    }
}

fn remember_preview(preview: &mut Vec<PreviewCandidate>, candidate: PreviewCandidate) {
    match candidate.distance {
        PreviewDistance::Exact(_) => {
            preview.retain(|row| matches!(row.distance, PreviewDistance::Exact(_)));
        }
        PreviewDistance::GreaterThan(_)
            if preview
                .iter()
                .any(|row| matches!(row.distance, PreviewDistance::Exact(_))) =>
        {
            return;
        }
        PreviewDistance::GreaterThan(_) => {}
    }
    if preview
        .iter()
        .any(|row| row.start == candidate.start && row.end == candidate.end)
    {
        return;
    }
    preview.push(candidate);
    preview.sort_by_key(|row| (row.distance.lower_bound(), row.start, row.end));
    preview.truncate(MAX_FUZZY_PREVIEW);
}

/// Character (ASCII byte) Levenshtein using an `O(k * n)` distance band.
///
/// Returns `None` when the distance would exceed `max_distance`. The needle is
/// capped by [`MAX_FUZZY_NORM_CHARS`], and the length-difference check bounds
/// candidate conversion before allocating Unicode scalar buffers.
fn bounded_levenshtein_str(a: &str, b: &str, max_distance: u32) -> Option<u32> {
    if a == b {
        return Some(0);
    }
    let a_len = a.chars().count();
    let max = max_distance as usize;
    let b_len = b
        .chars()
        .take(a_len.saturating_add(max).saturating_add(1))
        .count();
    if a_len > MAX_FUZZY_NORM_CHARS || a_len.abs_diff(b_len) > max {
        return None;
    }
    if a.is_ascii() && b.is_ascii() {
        bounded_levenshtein(a.as_bytes(), b.as_bytes(), max_distance)
    } else {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        bounded_levenshtein(&a_chars, &b_chars, max_distance)
    }
}

fn bounded_levenshtein<T: PartialEq>(a: &[T], b: &[T], max_distance: u32) -> Option<u32> {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let max = max_distance as usize;
    if long.len().saturating_sub(short.len()) > max {
        return None;
    }
    let outside = max_distance.saturating_add(1);
    let mut prev = vec![outside; short.len() + 1];
    let mut curr = vec![outside; short.len() + 1];
    for (index, value) in prev.iter_mut().enumerate().take(max.min(short.len()) + 1) {
        *value = index as u32;
    }
    for (i, long_item) in long.iter().enumerate() {
        let row = i + 1;
        let first = row.saturating_sub(max).max(1);
        let last = row.saturating_add(max).min(short.len());
        curr[0] = if row <= max { row as u32 } else { outside };
        if first > 1 {
            curr[first - 1] = outside;
        }
        for j in first..=last {
            let cost = u32::from(short[j - 1] != *long_item);
            let insert = curr[j - 1].saturating_add(1);
            let delete = prev[j].saturating_add(1);
            let subst = prev[j - 1].saturating_add(cost);
            curr[j] = insert.min(delete).min(subst).min(outside);
        }
        if last < short.len() {
            curr[last + 1] = outside;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    let distance = prev[short.len()];
    (distance <= max_distance).then_some(distance)
}

fn distance_overflow_error(
    max_distance: u32,
    preview: &[PreviewCandidate],
    body: &str,
) -> ToolError {
    let mut msg = format!("fuzzy pattern has no match within max_distance={max_distance}");
    if let Some(best) = preview.first() {
        match best.distance {
            PreviewDistance::Exact(distance) => {
                msg.push_str(&format!(" (nearest distance {distance})"));
            }
            PreviewDistance::GreaterThan(limit) => {
                msg.push_str(&format!(" (nearest distance > {limit})"));
            }
        }
    }
    append_preview(&mut msg, preview, body);
    ToolError::Execution(msg)
}

fn ambiguous_error(
    ranked: &[PreviewCandidate],
    body: &str,
    best: Candidate,
    required_margin: u32,
) -> ToolError {
    let mut msg = format!(
        "fuzzy match is not a unique best (best distance {}, required margin {required_margin})",
        best.distance
    );
    append_preview(&mut msg, ranked, body);
    ToolError::Execution(msg)
}

fn append_preview(msg: &mut String, ranked: &[PreviewCandidate], body: &str) {
    if ranked.is_empty() {
        return;
    }
    msg.push_str("; near-misses:");
    for candidate in ranked.iter().take(MAX_FUZZY_PREVIEW) {
        if msg.len() >= MAX_DIFF_SUMMARY_BYTES {
            msg.push_str(" [preview truncated]");
            break;
        }
        let (line, line_start, line_end) = line_location(body, candidate.start);
        let excerpt = preview_excerpt(body, line_start, line_end, candidate.start);
        match candidate.distance {
            PreviewDistance::Exact(distance) => {
                msg.push_str(&format!("\n  line {line} d={distance}: {excerpt}"));
            }
            PreviewDistance::GreaterThan(limit) => {
                msg.push_str(&format!("\n  line {line} d>{limit}: {excerpt}"));
            }
        }
    }
}

/// Returns a bounded line excerpt anchored at the candidate start.
///
/// A leading ellipsis marks omitted line content. [`snippet`] adds a trailing
/// ellipsis when the anchored suffix exceeds [`super::MAX_DIFF_SNIPPET`].
fn preview_excerpt(
    body: &str,
    line_start: usize,
    line_end: usize,
    candidate_start: usize,
) -> String {
    let start = candidate_start.clamp(line_start, line_end);
    let anchored = body
        .get(start..line_end)
        .unwrap_or_else(|| &body[line_start..line_end]);
    let mut excerpt = snippet(anchored);
    if start > line_start {
        excerpt.insert(0, '…');
    }
    excerpt
}
