//! Shared secure filesystem-search machinery.
//!
//! `grep` and `find` resolve the session cwd once, retain stable root
//! handles, normalize output paths, enforce shared limits, and bridge
//! blocking walkers onto a dedicated OS thread. Enumeration reads directory
//! entries and ignore files through those retained handles, never by
//! re-resolving the root path name. Names are never trusted as the object
//! to read or report. User-typed components are rewritten to the unique
//! on-disk directory-entry spelling proven by parent-handle identity;
//! zero or several matches fail closed so a case-insensitive alias cannot
//! keep a lower-case path or ignore key. Grep opens each matching
//! file once for content read through the retained parent handle. Find
//! binds a metadata-only capability so an unreadable file is still
//! discovered; directories and grep request content access, and a
//! metadata-then-content pair must share identity. Windows hidden bits are
//! re-read from the opened handle before read, confirm, descent, and
//! reporting. Ignore parse/build/load failures are terminating. Ancestor
//! ignore discovery that stops at a mount boundary is not a failure; ordinary
//! per-path I/O is a model-visible incomplete lower bound. Unix uses
//! root-relative `openat` calls with no-follow traversal. Windows opens
//! every component relative to retained directory handles with `NtOpenFile`,
//! rejects reparse points, and validates final Unicode handle paths against
//! the retained allowed-root handle.
//!
//! Linux opens each child with `openat2(RESOLVE_BENEATH | RESOLVE_NO_XDEV |
//! RESOLVE_NO_SYMLINKS)` so bind mounts cannot be crossed, including at
//! find confirmation. After that open, Linux proves the mount with
//! `STATX_MNT_ID`: an overlay directory and a file in it may differ in
//! `st_dev` while staying on one mount, and a bind mount is still a boundary
//! when the mount id changes. Find confirmation on Linux/Android uses
//! `O_PATH` so a mode-`000` name can be reported without content-read
//! permission; other Unix confirms with no-follow metadata (`fstatat`) and
//! the same `st_dev` / type / `nlink` checks, and fails closed when that
//! proof is unavailable.
//! Other Unix descent still uses `openat(O_NOFOLLOW)` plus `st_dev`; that is
//! the mount identity on Darwin/BSD, which have no Linux-style same-`st_dev`
//! bind mounts. Platforms without handle-relative open fail closed. Regular
//! files with a link count other than one are refused so a cwd-visible
//! hardlink cannot expose an inode that also lives outside the allowed root.
//! Directory listings are collected up to a width cap, decorated once with
//! the lossy rendered component key the frontier and top-N heaps use, sorted
//! with the original `OsString` as the complete tie-break, and visited best-first
//! by full rendered path within the invocation depth, entry, handle, ignore-byte,
//! and ignore-rule limits.
//!
//! Everything stays in-process (handle-relative walk plus ripgrep's
//! search core); no external `rg` or `fd` executable is used.
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::tool::ToolError;

// The walker lives in `crate::builtin::fs_walk`; the `walk` alias keeps the
// historical in-module paths (`walk::IgnoreStack`, `walk::name_is_hidden`, …).
use crate::builtin::fs_walk as walk;
pub(crate) use crate::builtin::fs_walk::walk_retained_tree;

/// Configured wall-clock deadline before supervised cancellation begins.
pub(crate) const SEARCH_TIME_LIMIT: Duration = Duration::from_secs(60);

/// Maximum bytes actually read by one grep invocation.
pub(crate) const SCAN_BYTES_CAP: u64 = 512 * 1024 * 1024;

/// Ceiling on stored results regardless of caller-provided caps.
pub(crate) const STORED_CEILING: usize = 10_000;

/// Bytes of a matching line retained in output.
pub(crate) const MAX_LINE_BYTES: usize = 500;

/// Total rendered output bytes before lines are omitted.
pub(crate) const OUTPUT_BYTES_CAP: usize = 100 * 1024;

/// Maximum committed-plus-provisional match callbacks per grep invocation.
pub(crate) const COUNT_BUDGET: u64 = 100_000;

/// Per-file heap ceiling for the grep searcher's line buffer.
pub(crate) const LINE_HEAP_LIMIT: usize = 10 * 1024 * 1024;

/// Maximum per-path I/O error strings retained in result details.
pub(crate) const IO_ERROR_SAMPLES: usize = 5;

/// Directory nesting cap relative to the selected target.
///
/// Deep enough for real trees; a larger value would let a cyclic or
/// hostile layout pin handles along the DFS spine until the wall clock
/// expires.
pub(crate) const MAX_WALK_DEPTH: usize = 256;

/// Names examined in one grep/find invocation, including skipped ones.
///
/// Bounds work on a very wide repository before match/time caps fire.
pub(crate) const MAX_WALK_ENTRIES: u64 = 100_000;

/// Maximum directory-entry names buffered for sorted traversal.
///
/// A directory wider than this stops the walk with
/// `directory width limit reached`; the tool result is a successful
/// lower-bound report whose details carry `stopped_early`, not a hard
/// failure. On-disk component spelling scans use the same cap and fail
/// closed with a distinct error instead of that stop reason.
pub(crate) const MAX_DIR_WIDTH: usize = 16_384;

