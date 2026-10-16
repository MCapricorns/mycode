//! Private planning and apply engine for [`super::EditTool`].
use aho_corasick::{AhoCorasick, MatchKind};
use memchr::memmem::Finder;
use regex::RegexBuilder;
use serde_json::json;
use std::fmt::Write as _;
use tokio_util::sync::CancellationToken;

use super::{
    EditArgs, EditOp, MAX_CAPTURE_BYTES, MAX_LITERAL_PATTERNS, MAX_MATCHES, MAX_OPERATIONS,
    Occurrence, REGEX_NEST_LIMIT, UTF8_BOM,
};
use crate::builtin::fs_io::MAX_WRITE_BYTES;
use crate::builtin::fs_search::{MAX_PATTERN_BYTES, REGEX_DFA_SIZE_LIMIT, REGEX_SIZE_LIMIT};
use crate::tool::{ToolError, ToolResult};

mod preview;
use self::preview::append_diff;
pub(super) use self::preview::snippet;

pub(super) fn check_cancel(cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::Execution(
            "file operation cancelled before completion".to_owned(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn edit_result(
    path_key: &str,
    revision: &str,
    applied: &Applied,
    detached_hardlink: bool,
) -> ToolResult {
    let mut text = format!(
        "Edited {path_key}: {} replacement{}, {} → {} bytes",
        applied.replacements,
        if applied.replacements == 1 { "" } else { "s" },
        applied.bytes_before,
        applied.bytes_after,
    );
    if detached_hardlink {
        text.push_str(" (detached_hardlink=true: this directory entry now names a new inode)");
    }
    let _ = write!(text, "\n[revision {revision}]");
    ToolResult::text(text).with_details(json!({
        "path": path_key,
        "replacements": applied.replacements,
        "bytes_before": applied.bytes_before,
        "bytes_after": applied.bytes_after,
        "revision": revision,
        "detached_hardlink": detached_hardlink,
        "diff": applied.diff,
    }))
}

pub(super) enum PreparedOp {
    Literal {
        patterns: Vec<String>,
        replacements: Vec<String>,
        pick: Pick,
    },
    Regex {
        compiled: regex::Regex,
        replacement: String,
        pick: Pick,
    },
    Fuzzy {
        pattern: String,
        replacement: String,
        max_distance: u32,
    },
    LineRange {
        start_line: usize,
        end_line: usize,
        expected_text: Option<String>,
        expected_hash: Option<String>,
        replacement: String,
    },
}

#[derive(Clone, Copy)]
pub(super) enum Pick {
    Unique,
    All,
    Nth(usize),
}

pub(super) struct Planned {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) replacement: String,
}

pub(super) struct Applied {
    pub(super) text: String,
    pub(super) replacements: usize,
    pub(super) bytes_before: usize,
    pub(super) bytes_after: usize,
    pub(super) diff: String,
}

pub(super) fn normalize_args(args: &EditArgs) -> Result<Vec<PreparedOp>, ToolError> {
    // Models often send `old_string`/`new_string` together with `operations`.
    // The batch is authoritative: legacy fields are not validated and do not
    // apply. An operations-only call is the same path.
    if let Some(operations) = args.operations.as_deref() {
        return prepare_operations(operations);
    }
    match (&args.old_string, &args.new_string) {
        (Some(old), Some(new)) => {
            if old.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "old_string must not be empty; provide the text to replace".into(),
                ));
            }
            bound_pattern("old_string", old)?;
            bound_pattern("new_string", new)?;
            Ok(vec![PreparedOp::Literal {
                patterns: vec![old.clone()],
                replacements: vec![new.clone()],
                pick: Pick::Unique,
            }])
        }
        (Some(_), None) => Err(ToolError::InvalidArgs(
            "new_string is required when old_string is set".to_owned(),
        )),
        (None, Some(_)) => Err(ToolError::InvalidArgs(
            "old_string is required when new_string is set".to_owned(),
        )),
        (None, None) => Err(ToolError::InvalidArgs(
            "provide old_string/new_string or operations".to_owned(),
        )),
    }
}

