//! Shared report assembly and pattern validation for the grep/find tools.
use globset::GlobMatcher;
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::builtin::fs_search::{MAX_PATTERN_BYTES, ResolvedRoot, io_incomplete_notice, rel_posix};
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

/// A compiled path glob, matched against `/`-separated relative paths.
///
/// A glob with no `/` also matches the last path component, the way
/// `.gitignore` reads it: `main.rs` finds `src/main.rs`. Matched only
/// against the whole relative path, such a name found nothing below the
/// search root.
pub(crate) struct PathGlob {
    matcher: GlobMatcher,
    any_depth: bool,
}

impl PathGlob {
    pub(crate) fn is_match(&self, relative: &str) -> bool {
        self.matcher.is_match(relative)
            || (self.any_depth
                && relative
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name != relative && self.matcher.is_match(name)))
    }
}

/// Compiles one glob, reporting failures with the tool's `label`.
///
/// A comma list with no braces (`*.rs,*.toml`) is read as the brace set
/// it means. Case folds where the host file system does.
pub(crate) fn compile_glob_labeled(pattern: &str, label: &str) -> Result<PathGlob, ToolError> {
    reject_pattern_bytes(pattern, &format!("{label} glob"))?;
    let spelled = if pattern.contains(',') && !pattern.contains('{') {
        let items: Vec<&str> = pattern
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect();
        format!("{{{}}}", items.join(","))
    } else {
        pattern.to_owned()
    };
    globset::GlobBuilder::new(&spelled)
        .case_insensitive(cfg!(any(windows, target_os = "macos")))
        .build()
        .map(|glob| PathGlob {
            matcher: glob.compile_matcher(),
            any_depth: !spelled.contains('/'),
        })
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

/// The report for an explicit `path` that is hidden or gitignored. The
/// walk would skip it, and an empty report read as "the text is not
/// there", so the model is told why and where to look instead.
pub(crate) fn skipped_target_report(tool: &str, root: &ResolvedRoot) -> ToolResult {
    let path = rel_posix(&root.cwd, &root.root);
    ToolResult::text(format!(
        "`{path}` is hidden or gitignored, so {tool} does not search it. Use `read` \
         to open a file there, or the shell tool to search it."
    ))
    .with_details(json!({
        "root": root.root.display().to_string(),
        "matches": 0,
        "skipped": true,
    }))
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
        let _ = write!(
            text,
            "[showing first {shown} of {count} {}; {}{reason}]",
            spec.noun, spec.truncated_advice
        );
    }
    // An empty body read as a failed call; say the search ran and found nothing.
    if text.is_empty() && spec.total == 0 {
        let _ = write!(text, "No {} found.", spec.noun);
    }
    for notice in &spec.extra_notices {
        text.push('\n');
        text.push_str(notice);
    }
    if output_truncated {
        let _ = write!(
            text,
            "\n[output truncated at {output_bytes} bytes; {}]",
            spec.output_advice
        );
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
