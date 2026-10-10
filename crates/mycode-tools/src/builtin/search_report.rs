//! Shared report assembly and pattern validation for the grep/find tools.
use globset::GlobMatcher;

use serde_json::{Value, json};

use crate::builtin::fs_search::{MAX_PATTERN_BYTES, io_incomplete_notice};
use crate::tool::{ToolError, ToolResult};

/// Rejects NUL bytes and oversized patterns/globs.
pub(crate) fn reject_pattern_bytes(pattern: &str, label: &str) -> Result<(), ToolError> {
    if pattern.as_bytes().contains(&0) {
        return Err(ToolError::InvalidArgs(format!(
            "{label} contains a NUL byte"
        )));
    }
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "{label} exceeds {MAX_PATTERN_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Compiles one glob, reporting failures with the tool's `label`.
pub(crate) fn compile_glob_labeled(pattern: &str, label: &str) -> Result<GlobMatcher, ToolError> {
    reject_pattern_bytes(pattern, &format!("{label} glob"))?;
    globset::Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|error| {
            ToolError::InvalidArgs(format!("invalid {label} glob `{pattern}`: {error}"))
        })
}

/// Inputs to [`render_report`].
pub(crate) struct ReportSpec<'a> {
    /// Sorted, already-rendered result entries.
    pub entries: Vec<String>,
    /// Committed total discovered (may exceed `entries.len()`).
    pub total: u64,
    /// Why the walk stopped early, if it did.
    pub stop_reason: Option<&'static str>,
    /// `(count, samples)` from the shared I/O-error collector.
    pub io: (u64, Vec<String>),
    /// Noun in the truncation notice, e.g. `"matching paths"`.
    pub noun: &'a str,
    /// Advice in the truncation notice, e.g. `"refine the pattern or raise limit"`.
    pub truncated_advice: &'a str,
    /// Advice in the output-cap notice, e.g. `"refine the pattern or lower limit"`.
    pub output_advice: &'a str,
    /// Pre-rendered extra notices appended before the output-cap notice.
    pub extra_notices: Vec<String>,
    /// Extra details merged into the JSON details object.
    pub extra_details: Vec<(&'a str, Value)>,
    /// Result ceiling asserted by the debug checks.
    pub cap: usize,
}

/// Renders the sorted entries under the shared output cap and assembles the
/// common truncation, I/O, and JSON-details report shape used by both tools.
pub(crate) fn render_report(
    root: &std::path::Path,
    output_bytes: usize,
    spec: ReportSpec<'_>,
) -> ToolResult {
    let mut text = String::new();
    let mut shown = 0usize;
    let mut output_truncated = false;
    for entry in &spec.entries {
        if text.len() + entry.len() + 1 > output_bytes {
            output_truncated = true;
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(entry);
        shown += 1;
    }

    let (io_count, io_samples) = spec.io;
    let exact = spec.stop_reason.is_none() && io_count == 0;
    let truncated = spec.total > shown as u64 || !exact;
    if truncated {
        let count = if exact {
            spec.total.to_string()
        } else {
            format!("at least {}", spec.total)
        };
        let reason = spec
            .stop_reason
            .map(|reason| format!("; stopped early: {reason}"))
            .unwrap_or_default();
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "[showing first {shown} of {count} {}; {}{reason}]",
            spec.noun, spec.truncated_advice
        ));
    }
    for notice in &spec.extra_notices {
        text.push('\n');
        text.push_str(notice);
    }
    if output_truncated {
        text.push_str(&format!(
            "\n[output truncated at {output_bytes} bytes; {}]",
            spec.output_advice
        ));
    }

    if let Some(notice) = io_incomplete_notice(io_count) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&notice);
    }

    let mut details = json!({
        "root": root.display().to_string(),
        "matches": spec.total,
        "shown": shown,
        "truncated": truncated,
    });
    for (key, value) in spec.extra_details {
        details[key] = value;
    }
    if !exact {
        details["matches_lower_bound"] = json!(true);
    }
    if let Some(reason) = spec.stop_reason {
        details["stopped_early"] = json!(reason);
    }
    if output_truncated {
        details["output_truncated"] = json!(true);
    }
    if io_count > 0 {
        details["io_error_count"] = json!(io_count);
        details["io_errors"] = json!(io_samples);
    }

    debug_assert!(spec.entries.len() <= spec.cap);
    debug_assert!(shown <= spec.cap);
    ToolResult::text(text).with_details(details)
}