fn prepare_operations(operations: &[EditOp]) -> Result<Vec<PreparedOp>, ToolError> {
    if operations.is_empty() {
        return Err(ToolError::InvalidArgs(
            "operations must not be empty".to_owned(),
        ));
    }
    if operations.len() > MAX_OPERATIONS {
        return Err(ToolError::InvalidArgs(format!(
            "at most {MAX_OPERATIONS} operations are allowed"
        )));
    }
    operations.iter().map(prepare_op).collect()
}

fn bound_pattern(label: &str, value: &str) -> Result<(), ToolError> {
    if value.len() > MAX_PATTERN_BYTES {
        Err(ToolError::InvalidArgs(format!(
            "{label} exceeds {MAX_PATTERN_BYTES} bytes"
        )))
    } else {
        Ok(())
    }
}

fn prepare_op(op: &EditOp) -> Result<PreparedOp, ToolError> {
    match op {
        EditOp::Literal {
            pattern,
            replacement,
            patterns,
            replacements,
            occurrence,
            n,
        } => {
            let pick = pick_from(*occurrence, *n, true)?;
            let (patterns, replacements) = literal_needles(
                pattern.as_deref(),
                replacement.as_deref(),
                patterns.as_deref(),
                replacements.as_deref(),
            )?;
            Ok(PreparedOp::Literal {
                patterns,
                replacements,
                pick,
            })
        }
        EditOp::Regex {
            pattern,
            replacement,
            occurrence,
            n,
        } => {
            if matches!(occurrence, Occurrence::Nth) || n.is_some() {
                return Err(ToolError::InvalidArgs(
                    "regex operations support occurrence unique or all, not nth".to_owned(),
                ));
            }
            let pick = pick_from(*occurrence, None, false)?;
            bound_pattern("regex pattern", pattern)?;
            bound_pattern("regex replacement", replacement)?;
            if pattern.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "regex pattern must not be empty".to_owned(),
                ));
            }
            let compiled = RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
                .nest_limit(REGEX_NEST_LIMIT)
                .build()
                .map_err(|error| {
                    ToolError::InvalidArgs(format!(
                        "invalid regex (lookaround and pattern backreferences are not supported): {error}"
                    ))
                })?;
            Ok(PreparedOp::Regex {
                compiled,
                replacement: replacement.clone(),
                pick,
            })
        }
        EditOp::Fuzzy {
            pattern,
            replacement,
            max_distance,
        } => super::fuzzy::prepare(pattern, replacement, *max_distance),
        EditOp::LineRange {
            start_line,
            end_line,
            expected_text,
            expected_hash,
            replacement,
        } => {
            if *start_line < 1 || *end_line < *start_line {
                return Err(ToolError::InvalidArgs(
                    "line_range requires 1-based start_line <= end_line".to_owned(),
                ));
            }
            if expected_text.is_none() && expected_hash.is_none() {
                return Err(ToolError::InvalidArgs(
                    "line_range requires expected_text and/or expected_hash".to_owned(),
                ));
            }
            if let Some(text) = expected_text {
                bound_pattern("expected_text", text)?;
            }
            bound_pattern("line_range replacement", replacement)?;
            Ok(PreparedOp::LineRange {
                start_line: *start_line,
                end_line: *end_line,
                expected_text: expected_text.clone(),
                expected_hash: expected_hash.clone(),
                replacement: replacement.clone(),
            })
        }
    }
}

fn pick_from(occurrence: Occurrence, n: Option<u32>, nth_ok: bool) -> Result<Pick, ToolError> {
    match occurrence {
        Occurrence::Unique => {
            if n.is_some() {
                return Err(ToolError::InvalidArgs(
                    "n is only valid with occurrence=nth".to_owned(),
                ));
            }
            Ok(Pick::Unique)
        }
        Occurrence::All => {
            if n.is_some() {
                return Err(ToolError::InvalidArgs(
                    "n is only valid with occurrence=nth".to_owned(),
                ));
            }
            Ok(Pick::All)
        }
        Occurrence::Nth => {
            if !nth_ok {
                return Err(ToolError::InvalidArgs(
                    "nth is not supported for this operation".to_owned(),
                ));
            }
            let n = n.ok_or_else(|| {
                ToolError::InvalidArgs("occurrence=nth requires a 1-based n".to_owned())
            })?;
            if n < 1 {
                return Err(ToolError::InvalidArgs("n must be >= 1".to_owned()));
            }
            Ok(Pick::Nth(n as usize))
        }
    }
}

