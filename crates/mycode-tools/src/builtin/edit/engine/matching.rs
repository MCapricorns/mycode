//! Literal, whitespace-tolerant, and regex match finding for one edit plan.

use super::*;
pub(super) fn relaxed_literal_matches(body: &str, pattern: &str) -> Vec<Found> {
    let include_terminator = pattern.ends_with('\n');
    let mut needle: Vec<String> = pattern.split('\n').map(normalize_line).collect();
    if include_terminator {
        needle.pop();
    }
    while needle.last().is_some_and(String::is_empty) {
        needle.pop();
    }
    if needle.is_empty() || needle.iter().all(String::is_empty) {
        return Vec::new();
    }
    let mut lines: Vec<(usize, usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let content = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line));
        lines.push((start, line.len(), content));
    }
    if lines.is_empty() {
        return Vec::new();
    }
    let width = needle.len();
    let mut found = Vec::new();
    for first in 0..lines.len() {
        let last = first + width - 1;
        if last >= lines.len() {
            break;
        }
        let matches = needle
            .iter()
            .enumerate()
            .all(|(index, wanted)| normalize_line(lines[first + index].2) == *wanted);
        if !matches {
            continue;
        }
        let start = lines[first].0;
        let mut end = lines[last].0 + lines[last].1;
        if !include_terminator {
            let content_len = lines[last].2.len();
            end = lines[last].0 + content_len;
        }
        found.push(Found {
            start,
            end,
            pattern_id: 0,
        });
    }
    // Overlapping windows mean the relaxed match is not a unique block; only
    // keep non-overlapping candidates so a repeated line cannot fan out.
    found.dedup_by(|next, previous| next.start < previous.end);
    found
}

pub(super) fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn literal_matches(body: &str, patterns: &[String]) -> Result<Vec<Found>, ToolError> {
    let mut exact = if patterns.len() == 1 {
        memmem_matches(body, &patterns[0])?
    } else {
        ac_matches(body, patterns)?
    };
    if !exact.is_empty() {
        // A pattern that opens with LF also matches inside a CRLF. The CR
        // belongs to the same newline, so the match takes it too; otherwise
        // the replacement leaves it behind as a stray CR.
        for found in &mut exact {
            if cr_before_lf(body, found.start) {
                found.start -= 1;
            }
        }
        return Ok(exact);
    }
    // Windows files are often CRLF while the model sends LF (or the reverse).
    // An exact miss retries on a copy with the CR of each CRLF dropped, and
    // the matched span is mapped back onto the original bytes. The
    // replacement is then rewritten to the file's dominant line ending.
    if body.contains('\r') || patterns.iter().any(|pattern| pattern.contains('\r')) {
        let found = newline_insensitive_matches(body, patterns, false)?;
        if !found.is_empty() {
            return Ok(found);
        }
    }
    // A model's copy of a line almost never keeps the spaces and tabs the file
    // has before its newline. The last tier also ignores those; indentation and
    // every other byte still have to match.
    newline_insensitive_matches(body, patterns, true)
}

pub(super) struct NormalizedNewlines {
    text: String,
    /// Original byte index of each byte in [`Self::text`].
    map: Vec<usize>,
}

