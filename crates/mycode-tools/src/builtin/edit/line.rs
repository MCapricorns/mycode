//! Shared line-boundary and line-range primitives for edit operations.
use super::engine::Planned;
use crate::tool::ToolError;

pub(super) fn line_range_planned(
    body: &str,
    start_line: usize,
    end_line: usize,
    expected_text: Option<&str>,
    expected_hash: Option<&str>,
    replacement: &str,
    bom_len: usize,
) -> Result<Planned, ToolError> {
    let (start, end) = locate_line_range(body, start_line, end_line)?;
    let range = &body[start..end];
    if let Some(expected) = expected_text
        && range != expected
    {
        return Err(ToolError::Execution(
            "line range does not match expected_text; re-read the file".to_owned(),
        ));
    }
    if let Some(expected) = expected_hash {
        let actual = blake3::hash(range.as_bytes()).to_hex();
        if !hash_eq(expected, actual.as_str()) {
            return Err(ToolError::Execution(
                "line range does not match expected_hash; re-read the file".to_owned(),
            ));
        }
    }
    Ok(Planned {
        start: start + bom_len,
        end: end + bom_len,
        replacement: replacement.to_owned(),
    })
}

/// Returns the CR, LF, or CRLF terminator length at `index`.
///
/// CRLF is one terminator. Fuzzy near-miss line numbers and `line_range`
/// byte ranges both call this so they cannot drift. Changing it changes
/// both diagnostics and selected ranges.
pub(super) fn line_terminator_len(bytes: &[u8], index: usize) -> usize {
    match bytes.get(index).copied() {
        Some(b'\r') if index + 1 < bytes.len() && bytes[index + 1] == b'\n' => 2,
        Some(b'\r' | b'\n') => 1,
        _ => 0,
    }
}

/// Returns the 1-based line and terminator-free content range of `offset`.
///
/// `offset` is clamped to `text.len()`. The exclusive range never includes
/// CR, LF, or CRLF, so a preview cannot span lines.
pub(super) fn line_location(text: &str, offset: usize) -> (usize, usize, usize) {
    let bytes = text.as_bytes();
    let offset = offset.min(bytes.len());
    let mut line_no = 1usize;
    let mut line_start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let term_len = line_terminator_len(bytes, index);
        if term_len == 0 {
            index += 1;
            continue;
        }
        if offset < index + term_len {
            return (line_no, line_start, index);
        }
        index += term_len;
        line_no += 1;
        line_start = index;
    }
    (line_no, line_start, bytes.len())
}

/// Locates the byte range of lines `start_line..=end_line` (1-based) in one
/// linear scan.
///
/// Line terminators belong to their line; `\r\n` counts once, and a final
/// chunk without a terminator counts as its own line (an empty file is one
/// empty line). Only the target range bounds and the total line count are
/// retained, never a span per line, so a maximum-size all-newline file stays
/// constant-memory here.
fn locate_line_range(
    text: &str,
    start_line: usize,
    end_line: usize,
) -> Result<(usize, usize), ToolError> {
    let bytes = text.as_bytes();
    let mut target_start: Option<usize> = None;
    let mut target_end: Option<usize> = None;
    let mut line_no = 0usize;
    let mut line_start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let term_len = line_terminator_len(bytes, index);
        if term_len > 0 {
            line_no += 1;
            if line_no == start_line {
                target_start = Some(line_start);
            }
            if line_no == end_line {
                target_end = Some(index + term_len);
            }
            index += term_len;
            line_start = index;
        } else {
            index += 1;
        }
    }
    let has_partial = line_start < bytes.len();
    if has_partial {
        if line_no + 1 == start_line {
            target_start = Some(line_start);
        }
        if line_no + 1 == end_line {
            target_end = Some(bytes.len());
        }
    }
    if bytes.is_empty() {
        // An empty file is one empty line, so only the 1-1 range is valid
        // and it selects the zero-length body at offset 0.
        if start_line == 1 && end_line == 1 {
            return Ok((0, 0));
        }
    }
    let total_lines = if bytes.is_empty() {
        1
    } else {
        line_no + usize::from(has_partial)
    };
    match (target_start, target_end) {
        (Some(start), Some(end)) if start <= end => Ok((start, end)),
        _ => Err(ToolError::Execution(format!(
            "line_range {start_line}-{end_line} is outside the file ({total_lines} lines)"
        ))),
    }
}

fn hash_eq(expected: &str, actual_hex: &str) -> bool {
    let expected = expected.strip_prefix("blake3:").unwrap_or(expected).trim();
    expected.eq_ignore_ascii_case(actual_hex)
}