fn literal_needles(
    pattern: Option<&str>,
    replacement: Option<&str>,
    patterns: Option<&[String]>,
    replacements: Option<&[String]>,
) -> Result<(Vec<String>, Vec<String>), ToolError> {
    match (pattern, replacement, patterns, replacements) {
        (Some(pattern), Some(replacement), None, None) => {
            if pattern.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "literal pattern must not be empty".to_owned(),
                ));
            }
            bound_pattern("literal pattern", pattern)?;
            bound_pattern("literal replacement", replacement)?;
            Ok((vec![pattern.to_owned()], vec![replacement.to_owned()]))
        }
        (None, None, Some(patterns), Some(replacements)) => {
            if patterns.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "literal patterns must not be empty".to_owned(),
                ));
            }
            if patterns.len() > MAX_LITERAL_PATTERNS {
                return Err(ToolError::InvalidArgs(format!(
                    "at most {MAX_LITERAL_PATTERNS} literal patterns are allowed"
                )));
            }
            if patterns.len() != replacements.len() {
                return Err(ToolError::InvalidArgs(
                    "patterns and replacements must have the same length".to_owned(),
                ));
            }
            for (index, needle) in patterns.iter().enumerate() {
                if needle.is_empty() {
                    return Err(ToolError::InvalidArgs(
                        "literal pattern must not be empty".to_owned(),
                    ));
                }
                bound_pattern("literal pattern", needle)?;
                bound_pattern("literal replacement", &replacements[index])?;
            }
            Ok((patterns.to_vec(), replacements.to_vec()))
        }
        _ => Err(ToolError::InvalidArgs(
            "literal operations require pattern+replacement or patterns+replacements".to_owned(),
        )),
    }
}

struct Found {
    start: usize,
    end: usize,
    pattern_id: usize,
}

pub(super) fn plan_edits(
    snapshot: &str,
    ops: &[PreparedOp],
    cancel: &CancellationToken,
) -> Result<Vec<Planned>, ToolError> {
    let (had_bom, body) = strip_bom(snapshot);
    let bom_len = if had_bom { UTF8_BOM.len() } else { 0 };
    let mut planned = Vec::new();
    let mut replacement_bytes = 0usize;
    for op in ops {
        check_cancel(cancel)?;
        match op {
            PreparedOp::Literal {
                patterns,
                replacements,
                pick,
            } => {
                let mut matches = literal_matches(body, patterns)?;
                // Deterministic positional order for `nth` picks and for the
                // overlap rejection that follows.
                matches.sort_by_key(|found| (found.start, found.end));
                reject_overlapping_candidates(&matches)?;
                let newline = dominant_newline(body);
                for found in select_matches(matches, *pick, "literal")? {
                    let replacement = match_newlines(&replacements[found.pattern_id], newline);
                    reserve_planned(
                        &mut planned,
                        &mut replacement_bytes,
                        found.start + bom_len,
                        found.end + bom_len,
                        &replacement,
                    )?;
                }
            }
            PreparedOp::Regex {
                compiled,
                replacement,
                pick,
            } => regex_planned(
                body,
                compiled,
                replacement,
                *pick,
                bom_len,
                &mut planned,
                &mut replacement_bytes,
            )?,
            PreparedOp::LineRange {
                start_line,
                end_line,
                expected_text,
                expected_hash,
                replacement,
            } => {
                let range = super::line::line_range_planned(
                    body,
                    *start_line,
                    *end_line,
                    expected_text.as_deref(),
                    expected_hash.as_deref(),
                    replacement,
                    bom_len,
                )?;
                reserve_planned(
                    &mut planned,
                    &mut replacement_bytes,
                    range.start,
                    range.end,
                    &range.replacement,
                )?;
            }
            PreparedOp::Fuzzy {
                pattern,
                replacement,
                max_distance,
            } => super::fuzzy::plan_fuzzy(
                body,
                super::fuzzy::FuzzyPlan {
                    pattern,
                    replacement,
                    max_distance: *max_distance,
                },
                bom_len,
                &mut planned,
                &mut replacement_bytes,
                cancel,
            )?,
        }
    }
    Ok(planned)
}

