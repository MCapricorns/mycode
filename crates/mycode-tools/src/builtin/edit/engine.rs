//! Private planning and apply engine for [`super::EditTool`].
use aho_corasick::{AhoCorasick, MatchKind};
use memchr::memmem::Finder;
use regex::RegexBuilder;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    EditArgs, EditOp, MAX_CAPTURE_BYTES, MAX_DIFF_SNIPPET, MAX_DIFF_SUMMARY_BYTES,
    MAX_LITERAL_PATTERNS, MAX_MATCHES, MAX_OPERATIONS, Occurrence, REGEX_NEST_LIMIT, UTF8_BOM,
};
use crate::builtin::fs_io::MAX_WRITE_BYTES;
use crate::builtin::fs_search::{MAX_PATTERN_BYTES, REGEX_DFA_SIZE_LIMIT, REGEX_SIZE_LIMIT};
use crate::tool::{ToolError, ToolResult};

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
    text.push_str(&format!("\n[revision {revision}]"));
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
    Ast {
        language: super::ast::AstLanguage,
        query: tree_sitter::Query,
        replacement: String,
        capture: Option<String>,
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
    match (&args.operations, &args.old_string, &args.new_string) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(ToolError::InvalidArgs(
            "cannot combine old_string/new_string with operations".to_owned(),
        )),
        (Some(operations), None, None) => {
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
            let ops: Vec<PreparedOp> = operations
                .iter()
                .map(|op| prepare_op(op, &args.path))
                .collect::<Result<_, _>>()?;
            super::ast::reject_mixed_languages(&ops)?;
            Ok(ops)
        }
        (None, Some(old), Some(new)) => {
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
        (None, Some(_), None) => Err(ToolError::InvalidArgs(
            "new_string is required when old_string is set".to_owned(),
        )),
        (None, None, Some(_)) => Err(ToolError::InvalidArgs(
            "old_string is required when new_string is set".to_owned(),
        )),
        (None, None, None) => Err(ToolError::InvalidArgs(
            "provide old_string/new_string or operations".to_owned(),
        )),
    }
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

fn prepare_op(op: &EditOp, path: &str) -> Result<PreparedOp, ToolError> {
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
        EditOp::Ast {
            language,
            query,
            replacement,
            capture,
        } => super::ast::prepare(
            language.as_deref(),
            path,
            query,
            replacement,
            capture.as_deref(),
        ),
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
    ast_snapshot: Option<&super::ast::AstSnapshot>,
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
                for found in select_matches(matches, *pick, "literal")? {
                    reserve_planned(
                        &mut planned,
                        &mut replacement_bytes,
                        found.start + bom_len,
                        found.end + bom_len,
                        &replacements[found.pattern_id],
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
            PreparedOp::Ast {
                language,
                query,
                replacement,
                capture,
            } => {
                let parsed = ast_snapshot.ok_or_else(|| {
                    ToolError::Execution("ast snapshot was not prepared".to_owned())
                })?;
                if parsed.language != *language {
                    return Err(ToolError::Execution(
                        "ast snapshot language does not match the prepared operation".to_owned(),
                    ));
                }
                super::ast::plan_ast(
                    body,
                    &parsed.tree,
                    super::ast::AstPlan {
                        query,
                        replacement,
                        capture: capture.as_deref(),
                    },
                    bom_len,
                    &mut planned,
                    &mut replacement_bytes,
                    cancel,
                )?;
            }
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
    if patterns.len() == 1 {
        memmem_matches(body, &patterns[0])
    } else {
        ac_matches(body, patterns)
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
                "regex pattern occurs 2 times; include more surrounding context to make it unique"
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

fn append_diff(diff: &mut String, old: &str, new: &str) {
    if old == new || diff.len() >= MAX_DIFF_SUMMARY_BYTES {
        return;
    }
    let hunk = line_diff_preview(old, new);
    let remaining = MAX_DIFF_SUMMARY_BYTES.saturating_sub(diff.len());
    if hunk.len() > remaining {
        diff.push_str("[diff truncated]\n");
        return;
    }
    diff.push_str(&hunk);
}

/// Line-oriented preview of additions and deletions. Unchanged lines are
/// omitted so the UI can show what actually changed.
fn line_diff_preview(old: &str, new: &str) -> String {
    const MAX_SIDE_LINES: usize = 80;
    const MAX_EMITTED: usize = 40;
    const PREVIEW_LINE_BYTES: usize = 200;
    let old_lines = split_preview_lines(old);
    let new_lines = split_preview_lines(new);
    if old_lines.len() > MAX_SIDE_LINES || new_lines.len() > MAX_SIDE_LINES {
        return truncated_sides(&old_lines, &new_lines, PREVIEW_LINE_BYTES, MAX_EMITTED);
    }
    let mut out = String::new();
    for (emitted, (sign, line)) in changed_lines(&old_lines, &new_lines)
        .into_iter()
        .enumerate()
    {
        if emitted >= MAX_EMITTED {
            out.push_str("[diff truncated]\n");
            break;
        }
        push_preview_line(&mut out, sign, line, PREVIEW_LINE_BYTES);
    }
    out
}

fn split_preview_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn changed_lines<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1].saturating_add(1)
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(('-', old[i]));
            i += 1;
        } else {
            out.push(('+', new[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(('-', old[i]));
        i += 1;
    }
    while j < m {
        out.push(('+', new[j]));
        j += 1;
    }
    out
}

fn truncated_sides(old: &[&str], new: &[&str], line_bytes: usize, max_emitted: usize) -> String {
    let old_take = max_emitted / 2;
    let shown = old.len().min(old_take);
    let mut out = String::new();
    for line in old.iter().take(old_take) {
        push_preview_line(&mut out, '-', line, line_bytes);
    }
    for line in new.iter().take(max_emitted.saturating_sub(shown)) {
        push_preview_line(&mut out, '+', line, line_bytes);
    }
    out.push_str("[diff truncated]\n");
    out
}

fn push_preview_line(out: &mut String, sign: char, line: &str, line_bytes: usize) {
    let (cut, truncated) = crate::builtin::truncate_bytes(line, line_bytes);
    out.push(sign);
    out.push(' ');
    out.push_str(&cut.replace('\r', "\\r"));
    if truncated {
        out.push('\u{2026}');
    }
    out.push('\n');
}

pub(super) fn snippet(text: &str) -> String {
    let (cut, truncated) = crate::builtin::truncate_bytes(text, MAX_DIFF_SNIPPET);
    let mut visible = cut.replace('\r', "\\r").replace('\n', "\\n");
    if truncated {
        visible.push('\u{2026}');
    }
    visible
}
