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
use crate::tool::{INTERRUPTED_NOTE, ToolError, ToolResult};

mod preview;
use self::preview::append_diff;
pub(super) use self::preview::snippet;

pub(super) fn check_cancel(cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::Execution(format!(
            "file operation cancelled before completion{INTERRUPTED_NOTE}"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn edit_result(
    path_key: &str,
    revision: &str,
    applied: &Applied,
    notes: &[String],
    detached_hardlink: bool,
) -> ToolResult {
    let mut text = format!(
        "Edited {path_key}: {} replacement{}, {} → {} bytes",
        applied.replacements,
        if applied.replacements == 1 { "" } else { "s" },
        applied.bytes_before,
        applied.bytes_after,
    );
    for note in notes {
        text.push_str("\nnote: ");
        text.push_str(note);
    }
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
                pick: if args.replace_all {
                    Pick::All
                } else {
                    Pick::Unique
                },
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

#[cfg(test)]
pub(super) fn plan_edits(
    snapshot: &str,
    ops: &[PreparedOp],
    cancel: &CancellationToken,
) -> Result<Vec<Planned>, ToolError> {
    plan_edits_with_notes(snapshot, ops, cancel).map(|(planned, _)| planned)
}

/// Plans edits and reports any compatibility repair that was applied, so the
/// model learns when its pattern was matched approximately.
pub(super) fn plan_edits_with_notes(
    snapshot: &str,
    ops: &[PreparedOp],
    cancel: &CancellationToken,
) -> Result<(Vec<Planned>, Vec<String>), ToolError> {
    let (had_bom, body) = strip_bom(snapshot);
    let bom_len = if had_bom { UTF8_BOM.len() } else { 0 };
    let mut planned = Vec::new();
    let mut notes = Vec::new();
    let mut replacement_bytes = 0usize;
    for (index, op) in ops.iter().enumerate() {
        check_cancel(cancel)?;
        plan_op(
            body,
            op,
            bom_len,
            &mut planned,
            &mut notes,
            &mut replacement_bytes,
            cancel,
        )
        .map_err(|error| tag_op_error(index, ops.len(), error))?;
    }
    Ok((planned, notes))
}

/// Plans one prepared operation onto the pending replacement list.
fn plan_op(
    body: &str,
    op: &PreparedOp,
    bom_len: usize,
    planned: &mut Vec<Planned>,
    notes: &mut Vec<String>,
    replacement_bytes: &mut usize,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    match op {
        PreparedOp::Literal {
            patterns,
            replacements,
            pick,
        } => {
            let mut matches = literal_matches(body, patterns)?;
            if matches.is_empty() && patterns.len() == 1 {
                // Models often reproduce a block from an older read with
                // different indentation or trailing spaces. Match the same
                // lines ignoring only whitespace before giving up.
                matches = relaxed_literal_matches(body, &patterns[0]);
                if !matches.is_empty() {
                    notes.push(format!(
                        "literal pattern matched ignoring whitespace/indentation: {}",
                        snippet(&patterns[0])
                    ));
                }
            }
            if matches.is_empty()
                && patterns.len() == 1
                && *pick == Pick::Unique
                && !patterns[0].trim().is_empty()
            {
                // A one-character difference (typo, renamed local, quote
                // style) still has one close site. Fuzzy commits only a
                // unique best match at distance 1.
                match super::fuzzy::plan_fuzzy_fallback(
                    body,
                    super::fuzzy::FuzzyPlan {
                        pattern: &patterns[0],
                        replacement: &replacements[0],
                        max_distance: 1,
                    },
                    bom_len,
                    planned,
                    replacement_bytes,
                    cancel,
                ) {
                    Ok(()) => {
                        notes.push(format!(
                            "literal pattern not found; applied a unique fuzzy match at distance 1: {}",
                            snippet(&patterns[0])
                        ));
                        return Ok(());
                    }
                    // A cancellation is not a miss: the fuzzy scan aborted
                    // because the user stopped the call, and "pattern not
                    // found" would misreport it to the model.
                    Err(error) if cancel.is_cancelled() => return Err(error),
                    Err(_) => {}
                }
            }
            if matches.is_empty() {
                return Err(literal_miss_error(body, patterns));
            }
            // Deterministic positional order for `nth` picks and for the
            // overlap rejection that follows.
            matches.sort_by_key(|found| (found.start, found.end));
            reject_overlapping_candidates(&matches)?;
            let newline = dominant_newline(body);
            for found in select_matches(matches, *pick, "literal")? {
                let replacement = match_newlines(&replacements[found.pattern_id], newline);
                reserve_planned(
                    planned,
                    replacement_bytes,
                    found.start + bom_len,
                    found.end + bom_len,
                    &replacement,
                )?;
            }
            Ok(())
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
            planned,
            replacement_bytes,
        ),
        PreparedOp::LineRange {
            start_line,
            end_line,
            expected_text,
            expected_hash,
            replacement,
        } => {
            let range = match super::line::line_range_planned(
                body,
                *start_line,
                *end_line,
                expected_text.as_deref(),
                expected_hash.as_deref(),
                replacement,
                bom_len,
            ) {
                Ok(range) => range,
                Err(error) => {
                    let Some(expected) =
                        expected_text.as_deref().filter(|_| expected_hash.is_none())
                    else {
                        return Err(error);
                    };
                    let Some(range) = super::line::planned_for_expected_text(
                        body,
                        expected,
                        replacement,
                        bom_len,
                    ) else {
                        return Err(error);
                    };
                    notes.push(format!(
                        "line_range {start_line}-{end_line} no longer matches; edited the unique \
                         expected_text block instead: {}",
                        snippet(expected)
                    ));
                    range
                }
            };
            reserve_planned(
                planned,
                replacement_bytes,
                range.start,
                range.end,
                &range.replacement,
            )
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
            planned,
            replacement_bytes,
            cancel,
        ),
    }
}

/// Tags a planning failure with its operation index in a batch.
///
/// A multi-operation call is atomic — one miss discards every planned
/// change. Without the index and that fact, the model cannot tell which op
/// failed and may keep editing as if the earlier ops had landed.
fn tag_op_error(index: usize, total: usize, error: ToolError) -> ToolError {
    if total <= 1 {
        return error;
    }
    let prefix = format!(
        "operation {} of {total} failed (the call is atomic: no changes were applied): ",
        index + 1
    );
    match error {
        ToolError::Execution(message) => ToolError::Execution(format!("{prefix}{message}")),
        ToolError::InvalidArgs(message) => ToolError::InvalidArgs(format!("{prefix}{message}")),
        other => other,
    }
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

/// Line-window match that ignores indentation, trailing spaces, and runs of
/// internal whitespace. Only used after an exact literal miss, and only for
/// one pattern. The replacement still covers the original bytes.
mod matching;
use self::matching::*;

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
    // BOM re-insertion below can add back bytes a replacement removed and is
    // bounded by the post-build length check; reserving BOM headroom here
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
mod near_miss_tests;

#[cfg(test)]
mod newline_tests;

#[cfg(test)]
mod normalize_tests;

#[cfg(test)]
mod op_index_tests;