/// Drops the CR of each CRLF and, with `strip_trailing`, the spaces and
/// tabs right before a newline.
pub(super) fn normalize_newlines(text: &str, strip_trailing: bool) -> NormalizedNewlines {
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 1;
            continue;
        }
        if strip_trailing && matches!(bytes[index], b' ' | b'\t') {
            let run_end = index
                + bytes[index..]
                    .iter()
                    .take_while(|byte| matches!(byte, b' ' | b'\t'))
                    .count();
            let at_line_end = match bytes.get(run_end) {
                Some(b'\n') => true,
                Some(b'\r') => bytes.get(run_end + 1) == Some(&b'\n'),
                _ => false,
            };
            if at_line_end {
                index = run_end;
                continue;
            }
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

pub(super) fn newline_insensitive_matches(
    body: &str,
    patterns: &[String],
    strip_trailing: bool,
) -> Result<Vec<Found>, ToolError> {
    let norm_body = normalize_newlines(body, strip_trailing);
    let norm_patterns: Vec<String> = patterns
        .iter()
        .map(|pattern| normalize_newlines(pattern, strip_trailing).text)
        .collect();
    if norm_patterns.iter().any(String::is_empty) {
        return Ok(Vec::new());
    }
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

pub(super) fn map_newline_span(
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

pub(super) fn cr_before_lf(body: &str, index: usize) -> bool {
    index > 0
        && body.as_bytes().get(index) == Some(&b'\n')
        && body.as_bytes().get(index - 1) == Some(&b'\r')
}

/// The line ending the file already uses, when it has any newlines.
pub(super) fn dominant_newline(body: &str) -> Option<&'static str> {
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
pub(super) fn match_newlines(text: &str, newline: Option<&str>) -> String {
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

/// The miss error for a literal op. When a whitespace-insensitive scan
/// still finds the pattern — the usual cause is different indentation —
/// the hint names the line so the model can correct the pattern instead
/// of re-reading the whole file.
pub(super) fn literal_miss_error(body: &str, patterns: &[String]) -> ToolError {
    const BASE: &str = "literal pattern not found; re-read the file and provide the exact text";
    let span = patterns
        .iter()
        .map(|pattern| pattern.lines().count())
        .max()
        .unwrap_or(1);
    if let Some(line) = whitespace_near_miss_line(body, patterns) {
        return ToolError::Execution(format!(
            "{BASE}; line {line} contains similar text that differs only in \
             whitespace/indentation. The file there reads:\n{}",
            excerpt(body, line, span + 2)
        ));
    }
    // A pattern whose opening line is in the file but whose later lines
    // are not is usually a stale or misremembered copy. Showing the real
    // lines lets the model fix the pattern without re-reading the file.
    if let Some(line) = first_line_anchor(body, patterns) {
        return ToolError::Execution(format!(
            "{BASE}; the pattern's first line matches line {line}, but the lines after \
             it differ. The file there reads:\n{}",
            excerpt(body, line, span)
        ));
    }
    ToolError::Execution(BASE.to_owned())
}

/// The first body line equal to some pattern's first non-blank line, both
/// trimmed. Short lines (`}`, `else {`) are too common to point anywhere.
pub(super) fn first_line_anchor(body: &str, patterns: &[String]) -> Option<usize> {
    const MIN_ANCHOR_CHARS: usize = 12;
    patterns
        .iter()
        .filter(|pattern| pattern.lines().count() > 1)
        .filter_map(|pattern| pattern.lines().map(str::trim).find(|line| !line.is_empty()))
        .filter(|first| first.chars().count() >= MIN_ANCHOR_CHARS)
        .filter_map(|first| {
            body.lines()
                .position(|line| line.trim() == first)
                .map(|index| index + 1)
        })
        .min()
}

/// Up to `span` body lines from 1-based `line`, bounded so a hint cannot
/// grow into a file dump.
pub(super) fn excerpt(body: &str, line: usize, span: usize) -> String {
    const MAX_LINES: usize = 12;
    const MAX_BYTES: usize = 1200;
    let lines: Vec<&str> = body
        .lines()
        .skip(line.saturating_sub(1))
        .take(span.clamp(1, MAX_LINES))
        .collect();
    let text = lines.join("\n");
    let (mut text, cut) = crate::builtin::truncate_bytes(&text, MAX_BYTES);
    if cut {
        text.push_str("\n[excerpt truncated]");
    }
    text
}

pub(super) fn strip_whitespace(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// Diagnostic-only scan for a literal op that missed. Finds the earliest
/// body position where a pattern appears once every whitespace character is
/// dropped, so different indentation or line wrapping still points the
/// model at the right spot. Matching itself stays exact — this only
/// enriches the error.
pub(super) fn whitespace_near_miss_line(body: &str, patterns: &[String]) -> Option<usize> {
    let stripped_body = strip_whitespace(body);
    if stripped_body.is_empty() {
        return None;
    }
    let best = patterns
        .iter()
        .map(|pattern| strip_whitespace(pattern))
        .filter(|needle| !needle.is_empty())
        .filter_map(|needle| memchr::memmem::find(stripped_body.as_bytes(), needle.as_bytes()))
        .min()?;
    // Walk the body once to map the stripped offset back to its line,
    // instead of keeping an index entry for every body byte.
    let mut kept = 0usize;
    let mut line = 1usize;
    for ch in body.chars() {
        if ch.is_whitespace() {
            if ch == '\n' {
                line += 1;
            }
            continue;
        }
        if kept >= best {
            break;
        }
        kept += ch.len_utf8();
    }
    Some(line)
}

pub(super) fn memmem_matches(body: &str, pattern: &str) -> Result<Vec<Found>, ToolError> {
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

pub(super) fn ac_matches(body: &str, patterns: &[String]) -> Result<Vec<Found>, ToolError> {
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

pub(super) fn select_matches(matches: Vec<Found>, pick: Pick, label: &str) -> Result<Vec<Found>, ToolError> {
    match pick {
        Pick::Unique => match matches.len() {
            0 => Err(ToolError::Execution(format!(
                "{label} pattern not found; re-read the file and provide the exact text"
            ))),
            1 => Ok(matches),
            n => Err(ToolError::Execution(format!(
                "{label} pattern occurs {n} times; include more surrounding lines to make it unique, or set replace_all (`occurrence: \"all\"` in an operation) to change every one"
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