/// Pushes one planned replacement, enforcing the global match and aggregate
/// byte budgets before the replacement is cloned into the plan.
///
/// Checking at push time keeps peak planning memory at the budget plus one
/// replacement instead of materializing every match of an operation first.
pub(super) fn reserve_planned(
    planned: &mut Vec<Planned>,
    replacement_bytes: &mut usize,
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<(), ToolError> {
    if planned.len() >= MAX_MATCHES {
        return Err(ToolError::InvalidArgs(format!(
            "edit produced more than {MAX_MATCHES} matches"
        )));
    }
    *replacement_bytes = replacement_bytes.saturating_add(replacement.len());
    if *replacement_bytes > MAX_WRITE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "planned replacements exceed {MAX_WRITE_BYTES} bytes"
        )));
    }
    planned.push(Planned {
        start,
        end,
        replacement: replacement.to_owned(),
    });
    Ok(())
}

/// Rejects candidates whose byte ranges overlap within one operation.
///
/// Requires `matches` sorted by `(start, end)`. `nth` and `all` must never
/// silently prefer one of two competing matches; ambiguity is an error,
/// matching the cross-operation overlap rule in [`apply_planned`].
fn reject_overlapping_candidates(matches: &[Found]) -> Result<(), ToolError> {
    for pair in matches.windows(2) {
        if pair[1].start < pair[0].end {
            return Err(ToolError::InvalidArgs(
                "literal patterns overlap in the same operation; make the matched ranges unique"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

fn strip_bom(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix(UTF8_BOM) {
        (true, rest)
    } else {
        (false, text)
    }
}

fn literal_matches(body: &str, patterns: &[String]) -> Result<Vec<Found>, ToolError> {
    let exact = if patterns.len() == 1 {
        memmem_matches(body, &patterns[0])?
    } else {
        ac_matches(body, patterns)?
    };
    if !exact.is_empty() {
        return Ok(exact);
    }
    // Windows files are often CRLF while the model sends LF (or the reverse).
    // An exact miss retries on a copy with the CR of each CRLF dropped, and
    // the matched span is mapped back onto the original bytes. The
    // replacement is then rewritten to the file's dominant line ending.
    if body.contains('\r') || patterns.iter().any(|pattern| pattern.contains('\r')) {
        return newline_insensitive_matches(body, patterns);
    }
    Ok(exact)
}

struct NormalizedNewlines {
    text: String,
    /// Original byte index of each byte in [`Self::text`].
    map: Vec<usize>,
}

fn normalize_newlines(text: &str) -> NormalizedNewlines {
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 1;
            continue;
        }
        let start = index;
        let ch = text[index..].chars().next().unwrap_or('\u{fffd}');
        let len = ch.len_utf8();
        out.push(ch);
        for offset in 0..len {
            map.push(start + offset);
        }
        index += len;
    }
    NormalizedNewlines { text: out, map }
}

fn newline_insensitive_matches(body: &str, patterns: &[String]) -> Result<Vec<Found>, ToolError> {
    let norm_body = normalize_newlines(body);
    let norm_patterns: Vec<String> = patterns
        .iter()
        .map(|pattern| normalize_newlines(pattern).text)
        .collect();
    let found = if norm_patterns.len() == 1 {
        memmem_matches(&norm_body.text, &norm_patterns[0])?
    } else {
        ac_matches(&norm_body.text, &norm_patterns)?
    };
    let mut mapped = Vec::with_capacity(found.len());
    for hit in found {
        let Some((start, end)) = map_newline_span(&norm_body, body, hit.start, hit.end) else {
            continue;
        };
        mapped.push(Found {
            start,
            end,
            pattern_id: hit.pattern_id,
        });
    }
    Ok(mapped)
}

fn map_newline_span(
    norm: &NormalizedNewlines,
    body: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    if start > end || end > norm.map.len() {
        return None;
    }
    let mut orig_start = if start == norm.map.len() {
        body.len()
    } else {
        norm.map[start]
    };
    let mut orig_end = if end == norm.map.len() {
        body.len()
    } else {
        norm.map[end]
    };
    // A CRLF is one newline in the normalized text and two bytes in `body`.
    // `map` records the LF. An endpoint that lands on that LF has to step
    // back over the CR that belongs to the same newline.
    // Start: the newline is inside the match, so the CR is too.
    // End: `map[end]` is the first byte *after* the match. When that byte is
    // the LF, the CR in front of it is outside the match as well.
    if cr_before_lf(body, orig_start) {
        orig_start -= 1;
    }
    if cr_before_lf(body, orig_end) {
        orig_end -= 1;
    }
    (orig_start <= orig_end && body.is_char_boundary(orig_start) && body.is_char_boundary(orig_end))
        .then_some((orig_start, orig_end))
}

fn cr_before_lf(body: &str, index: usize) -> bool {
    index > 0
        && body.as_bytes().get(index) == Some(&b'\n')
        && body.as_bytes().get(index - 1) == Some(&b'\r')
}

/// The line ending the file already uses, when it has any newlines.
fn dominant_newline(body: &str) -> Option<&'static str> {
    let crlf = body.matches("\r\n").count();
    let lf = body.bytes().filter(|byte| *byte == b'\n').count();
    let bare_lf = lf.saturating_sub(crlf);
    if crlf == 0 && bare_lf == 0 {
        return None;
    }
    if crlf > bare_lf {
        Some("\r\n")
    } else {
        Some("\n")
    }
}

/// Rewrites `text`'s newlines to `newline`. `None` leaves `text` unchanged.
fn match_newlines(text: &str, newline: Option<&str>) -> String {
    let Some(newline) = newline else {
        return text.to_owned();
    };
    let lf = text.replace("\r\n", "\n").replace('\r', "\n");
    if newline == "\n" {
        lf
    } else {
        lf.replace('\n', "\r\n")
    }
}

fn memmem_matches(body: &str, pattern: &str) -> Result<Vec<Found>, ToolError> {
    let needle = pattern.as_bytes();
    if needle.is_empty() {
        return Err(ToolError::InvalidArgs(
            "literal patterns must not be empty".to_owned(),
        ));
    }
    let haystack = body.as_bytes();
    let finder = Finder::new(needle);
    let mut found = Vec::new();
    // Restart one byte after each hit so self-overlapping candidates (for
    // example `aa` inside `aaa`) surface and the overlap rejection can fail
    // closed instead of silently picking a leftmost subset.
    let mut cursor = 0usize;
    while let Some(relative) = finder.find(&haystack[cursor..]) {
        if found.len() >= MAX_MATCHES {
            return Err(ToolError::InvalidArgs(format!(
                "edit produced more than {MAX_MATCHES} matches"
            )));
        }
        let start = cursor + relative;
        found.push(Found {
            start,
            end: start + needle.len(),
            pattern_id: 0,
        });
        cursor = start + 1;
    }
    Ok(found)
}

fn ac_matches(body: &str, patterns: &[String]) -> Result<Vec<Found>, ToolError> {
    let ac = AhoCorasick::builder()
        .match_kind(MatchKind::Standard)
        .build(patterns)
        .map_err(|error| ToolError::InvalidArgs(format!("invalid literal patterns: {error}")))?;
    let mut found = Vec::new();
    for mat in ac.find_overlapping_iter(body) {
        if found.len() >= MAX_MATCHES {
            return Err(ToolError::InvalidArgs(format!(
                "edit produced more than {MAX_MATCHES} matches"
            )));
        }
        found.push(Found {
            start: mat.start(),
            end: mat.end(),
            pattern_id: mat.pattern().as_usize(),
        });
    }
    Ok(found)
}

fn select_matches(matches: Vec<Found>, pick: Pick, label: &str) -> Result<Vec<Found>, ToolError> {
    match pick {
        Pick::Unique => match matches.len() {
            0 => Err(ToolError::Execution(format!(
                "{label} pattern not found; re-read the file and provide the exact text"
            ))),
            1 => Ok(matches),
            n => Err(ToolError::Execution(format!(
                "{label} pattern occurs {n} times; include more surrounding lines to make it unique"
            ))),
        },
        Pick::All => {
            if matches.is_empty() {
                Err(ToolError::Execution(format!(
                    "{label} pattern not found; re-read the file and provide the exact text"
                )))
            } else {
                Ok(matches)
            }
        }
        Pick::Nth(n) => {
            if n > matches.len() {
                Err(ToolError::Execution(format!(
                    "{label} nth={n} requested but only {} match{}",
                    matches.len(),
                    if matches.len() == 1 { "" } else { "es" }
                )))
            } else {
                Ok(vec![matches.into_iter().nth(n - 1).expect("n is in range")])
            }
        }
    }
}

fn regex_planned(
    body: &str,
    compiled: &regex::Regex,
    replacement: &str,
    pick: Pick,
    bom_len: usize,
    planned: &mut Vec<Planned>,
    replacement_bytes: &mut usize,
) -> Result<(), ToolError> {
    if matches!(pick, Pick::Nth(_)) {
        return Err(ToolError::InvalidArgs(
            "regex operations support occurrence unique or all, not nth".to_owned(),
        ));
    }
    let mut pushed = 0usize;
    for caps in compiled.captures_iter(body) {
        let full = caps
            .get(0)
            .ok_or_else(|| ToolError::Execution("regex match is missing capture 0".to_owned()))?;
        if full.start() == full.end() {
            return Err(ToolError::InvalidArgs(
                "regex matches must not be zero-width".to_owned(),
            ));
        }
        if matches!(pick, Pick::Unique) && pushed > 0 {
            return Err(ToolError::Execution(
                "regex pattern occurs more than once; include more surrounding context to make it unique"
                    .to_owned(),
            ));
        }
        let expanded = expand_captures(&caps, replacement)?;
        reserve_planned(
            planned,
            replacement_bytes,
            full.start() + bom_len,
            full.end() + bom_len,
            &expanded,
        )?;
        pushed += 1;
    }
    if pushed == 0 {
        return Err(ToolError::Execution(
            "regex pattern not found; re-read the file and provide the exact text".to_owned(),
        ));
    }
    Ok(())
}

fn expand_captures(caps: &regex::Captures<'_>, template: &str) -> Result<String, ToolError> {
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let start = index;
            while index < bytes.len() && bytes[index] != b'$' {
                index += 1;
            }
            push_bounded(&mut out, &template[start..index])?;
            continue;
        }
        if index + 1 < bytes.len() && bytes[index + 1] == b'$' {
            push_bounded(&mut out, "$")?;
            index += 2;
            continue;
        }
        let (consumed, capture) = parse_capture_ref(&bytes[index..], caps)?;
        push_bounded(&mut out, capture)?;
        index += consumed;
    }
    Ok(out)
}

