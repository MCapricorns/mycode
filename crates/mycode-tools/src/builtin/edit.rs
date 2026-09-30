//! `edit` — host-owned atomic editor on the file kernel.
//!
//! One bounded snapshot read, one result allocation, one kernel publish
//! using the snapshot revision. Matches are collected on that snapshot,
//! sorted by byte range, and rejected on ambiguity, UTF-8 boundary errors,
//! overlap, or empty search matches. `fuzzy` commits only a unique-best
//! normalized match; `ast` replaces tree-sitter capture ranges only.
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::builtin::fs_io::{
    FileAccess, MAX_WRITE_BYTES, read_file_snapshot_async, write_file_with_lease,
};
use crate::builtin::process::acquire_execution_lease;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum operations in one edit call.
///
/// Batch edits stay small so planning stays linear in file size. Raising
/// this also raises worst-case match storage.
pub(super) const MAX_OPERATIONS: usize = 32;
/// Maximum literal patterns in one Aho-Corasick operation.
///
/// Keeps automaton construction bounded independently of file size.
pub(super) const MAX_LITERAL_PATTERNS: usize = 32;
/// Maximum matches collected across every operation.
///
/// Caps planned-replacement storage; a whole-file per-character rewrite
/// must fail closed instead of allocating per byte.
pub(super) const MAX_MATCHES: usize = 32_768;
/// Maximum expanded capture replacement per match, in bytes.
///
/// Prevents `$0`-style templates from materializing a huge string from
/// one match.
pub(super) const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
/// Regex nesting cap passed to `RegexBuilder`.
///
/// Lower than the crate default so pathological nesting fails at compile
/// instead of during search.
pub(super) const REGEX_NEST_LIMIT: u32 = 32;
/// Maximum characters of a single diff side.
///
/// Keeps the UI summary small so it cannot echo a secret file.
pub(super) const MAX_DIFF_SNIPPET: usize = 80;
/// Maximum bytes of the aggregated diff summary.
///
/// UI-only; never sent as the whole file.
pub(super) const MAX_DIFF_SUMMARY_BYTES: usize = 4096;
/// UTF-8 byte-order mark preserved across snapshot apply.
pub(super) const UTF8_BOM: &str = "\u{feff}";

/// The `edit` builtin.
pub struct EditTool;

/// Arguments for [`EditTool`].
///
/// The legacy `{path, old_string, new_string}` unique replace is accepted
/// and treated as one literal unique operation.
#[derive(Deserialize, JsonSchema)]
pub struct EditArgs {
    /// Path of the file to edit. Relative paths resolve against the
    /// session cwd.
    pub path: String,
    /// Opaque revision from a prior `read`. When set it must match the
    /// snapshot; the publish always compare-and-swaps the snapshot
    /// revision.
    pub expected_revision: Option<String>,
    /// Exact text to replace once. Legacy unique-replace field; cannot be
    /// combined with `operations`.
    pub old_string: Option<String>,
    /// Replacement for `old_string`.
    pub new_string: Option<String>,
    /// Bounded batch of literal, regex, line-range, fuzzy, and ast operations.
    pub operations: Option<Vec<EditOp>>,
}

impl std::fmt::Debug for EditArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditArgs")
            .field("path", &self.path)
            .field("expected_revision", &self.expected_revision)
            .field(
                "old_string",
                &self.old_string.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "new_string",
                &self.new_string.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "operations",
                &self.operations.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Which matches of a search operation to replace.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Occurrence {
    /// Replace exactly one match; zero or two-plus is an error.
    #[default]
    Unique,
    /// Replace every non-overlapping match; zero matches is an error.
    All,
    /// Replace the 1-based `n`th match.
    Nth,
}

/// One bounded edit operation against a single snapshot.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOp {
    /// Literal search: one pattern uses memmem; several use Aho-Corasick.
    Literal {
        /// Single needle. Mutually exclusive with `patterns`.
        pattern: Option<String>,
        /// Replacement for `pattern`.
        replacement: Option<String>,
        /// Multiple needles. Mutually exclusive with `pattern`.
        patterns: Option<Vec<String>>,
        /// Replacements parallel to `patterns`.
        replacements: Option<Vec<String>>,
        /// Which matches to take. Defaults to unique.
        #[serde(default)]
        occurrence: Occurrence,
        /// 1-based index when `occurrence` is `nth`.
        n: Option<u32>,
    },
    /// Bounded Rust regex (`unique` or `all`) with capture replacement.
    Regex {
        /// Pattern compiled by the Rust `regex` crate (no backtracking).
        pattern: String,
        /// Replacement; `$1` / `$name` expand from captures.
        replacement: String,
        /// Which matches to take. `nth` is rejected.
        #[serde(default)]
        occurrence: Occurrence,
        /// Unused; present so a mistaken `n` can be rejected.
        n: Option<u32>,
    },
    /// Unique-best fuzzy replace after token/whitespace normalization.
    ///
    /// Not an occurrence flag on `literal` and never falls back to exact
    /// memmem. `max_distance` is required and must be 1, 2, or 3.
    Fuzzy {
        /// Needle tokenized the same way as the file (whitespace collapsed).
        pattern: String,
        /// Replacement for the unique best original byte range.
        replacement: String,
        /// Inclusive character Levenshtein cap on the normalized form.
        max_distance: u32,
    },
    /// Tree-sitter capture replace. Never pretty-prints the whole file.
    Ast {
        /// Grammar name (`rust`, `python`, …). Inferred from the path when omitted.
        language: Option<String>,
        /// One bounded tree-sitter query with portable ASCII named captures.
        query: String,
        /// Replacement for the capture range; `@name` expands from the match
        /// and `@@` emits one literal `@`.
        replacement: String,
        /// Portable ASCII capture to replace. When omitted, every capture is replaced.
        capture: Option<String>,
    },
    /// Inclusive 1-based line range with expected old text and/or hash.
    LineRange {
        /// First line to replace (1-based, BOM-stripped).
        start_line: usize,
        /// Last line to replace (inclusive).
        end_line: usize,
        /// Exact bytes of the selected range.
        expected_text: Option<String>,
        /// Blake3 hex of the selected range.
        expected_hash: Option<String>,
        /// Replacement for the range (may be empty).
        replacement: String,
    },
}