/// Total bytes loaded from ignore files in one invocation.
///
/// Per-file cap is [`walk::IGNORE_FILE_MAX_BYTES`]; this bounds the sum
/// across the tree so many small ignore files cannot pin memory.
pub(crate) const MAX_IGNORE_TOTAL_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum compiled ignore layers retained for one invocation.
pub(crate) const MAX_IGNORE_LAYERS: usize = 1_024;

/// Maximum ignore rules (accepted lines) compiled for one invocation.
pub(crate) const MAX_IGNORE_RULES: usize = 16_384;

/// Charged live directory/file handles for one invocation.
///
/// Covers the allowed root, selected target, and every best-first frontier
/// directory. Exhausted and empty directory frames release their handle
/// charge immediately; only the live frontier retains charged walk handles.
/// Equal to twice [`MAX_WALK_DEPTH`] so a full-depth spine plus the two
/// retained roots still fail closed before `RLIMIT_NOFILE`. This is a
/// limiter budget, not a kernel-enforced exact handle cap: short-lived
/// descriptors used inside one open or stat are not charged.
pub(crate) const MAX_OPEN_HANDLES: u64 = 512;

/// Maximum bytes in a grep pattern or find/include/exclude glob.
///
/// The regex crate's nest limit is not a heap bound; capping the concrete
/// pattern is what keeps compile memory proportional and fail-closed.
pub(crate) const MAX_PATTERN_BYTES: usize = 16 * 1024;

/// In-memory grep/find result heap cap (interned paths plus stored lines).
///
/// Output is already cut at [`OUTPUT_BYTES_CAP`]. This separate heap bound
/// stops 10_000 long handle-relative paths from pinning a gigabyte before
/// the renderer truncates.
pub(crate) const MAX_RESULT_STORE_BYTES: usize = 4 * 1024 * 1024;

/// Parent directories examined when locating a Git boundary above cwd.
///
/// Same numeric ceiling as [`MAX_WALK_DEPTH`]: deep enough for real
/// monorepos, and a larger value would let a hostile layout walk to `/`.
pub(crate) const MAX_GIT_PARENT_HOPS: usize = 256;

/// Compiled-NFA ceiling passed to grep-regex (1 MiB).
pub(crate) const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

/// Per-thread DFA cache ceiling passed to grep-regex (1 MiB).
pub(crate) const REGEX_DFA_SIZE_LIMIT: usize = 1024 * 1024;

/// Access requested when opening a child from a retained parent handle.
///
/// Find binds [`SearchAccess::Metadata`] so a mode-`000` or
/// `FILE_READ_ATTRIBUTES`-only file can still be discovered. Grep and
/// directory listing bind [`SearchAccess::Content`]. A directory that was
/// first opened for metadata is reopened for content only when the two
/// identities match. Metadata handles are never used to read file bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchAccess {
    /// Read file bytes or list a directory (grep, ignore files, descent).
    Content,
    /// Type, identity, and attribute checks only (find files).
    Metadata,
}

/// Global caps for one tool invocation.
#[derive(Clone, Debug)]
pub(crate) struct Limits {
    /// Wall-clock budget for the whole walk and search.
    pub time_limit: Duration,
    /// Maximum bytes actually read from opened files.
    pub scan_bytes: u64,
    /// Stored-result ceiling.
    pub stored_ceiling: usize,
    /// Per-line display truncation in bytes.
    pub line_bytes: usize,
    /// Search-engine line-buffer heap ceiling per file.
    pub line_heap: usize,
    /// Total output byte cap.
    pub output_bytes: usize,
    /// Maximum committed-plus-provisional match callbacks.
    pub count_budget: u64,
    /// Maximum directory nesting relative to the selected target.
    pub max_walk_depth: usize,
    /// Maximum directory entries examined in one invocation.
    pub max_walk_entries: u64,
    /// Maximum names buffered from one directory.
    pub max_dir_width: usize,
    /// Total ignore-file bytes loaded in one invocation.
    pub max_ignore_bytes: u64,
    /// Maximum compiled ignore layers in one invocation.
    pub max_ignore_layers: usize,
    /// Maximum ignore rules compiled in one invocation.
    pub max_ignore_rules: usize,
    /// Maximum live handles retained for one invocation.
    pub max_open_handles: u64,
    /// Cumulative bytes stored in the grep/find result heap.
    pub max_result_bytes: usize,
    /// Shared wall-clock deadline for the limiter and the outer timer.
    ///
    /// When `None`, [`WalkLimiter::new`] uses `Instant::now() + time_limit`.
    /// Execution and preflight compute one `Instant` and pass it here so a
    /// worker cannot publish `stopped_early` while the outer timer is still
    /// sleeping.
    pub deadline: Option<Instant>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            time_limit: SEARCH_TIME_LIMIT,
            scan_bytes: SCAN_BYTES_CAP,
            stored_ceiling: STORED_CEILING,
            line_bytes: MAX_LINE_BYTES,
            line_heap: LINE_HEAP_LIMIT,
            output_bytes: OUTPUT_BYTES_CAP,
            count_budget: COUNT_BUDGET,
            max_walk_depth: MAX_WALK_DEPTH,
            max_walk_entries: MAX_WALK_ENTRIES,
            max_dir_width: MAX_DIR_WIDTH,
            max_ignore_bytes: MAX_IGNORE_TOTAL_BYTES,
            max_ignore_layers: MAX_IGNORE_LAYERS,
            max_ignore_rules: MAX_IGNORE_RULES,
            max_open_handles: MAX_OPEN_HANDLES,
            max_result_bytes: MAX_RESULT_STORE_BYTES,
            deadline: None,
        }
    }
}

