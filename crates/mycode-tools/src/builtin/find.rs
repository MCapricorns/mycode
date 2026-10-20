//! `find` — secure in-process file and directory discovery.
//!
//! A handle-relative walker supplies candidate names. Explicit and walked
//! regular files retain metadata-only capabilities, so discovery does not
//! require content-read access. Directories are reopened for content only
//! after metadata/content identities match. Current Windows hidden bits are
//! re-read before confirmation, reporting, and descent. Symlinks, reparse
//! points, and uncertain ignore boundaries fail closed. Ordinary per-path
//! I/O keeps confirmed results but adds a model-visible incomplete lower-bound
//! notice. Output uses `/`; no external `fd` executable is used. Cancellation
//! or future drop is supervised until the worker is interrupted and joined.
use parking_lot::Mutex;
use std::collections::BinaryHeap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{INTERRUPTED_NOTE, Tool, ToolError, ToolResult};

use super::blocking::run_blocking_until;
use super::fs_search::{
    FsEntryKind, IO_ERROR_SAMPLES, IoErrors, Limits, PathOrderKey, ResolvedRoot, SearchAccess,
    WalkLimiter, bind_search_root_with_access, is_hidden_skip, rel_posix, stop_reason_error,
    to_posix, walk_retained_tree,
};

use super::search_report::{
    PathGlob, ReportSpec, compile_glob_labeled, reject_pattern_bytes, render_report,
    skipped_target_report,
};

/// Default cap on reported paths.
pub const DEFAULT_LIMIT: usize = 1000;

/// The `find` builtin.
#[derive(Debug)]
pub struct FindTool;

/// Arguments for [`FindTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindArgs {
    /// Glob matched against each path relative to the search root.
    pub pattern: String,
    /// Directory or single file to search. Relative paths resolve against the
    /// session cwd; absolute paths may point anywhere.
    pub path: Option<String>,
    /// Maximum number of paths to report.
    pub limit: Option<usize>,
    /// Deepest level reported, counted from the search root (`1` is its direct
    /// children). A directory at the cap is reported but not descended.
    /// Omitted searches every level.
    pub max_depth: Option<usize>,
}

struct FindState {
    heap: Mutex<BinaryHeap<PathOrderKey>>,
    total: AtomicU64,
    io_errors: IoErrors,
    limiter: Arc<WalkLimiter>,
}

impl FindState {
    fn new(limiter: Arc<WalkLimiter>) -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            total: AtomicU64::new(0),
            io_errors: IoErrors::new(IO_ERROR_SAMPLES),
            limiter,
        }
    }
}

fn offer(
    heap: &mut BinaryHeap<PathOrderKey>,
    limiter: &WalkLimiter,
    limit: usize,
    path: PathOrderKey,
) {
    let bytes = path.store_bytes();
    if !limiter.try_reserve_result_bytes(bytes) {
        return;
    }
    if heap.len() < limit {
        heap.push(path);
    } else if heap.peek().is_some_and(|worst| &path < worst) {
        if let Some(evicted) = heap.pop() {
            limiter.release_result_bytes(evicted.store_bytes());
        }
        heap.push(path);
    } else {
        limiter.release_result_bytes(bytes);
    }
}

impl Tool for FindTool {
    type Args = FindArgs;
    type Output = ();

    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Find files and directories by glob pattern under a directory (default: \
         the session cwd). `*` also matches across `/`. To list one directory, \
         pass it as `path` with pattern `*` and `max_depth` 1. Respects \
         .gitignore; hidden and gitignored paths are skipped. A pattern without \
         `/` also matches file names at any depth (`main.rs` finds \
         `src/main.rs`). Reports up to \
         `limit` (default 1000) matching paths relative to the search root, \
         sorted, using `/` separators; directories end with `/`. A notice \
         follows when more matches exist. Cancellation or the wall-clock limit \
         returns an execution error rather than a partial report."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "find: list or locate files and directories by glob (pattern, optional \
             path/limit/max_depth); pattern `*` with max_depth 1 lists one directory.",
        )
    }

    fn search_access(&self) -> Option<SearchAccess> {
        Some(SearchAccess::Metadata)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        reject_pattern_bytes(&args.pattern, "pattern")?;
        if args.max_depth == Some(0) {
            return Err(ToolError::InvalidArgs(
                "max_depth must be at least 1".to_owned(),
            ));
        }
        let max_depth = args.max_depth;
        let cwd = ctx.cwd.clone();
        let path = args.path;
        let pattern = args.pattern;
        let limit = args.limit;
        let deadline = Instant::now() + Limits::default().time_limit;
        let limits = Limits {
            deadline: Some(deadline),
            ..Limits::default()
        };
        let cancel = ctx.cancel.clone();
        let prepared = ctx.prepared_search.clone();
        run_blocking_until("find", &cancel, deadline, move |worker_cancel| {
            if worker_cancel.is_cancelled() {
                return Err(ToolError::Execution(format!(
                    "find cancelled before completion{INTERRUPTED_NOTE}"
                )));
            }
            let glob = compile_find_glob(&pattern)?;
            let root = bind_search_root_with_access(
                prepared.as_deref(),
                &cwd,
                path.as_deref(),
                &worker_cancel,
                &limits,
                SearchAccess::Metadata,
            )?;
            run_find(glob, root, limit, max_depth, &worker_cancel, &limits)
        })
        .await
    }
}