fn parse_capture_ref<'a>(
    rest: &[u8],
    caps: &'a regex::Captures<'a>,
) -> Result<(usize, &'a str), ToolError> {
    if rest.len() >= 2 && rest[1] == b'{' {
        let close = rest
            .iter()
            .position(|byte| *byte == b'}')
            .ok_or_else(|| ToolError::InvalidArgs("unterminated capture replacement".to_owned()))?;
        let name = std::str::from_utf8(&rest[2..close])
            .map_err(|_| ToolError::InvalidArgs("capture name is not UTF-8".to_owned()))?;
        let text = capture_text(caps, name)?;
        return Ok((close + 1, text));
    }
    // Longest possible name, matching `regex` replacement syntax: `$1a`
    // references the group named `1a`, not group 1 followed by a literal
    // `a`; an unknown group expands to the empty string.
    let mut end = 1usize;
    while end < rest.len() && (rest[end].is_ascii_alphanumeric() || rest[end] == b'_') {
        end += 1;
    }
    if end == 1 {
        return Ok((1, "$"));
    }
    let name = std::str::from_utf8(&rest[1..end])
        .map_err(|_| ToolError::InvalidArgs("capture name is not UTF-8".to_owned()))?;
    Ok((end, capture_text(caps, name)?))
}

fn capture_text<'a>(caps: &'a regex::Captures<'a>, name: &str) -> Result<&'a str, ToolError> {
    if let Ok(index) = name.parse::<usize>() {
        return Ok(caps.get(index).map(|m| m.as_str()).unwrap_or(""));
    }
    Ok(caps.name(name).map(|m| m.as_str()).unwrap_or(""))
}