mod error_map;
mod limiter;
mod path_util;
mod root;

pub(crate) use root::open_directory_nofollow;
mod target;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub(crate) use error_map::*;
pub(crate) use limiter::*;
pub(crate) use path_util::*;
pub(crate) use root::*;
pub(crate) use target::*;
#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;

// Blocking-worker runtime lives in `crate::builtin::blocking`; the search
// entry points below keep their historical `fs_search` paths for the
// crate-public API.
pub use crate::builtin::blocking::{prepare_search_async, prepare_search_async_with_access};

/// Handle-backed grep/find target bound during dispatch preparation.
///
/// A value exists only as a ready retained root: `prepare_search` never
/// constructs a path key with an empty root. The dispatcher moves that root
/// into execution. A later path rewrite must re-prepare; execution never
/// re-resolves a prepared root, including after the root is taken.
pub struct PreparedSearch {
    key: String,
    access: SearchAccess,
    root: Mutex<Option<ResolvedRoot>>,
}

impl std::fmt::Debug for PreparedSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSearch")
            .field("key", &self.key)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl PreparedSearch {
    /// Returns the cwd-relative on-disk spelling bound to this capability.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Access mode retained by this capability.
    pub fn access(&self) -> SearchAccess {
        self.access
    }

    /// Takes the retained root exactly once for execution.
    pub(crate) fn take_root(&self) -> Option<ResolvedRoot> {
        self.root.lock().ok().and_then(|mut guard| guard.take())
    }
}

/// Resolves `cwd` and `path_arg` once for retained-capability execution.
///
/// Any resolve, open, alias, ignore, missing, or sharing failure is a
/// terminating [`ToolError`]. Success is always a ready retained root.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] or [`ToolError::InvalidArgs`] when the
/// target cannot be bound, including missing paths, sharing violations,
/// cancellation, and deadline expiry.
pub fn prepare_search(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_with_access(cwd, path_arg, cancel, SearchAccess::Content)
}

/// [`prepare_search`] with an explicit content/metadata capability.
///
/// # Errors
///
/// Same as [`prepare_search`].
pub(crate) fn prepare_search_with_access(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    access: SearchAccess,
) -> Result<PreparedSearch, ToolError> {
    prepare_search_with_limits_access(cwd, path_arg, cancel, &Limits::default(), access)
}

pub(crate) fn prepare_search_with_limits_access(
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
    access: SearchAccess,
) -> Result<PreparedSearch, ToolError> {
    let root = resolve_search_root_with_access(cwd, path_arg, cancel, limits, access)?;
    Ok(PreparedSearch {
        key: posix_relative_key(&root.target_relative),
        access,
        root: Mutex::new(Some(root)),
    })
}

/// Binds the grep/find root for execution.
///
/// A preflight [`PreparedSearch`] is a ready retained root. Execution takes
/// that root once and does not resolve it again. A prepared root that is
/// already consumed is an error. Only the path with no dispatch preparation
/// resolves a root here.
///
/// # Errors
///
/// Returns [`ToolError::Execution`] when a prepared root is already consumed,
/// does not match its path key, or was prepared for a different access mode.
/// The no-preflight path returns the same errors as
/// [`resolve_search_root_with_access`].
pub(crate) fn bind_search_root_with_access(
    prepared: Option<&PreparedSearch>,
    cwd: &Path,
    path_arg: Option<&str>,
    cancel: &CancellationToken,
    limits: &Limits,
    access: SearchAccess,
) -> Result<ResolvedRoot, ToolError> {
    if let Some(prepared) = prepared {
        if prepared.access() != access {
            return Err(ToolError::Execution(format!(
                "prepared search access mismatch: prepared {:?}, requested {:?}",
                prepared.access(),
                access
            )));
        }
        let Some(root) = prepared.take_root() else {
            return Err(ToolError::Execution(
                "prepared search root is missing or was already consumed".to_owned(),
            ));
        };
        let bound = posix_relative_key(&root.target_relative);
        if bound != prepared.key() {
            return Err(ToolError::Execution(
                "prepared search root does not match its path key".to_owned(),
            ));
        }
        if let Some(deadline) = limits.deadline {
            root.limiter.refresh_deadline_at(deadline);
        } else {
            root.limiter.refresh_deadline(limits.time_limit);
        }
        return Ok(root);
    }
    resolve_search_root_with_access(cwd, path_arg, cancel, limits, access)
}

#[cfg(test)]
mod ancestor_mount;