#[async_trait]
impl Tool for EditTool {
    type Args = EditArgs;
    type Output = ();

    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Atomically edit an existing UTF-8 file. Prefer a unique literal \
         replace (`old_string`/`new_string`, or `operations` with type \
         `literal`). Batch literal (memmem / Aho-Corasick), fuzzy \
         (unique-best normalized Levenshtein), bounded regex, line-range, \
         and ast (tree-sitter capture) ops all match one snapshot and \
         publish once. `old_string` must match exactly once. Hidden files \
         are editable. Returns a revision and a bounded diff summary, not \
         the file body."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "edit: unique string replace (path, old_string, new_string) or \
             operations[] (literal/regex/line_range/fuzzy/ast) with optional \
             expected_revision.",
        )
    }

    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingContent)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let prepare_cancel = ctx.cancel.clone();
        let (args, ops) = tokio::task::spawn_blocking(move || {
            check_cancel(&prepare_cancel)?;
            let ops = normalize_args(&args)?;
            check_cancel(&prepare_cancel)?;
            Ok::<_, ToolError>((args, ops))
        })
        .await
        .map_err(|error| {
            ToolError::Execution(format!("edit preparation worker failed: {error}"))
        })??;
        let snapshot = read_file_snapshot_async(
            ctx.prepared_file.clone(),
            ctx.cwd.clone(),
            args.path.clone(),
            ctx.cancel.clone(),
        )
        .await?;
        if snapshot.text.len() > MAX_WRITE_BYTES {
            return Err(ToolError::Execution(format!(
                "file exceeds {MAX_WRITE_BYTES} bytes and cannot be edited"
            )));
        }
        if let Some(expected) = args.expected_revision.as_deref()
            && expected != snapshot.revision.as_str()
        {
            return Err(ToolError::Execution(
                "stale expected_revision; re-read the file and retry".to_owned(),
            ));
        }
        check_cancel(&ctx.cancel)?;
        let planning_cancel = ctx.cancel.clone();
        let (snapshot, applied) = tokio::task::spawn_blocking(move || {
            check_cancel(&planning_cancel)?;
            let ast_snapshot = ast::parse_snapshot(&ops, &snapshot.text, &planning_cancel)?;
            let planned = plan_edits(
                &snapshot.text,
                &ops,
                ast_snapshot.as_ref(),
                &planning_cancel,
            )?;
            let applied = apply_planned(&snapshot.text, &planned, &planning_cancel)?;
            ast::reject_new_syntax_errors(
                ast_snapshot.as_ref(),
                &snapshot.text,
                &applied.text,
                &planned,
                &planning_cancel,
            )?;
            check_cancel(&planning_cancel)?;
            Ok::<_, ToolError>((snapshot, applied))
        })
        .await
        .map_err(|error| ToolError::Execution(format!("edit planning worker failed: {error}")))??;
        if applied.bytes_after > MAX_WRITE_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "edited content exceeds {MAX_WRITE_BYTES} bytes"
            )));
        }
        check_cancel(&ctx.cancel)?;
        if applied.text == snapshot.text {
            return Ok(edit_result(
                &snapshot.path_key,
                snapshot.revision.as_str(),
                &applied,
                false,
            ));
        }
        // Host-controlled mutation starts at publish. Planning has no publish
        // side effects, so the process-wide execution lease is taken only after
        // a real changed payload exists.
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                return Err(ToolError::Execution("edit cancelled before execution".into()));
            }
            lease = acquire_execution_lease() => lease,
        };
        let outcome = write_file_with_lease(
            None,
            ctx.cwd.clone(),
            args.path,
            applied.text.clone(),
            Some(snapshot.revision.as_str().to_owned()),
            false,
            lease,
            ctx.cancel.clone(),
        )
        .await?;
        Ok(edit_result(
            &outcome.path_key,
            outcome.revision.as_str(),
            &applied,
            outcome.detached_hardlink,
        ))
    }
}

mod ast;
mod engine;
mod fuzzy;
mod line;
use engine::*;