fn run_find(
    glob: PathGlob,
    root: ResolvedRoot,
    limit: Option<usize>,
    max_depth: Option<usize>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT).min(limits.stored_ceiling);
    let state = Arc::new(FindState::new(Arc::clone(&root.limiter)));
    let report_root = root.root.clone();

    match root.target_is_skipped() {
        Ok(true) => return Ok(skipped_target_report("find", &root)),
        Ok(false) => {}
        Err(error) if is_hidden_skip(&error) => {
            return Ok(skipped_target_report("find", &root));
        }
        Err(error) => {
            return Ok(ToolResult::error(format!(
                "search target hidden check failed: {error}"
            )));
        }
    }

    if root.is_file() {
        if !matches!(state.limiter.check(cancel), ignore::WalkState::Quit) {
            // rel_posix never returns an empty string (an empty suffix falls
            // back to the full posix path), so no empty-branch is needed.
            let candidate = rel_posix(&root.cwd, &root.root);
            if glob.is_match(&candidate) {
                match root.validate_target() {
                    Ok(()) => {
                        state.total.fetch_add(1, Ordering::Relaxed);
                        let mut heap = state.heap.lock();
                        offer(
                            &mut heap,
                            &state.limiter,
                            effective_limit,
                            PathOrderKey::from_rendered_and_raw(candidate, root.root.as_os_str()),
                        );
                    }
                    Err(error) => state.io_errors.record(&candidate, &error),
                }
            }
        }
        // The single-file target and allowed-root handles remain alive until
        // this report has been assembled.
        finish_find_report(&report_root, &state, effective_limit, limits)
    } else {
        if let Err(error) = walk_retained_tree(
            &root,
            &state.limiter,
            cancel,
            &state.io_errors,
            |relative_path, name, expected, parent| {
                if matches!(state.limiter.check(cancel), ignore::WalkState::Quit) {
                    return ignore::WalkState::Quit;
                }
                // An entry at the depth cap is still reported; a directory
                // there is not descended into.
                let next = if max_depth.is_some_and(|max| relative_path.components().count() >= max)
                {
                    ignore::WalkState::Skip
                } else {
                    ignore::WalkState::Continue
                };
                let relative = to_posix(relative_path);
                if !glob.is_match(&relative) {
                    return next;
                }

                // Deterministic race hook: enumeration has completed, but the
                // candidate has not yet been opened or trusted.
                if let Err(error) = root.confirm_walked(parent, name, expected) {
                    if !is_hidden_skip(&error) {
                        state.io_errors.record(&relative, &error);
                    }
                    return next;
                }
                let rendered = if expected == FsEntryKind::Directory {
                    format!("{relative}/")
                } else {
                    relative
                };
                // Report only after metadata confirmation. The enumerated
                // name is never sufficient by itself.
                state.total.fetch_add(1, Ordering::Relaxed);
                let mut heap = state.heap.lock();
                offer(
                    &mut heap,
                    &state.limiter,
                    effective_limit,
                    PathOrderKey::from_rendered_and_raw(rendered, relative_path.as_os_str()),
                );
                drop(heap);
                next
            },
        ) {
            return Ok(ToolResult::error(format!(
                "search ignore boundary could not be established: {error}"
            )));
        }
        // Report while `root` still retains both root identities.
        finish_find_report(&report_root, &state, effective_limit, limits)
    }
}

fn finish_find_report(
    root: &Path,
    state: &Arc<FindState>,
    effective_limit: usize,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    if let Some(error) = stop_reason_error("find", &state.limiter) {
        return Err(error);
    }
    Ok(find_report(root, state, effective_limit, limits))
}