fn push_bounded(out: &mut String, chunk: &str) -> Result<(), ToolError> {
    if out.len().saturating_add(chunk.len()) > MAX_CAPTURE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "capture replacement exceeds {MAX_CAPTURE_BYTES} bytes"
        )));
    }
    out.push_str(chunk);
    Ok(())
}

pub(super) fn apply_planned(
    snapshot: &str,
    planned: &[Planned],
    cancel: &CancellationToken,
) -> Result<Applied, ToolError> {
    check_cancel(cancel)?;
    let mut ordered: Vec<&Planned> = planned.iter().collect();
    ordered.sort_by_key(|item| (item.start, item.end));
    for item in &ordered {
        if item.start > item.end
            || item.end > snapshot.len()
            || !snapshot.is_char_boundary(item.start)
            || !snapshot.is_char_boundary(item.end)
        {
            return Err(ToolError::Execution(
                "edit match is not on a UTF-8 character boundary".to_owned(),
            ));
        }
    }
    for pair in ordered.windows(2) {
        if pair[1].start < pair[0].end {
            return Err(ToolError::Execution(
                "edit operations overlap on the snapshot; split them or make ranges unique"
                    .to_owned(),
            ));
        }
        if pair[1].start == pair[0].start {
            return Err(ToolError::Execution(
                "edit operations are order-dependent at the same byte range".to_owned(),
            ));
        }
    }
    let (had_bom, _body) = strip_bom(snapshot);
    let extra: isize = ordered.iter().fold(0isize, |acc, item| {
        acc.saturating_add(item.replacement.len() as isize)
            .saturating_sub((item.end - item.start) as isize)
    });
    // BOM re-insertion below only restores bytes a replacement removed, so
    // the final length never exceeds this bound; adding BOM headroom here
    // would reject content that lands exactly on the write limit.
    let capacity = snapshot.len().saturating_add_signed(extra);
    if capacity > MAX_WRITE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "edited content exceeds {MAX_WRITE_BYTES} bytes"
        )));
    }
    let mut text = String::with_capacity(capacity);
    let mut cursor = 0usize;
    let mut diff = String::new();
    for item in &ordered {
        check_cancel(cancel)?;
        text.push_str(&snapshot[cursor..item.start]);
        let old = &snapshot[item.start..item.end];
        append_diff(&mut diff, old, &item.replacement);
        text.push_str(&item.replacement);
        cursor = item.end;
        if text.len() > MAX_WRITE_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "edited content exceeds {MAX_WRITE_BYTES} bytes"
            )));
        }
    }
    text.push_str(&snapshot[cursor..]);
    if had_bom && !text.starts_with(UTF8_BOM) {
        text.insert_str(0, UTF8_BOM);
    }
    if text.len() > MAX_WRITE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "edited content exceeds {MAX_WRITE_BYTES} bytes"
        )));
    }
    Ok(Applied {
        replacements: ordered.len(),
        bytes_before: snapshot.len(),
        bytes_after: text.len(),
        diff,
        text,
    })
}

#[cfg(test)]
mod newline_tests;

#[cfg(test)]
mod normalize_tests;