fn find_report(
    root: &Path,
    state: &Arc<FindState>,
    effective_limit: usize,
    limits: &Limits,
) -> ToolResult {
    let heap = std::mem::take(&mut *state.heap.lock());
    let paths = heap.into_sorted_vec();
    let entries: Vec<String> = paths
        .iter()
        .map(|path| path.rendered().to_owned())
        .collect();

    let total = state.total.load(Ordering::Relaxed);
    let stop_reason = state.limiter.stopped_reason().or(state
        .limiter
        .result_store_truncated()
        .then_some("result store limit reached"));
    render_report(
        root,
        limits.output_bytes,
        ReportSpec {
            entries,
            total,
            stop_reason,
            io: state.io_errors.summary(),
            noun: "matching paths",
            truncated_advice: "refine the pattern or raise limit",
            output_advice: "refine the pattern or lower limit",
            extra_notices: if total == 0 {
                vec!["[hidden and gitignored paths are skipped]".to_owned()]
            } else {
                Vec::new()
            },
            extra_details: vec![("limit", json!(effective_limit))],
            cap: effective_limit,
        },
    )
}

fn compile_find_glob(pattern: &str) -> Result<PathGlob, ToolError> {
    reject_pattern_bytes(pattern, "pattern")?;
    compile_glob_labeled(pattern, "pattern")
}

#[cfg(test)]
mod tests {
    use mycode_core::message::ContentBlock;

    use super::{FindArgs, FindTool};
    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolError};

    async fn find(root: &std::path::Path, max_depth: Option<usize>) -> Result<String, ToolError> {
        find_matching(root, "*", max_depth).await
    }

    async fn find_matching(
        root: &std::path::Path,
        pattern: &str,
        max_depth: Option<usize>,
    ) -> Result<String, ToolError> {
        let ctx = ToolCtx::new(root);
        let mut out = ToolStream::closed();
        let result = FindTool
            .execute(
                FindArgs {
                    pattern: pattern.to_owned(),
                    path: None,
                    limit: None,
                    max_depth,
                },
                &ctx,
                &mut out,
            )
            .await?;
        assert!(!result.is_error, "{:?}", result.content);
        Ok(result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// A bare file name finds that file at any depth; a pattern with `/`
    /// still anchors at the search root. A miss says the search ran.
    #[tokio::test]
    async fn a_bare_name_matches_at_any_depth() {
        let root = std::env::temp_dir().join(format!(
            "mycode-find-name-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(root.join("src").join("ui")).expect("tree");
        std::fs::write(root.join("src").join("ui").join("workspace.rs"), "").expect("file");

        let listing = find_matching(&root, "workspace.rs", None)
            .await
            .expect("name");
        assert_eq!(
            listing.lines().next(),
            Some("src/ui/workspace.rs"),
            "{listing}"
        );
        let listing = find_matching(&root, "ui/*.rs", None)
            .await
            .expect("anchored");
        assert!(listing.starts_with("No matching paths found."), "{listing}");
        let listing = find_matching(&root, "src/**/*.rs", None)
            .await
            .expect("deep");
        assert!(listing.contains("src/ui/workspace.rs"), "{listing}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn max_depth_lists_one_directory_and_marks_directories() {
        let root = std::env::temp_dir().join(format!(
            "mycode-find-depth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(root.join("sub").join("deep")).expect("tree");
        std::fs::write(root.join("a.txt"), "a").expect("a");
        std::fs::write(root.join("sub").join("b.txt"), "b").expect("b");
        std::fs::write(root.join("sub").join("deep").join("c.txt"), "c").expect("c");

        let listing = find(&root, Some(1)).await.expect("depth 1");
        let lines: Vec<&str> = listing.lines().collect();
        assert!(lines.contains(&"a.txt"), "{listing}");
        assert!(lines.contains(&"sub/"), "{listing}");
        assert!(!listing.contains("b.txt"), "{listing}");
        assert!(!listing.contains("deep"), "{listing}");

        let everything = find(&root, None).await.expect("every level");
        let lines: Vec<&str> = everything.lines().collect();
        assert!(lines.contains(&"sub/deep/"), "{everything}");
        assert!(lines.contains(&"sub/deep/c.txt"), "{everything}");

        let error = find(&root, Some(0)).await.expect_err("zero depth");
        assert!(error.to_string().contains("max_depth"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
