//! `grep` — secure in-process content search.
//!
//! The tool uses ripgrep's matcher/searcher libraries and a handle-relative
//! walker without spawning external executables. Walked names are opened
//! exactly once through the retained root handle, validated as stable and
//! contained, and that same [`std::fs::File`] is passed to the searcher.
//! Every source byte, including binary classification drains, is charged
//! through one atomic scan budget. Per-file matches remain provisional
//! until the same reader reaches EOF without a NUL byte; binary or
//! incompletely classified files publish nothing.
//!
//! Results are deterministic top-N by the shared path order key (lossy
//! rendered path, original `OsString` tie-break) plus line number. Matching
//! line text is not part of that order. Path keys are interned and charged
//! only while at least one retained line of that path remains in the
//! provisional or global result heap; discard, zero retained lines,
//! last-line eviction, and `max_results = 0` refund the charge. Current
//! Windows hidden bits are re-read on the opened handle before content
//! access. Ignore parse/build/read or boundary uncertainty fails closed;
//! ordinary per-file I/O produces a model-visible incomplete lower-bound
//! notice. Paths use `/`, and cancellation or future drop is supervised
//! until the worker is interrupted and joined.
use parking_lot::Mutex;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use grep_matcher::LineTerminator;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{INTERRUPTED_NOTE, Tool, ToolError, ToolResult};

use super::blocking::run_blocking_until;
use super::fs_search::{
    FsEntryKind, IO_ERROR_SAMPLES, IoErrors, Limits, PathOrderKey, REGEX_DFA_SIZE_LIMIT,
    REGEX_SIZE_LIMIT, ResolvedRoot, ScanReservation, SearchAccess, WalkLimiter,
    bind_search_root_with_access, display_line, is_hidden_skip, opened_file_is_hidden, rel_posix,
    stop_reason_error, to_posix, walk_retained_tree,
};

use super::search_report::{
    PathGlob, ReportSpec, compile_glob_labeled, reject_pattern_bytes, render_report,
    skipped_target_report,
};

/// Default cap on reported matching lines.
pub const MAX_MATCHES: usize = 200;

/// The `grep` builtin.
#[derive(Debug)]
pub struct GrepTool;

/// Arguments for [`GrepTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepArgs {
    /// Pattern to search for: literal text by default, or a regular
    /// expression when `is_regex` is set.
    pub pattern: String,
    /// Interpret `pattern` as a regular expression.
    #[serde(default)]
    pub is_regex: bool,
    /// File or directory to search (relative to the session cwd).
    pub path: Option<String>,
    /// Only search files whose relative path matches this glob.
    pub include: Option<String>,
    /// Skip files whose relative path matches this glob.
    pub exclude: Option<String>,
    /// Maximum number of matching lines to report.
    pub max_results: Option<usize>,
}

struct LineMatch {
    path: Arc<PathOrderKey>,
    line_no: u64,
    line: String,
}

impl LineMatch {
    fn store_bytes(&self) -> usize {
        self.line.len().saturating_add(std::mem::size_of::<u64>())
    }
}

impl PartialEq for LineMatch {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.line_no == other.line_no
    }
}

impl Eq for LineMatch {}

impl PartialOrd for LineMatch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LineMatch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path
            .cmp(&other.path)
            .then(self.line_no.cmp(&other.line_no))
    }
}

struct SearchState {
    heap: Mutex<BinaryHeap<LineMatch>>,
    /// Text matches whose classification reached EOF without NUL.
    total_matches: AtomicU64,
    /// Committed text matches plus provisional, not-yet-classified matches.
    match_slots: AtomicU64,
    /// A text file had callbacks stopped while provisional slots were full.
    count_incomplete: AtomicBool,
    match_limit: u64,
    /// The pattern looked like a regex but `is_regex` was not set. It was
    /// also matched as one when `widened`, else only literally.
    literal_meta: bool,
    widened: bool,
    files_searched: AtomicU64,
    lines_truncated: AtomicBool,
    io_errors: IoErrors,
    limiter: Arc<WalkLimiter>,
}

impl SearchState {
    fn new(limits: &Limits, limiter: Arc<WalkLimiter>) -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
            total_matches: AtomicU64::new(0),
            match_slots: AtomicU64::new(0),
            count_incomplete: AtomicBool::new(false),
            match_limit: limits.count_budget,
            literal_meta: false,
            widened: false,
            files_searched: AtomicU64::new(0),
            lines_truncated: AtomicBool::new(false),
            io_errors: IoErrors::new(IO_ERROR_SAMPLES),
            limiter,
        }
    }

    /// Atomically reserves one callback slot. The returned value is the
    /// number of committed-plus-provisional slots after reservation.
    fn reserve_match(&self) -> Option<u64> {
        loop {
            let current = self.match_slots.load(Ordering::Acquire);
            if current >= self.match_limit {
                return None;
            }
            match self.match_slots.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(current + 1),
                Err(_) => continue,
            }
        }
    }

    fn release_provisional(&self, count: u64) {
        if count > 0 {
            self.match_slots.fetch_sub(count, Ordering::AcqRel);
        }
    }

    fn settle_text(&self, count: u64) -> u64 {
        self.total_matches.fetch_add(count, Ordering::AcqRel) + count
    }
}

struct FileSink<'a> {
    path: Arc<PathOrderKey>,
    lines: Vec<LineMatch>,
    binary: bool,
    reserved_matches: u64,
    callbacks_stopped: bool,
    line_truncated: bool,
    settled: bool,
    path_charged: bool,
    state: &'a SearchState,
    cap: usize,
    limits: &'a Limits,
}

impl FileSink<'_> {
    fn matched_event(&mut self, matched: &SinkMatch<'_>) -> bool {
        let Some(slots_after) = self.state.reserve_match() else {
            self.callbacks_stopped = true;
            return false;
        };
        self.reserved_matches += 1;

        if self.lines.len() < self.cap {
            let mut bytes = matched.bytes();
            if bytes.last() == Some(&b'\n') {
                bytes = &bytes[..bytes.len() - 1];
            }
            if bytes.last() == Some(&b'\r') {
                bytes = &bytes[..bytes.len() - 1];
            }
            let mut truncated = false;
            let line = display_line(bytes, self.limits.line_bytes, &mut truncated);
            self.line_truncated |= truncated;
            let candidate = LineMatch {
                path: Arc::clone(&self.path),
                line_no: matched.line_number().unwrap_or(0),
                line,
            };
            if self
                .state
                .limiter
                .try_reserve_result_bytes(candidate.store_bytes())
            {
                if self.charge_path() {
                    self.lines.push(candidate);
                } else {
                    self.state
                        .limiter
                        .release_result_bytes(candidate.store_bytes());
                }
            }
        }

        if slots_after >= self.state.match_limit {
            self.callbacks_stopped = true;
            false
        } else {
            true
        }
    }

    fn discard(mut self) {
        self.refund_retained();
        self.state.release_provisional(self.reserved_matches);
        self.settled = true;
    }

    fn commit_text(mut self) {
        let total = self.state.settle_text(self.reserved_matches);
        if self.callbacks_stopped {
            self.state.count_incomplete.store(true, Ordering::Release);
        }
        if self.line_truncated {
            self.state.lines_truncated.store(true, Ordering::Release);
        }
        self.state.files_searched.fetch_add(1, Ordering::Relaxed);

        let lines = std::mem::take(&mut self.lines);
        if lines.is_empty() {
            self.release_path_charge();
        } else {
            let mut heap = self.state.heap.lock();
            for line in lines {
                if heap.len() < self.cap {
                    heap.push(line);
                } else if heap.peek().is_some_and(|worst| line < *worst) {
                    if let Some(evicted) = heap.pop() {
                        self.state
                            .limiter
                            .release_result_bytes(evicted.store_bytes());
                        if !Arc::ptr_eq(&evicted.path, &self.path)
                            && !heap_has_path(&heap, &evicted.path)
                        {
                            self.state
                                .limiter
                                .release_result_bytes(evicted.path.store_bytes());
                        }
                    }
                    heap.push(line);
                } else {
                    self.state.limiter.release_result_bytes(line.store_bytes());
                }
            }
            if self.path_charged && !heap_has_path(&heap, &self.path) {
                self.release_path_charge();
            }
        }
        self.settled = true;

        if self.callbacks_stopped && total >= self.state.match_limit {
            self.state.limiter.stop("match-count budget reached");
        }
    }

    fn charge_path(&mut self) -> bool {
        if self.path_charged {
            return true;
        }
        if !self
            .state
            .limiter
            .try_reserve_result_bytes(self.path.store_bytes())
        {
            return false;
        }
        self.path_charged = true;
        true
    }

    fn release_path_charge(&mut self) {
        if self.path_charged {
            self.state
                .limiter
                .release_result_bytes(self.path.store_bytes());
            self.path_charged = false;
        }
    }

    fn refund_retained(&mut self) {
        for line in self.lines.drain(..) {
            self.state.limiter.release_result_bytes(line.store_bytes());
        }
        self.release_path_charge();
    }
}

fn heap_has_path(heap: &BinaryHeap<LineMatch>, path: &Arc<PathOrderKey>) -> bool {
    heap.iter().any(|line| Arc::ptr_eq(&line.path, path))
}

impl Drop for FileSink<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.refund_retained();
            self.state.release_provisional(self.reserved_matches);
            self.settled = true;
        }
    }
}

impl Sink for FileSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, matched: &SinkMatch<'_>) -> io::Result<bool> {
        Ok(self.matched_event(matched))
    }

    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> io::Result<bool> {
        self.binary = true;
        Ok(false)
    }
}

impl Tool for GrepTool {
    type Args = GrepArgs;
    type Output = ();

    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents under a directory (or the session cwd). Literal \
         text by default, regex with is_regex. Case is smart: an all-lowercase \
         pattern ignores case. Optional include/exclude globs; one without `/` \
         matches file names at any depth (`*.rs`, `main.rs`, `*.rs,*.toml`). \
         Matching is per line. Reports up to 200 matching lines as \
         `path:line:text`, with a notice when more matches exist. Hidden and \
         gitignored files are skipped."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("grep: search file contents (pattern, optional is_regex/path/include/exclude).")
    }

    fn search_access(&self) -> Option<SearchAccess> {
        Some(SearchAccess::Content)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        reject_pattern_bytes(&args.pattern, "pattern")?;
        if let Some(include) = args.include.as_deref() {
            reject_pattern_bytes(include, "include glob")?;
        }
        if let Some(exclude) = args.exclude.as_deref() {
            reject_pattern_bytes(exclude, "exclude glob")?;
        }
        let cwd = ctx.cwd.clone();
        let deadline = Instant::now() + Limits::default().time_limit;
        let limits = Limits {
            deadline: Some(deadline),
            ..Limits::default()
        };
        let cancel = ctx.cancel.clone();
        let prepared = ctx.prepared_search.clone();
        run_blocking_until("search", &cancel, deadline, move |worker_cancel| {
            if worker_cancel.is_cancelled() {
                return Err(ToolError::Execution(format!(
                    "search cancelled before completion{INTERRUPTED_NOTE}"
                )));
            }
            let matcher = compile_matcher(&args.pattern, args.is_regex)?;
            let include = compile_glob(args.include.as_deref(), "include")?;
            let exclude = compile_glob(args.exclude.as_deref(), "exclude")?;
            let root = bind_search_root_with_access(
                prepared.as_deref(),
                &cwd,
                args.path.as_deref(),
                &worker_cancel,
                &limits,
                SearchAccess::Content,
            )?;
            run_search(
                matcher,
                root,
                &args,
                include,
                exclude,
                &worker_cancel,
                &limits,
            )
        })
        .await
    }
}

fn run_search(
    matcher: RegexMatcher,
    mut root: ResolvedRoot,
    args: &GrepArgs,
    include: Option<PathGlob>,
    exclude: Option<PathGlob>,
    cancel: &CancellationToken,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    let cap = args
        .max_results
        .unwrap_or(MAX_MATCHES)
        .min(limits.stored_ceiling);
    let mut state = SearchState::new(limits, Arc::clone(&root.limiter));
    state.literal_meta = !args.is_regex && pattern_looks_regex(&args.pattern);
    state.widened = widens_to_regex(&args.pattern, args.is_regex);
    let state = Arc::new(state);
    let report_root = root.root.clone();

    match root.target_is_skipped() {
        Ok(true) => return Ok(skipped_target_report("grep", &root)),
        Ok(false) => {}
        Err(error) if is_hidden_skip(&error) => {
            return Ok(skipped_target_report("grep", &root));
        }
        Err(error) => {
            return Ok(ToolResult::error(format!(
                "search target hidden check failed: {error}"
            )));
        }
    }

    if root.is_file() {
        // Single-file matches render cwd-relative, exactly like walk-mode
        // results (and find), so include/exclude globs see one base.
        let relative = rel_posix(&root.cwd, &root.root);
        search_one_file(
            &matcher, &mut root, &relative, &include, &exclude, &state, cap, cancel, limits,
        );
        // `root` and the allowed-root identity handle remain alive through
        // report assembly.
        finish_search_report(&report_root, &state, cap, limits)
    } else {
        let root_guard = Arc::new(root);
        if let Err(error) = walk_and_search(
            &matcher,
            Arc::clone(&root_guard),
            &include,
            &exclude,
            &state,
            cap,
            cancel,
            limits,
        ) {
            return Ok(ToolResult::error(format!(
                "search ignore boundary could not be established: {error}"
            )));
        }
        // Assemble before `root_guard` drops, retaining the root identity for
        // the complete operation rather than only walker enumeration.
        finish_search_report(&report_root, &state, cap, limits)
    }
}

fn finish_search_report(
    root: &Path,
    state: &Arc<SearchState>,
    cap: usize,
    limits: &Limits,
) -> Result<ToolResult, ToolError> {
    if let Some(error) = stop_reason_error("search", &state.limiter) {
        return Err(error);
    }
    Ok(assemble_report(root, state, cap, limits))
}

#[expect(
    clippy::too_many_arguments,
    reason = "walker workers need explicit shared search policy"
)]
fn walk_and_search(
    matcher: &RegexMatcher,
    root: Arc<ResolvedRoot>,
    include: &Option<PathGlob>,
    exclude: &Option<PathGlob>,
    state: &Arc<SearchState>,
    cap: usize,
    cancel: &CancellationToken,
    limits: &Limits,
) -> io::Result<()> {
    let mut searcher = build_searcher(limits);
    walk_retained_tree(
        &root,
        &state.limiter,
        cancel,
        &state.io_errors,
        |relative_path, name, kind, parent| {
            if matches!(state.limiter.check(cancel), ignore::WalkState::Quit) {
                return ignore::WalkState::Quit;
            }
            if kind != FsEntryKind::File {
                return ignore::WalkState::Continue;
            }
            let relative = to_posix(relative_path);
            if include
                .as_ref()
                .is_some_and(|glob| !glob.is_match(&relative))
                || exclude
                    .as_ref()
                    .is_some_and(|glob| glob.is_match(&relative))
            {
                return ignore::WalkState::Continue;
            }

            let mut file = match root.open_walked(parent, name, FsEntryKind::File) {
                Ok(file) => file,
                Err(error) => {
                    if !is_hidden_skip(&error) {
                        state.io_errors.record(&relative, &error);
                    }
                    return if state.limiter.quit.load(Ordering::Acquire) {
                        ignore::WalkState::Quit
                    } else {
                        ignore::WalkState::Continue
                    };
                }
            };
            let path = PathOrderKey::from_rendered_and_raw(relative, relative_path.as_os_str());
            if let Err(error) = search_open_file(
                &mut searcher,
                matcher,
                &mut file,
                path,
                state,
                cap,
                cancel,
                limits,
            ) && !state.limiter.quit.load(Ordering::Acquire)
                && !cancel.is_cancelled()
            {
                state.io_errors.record(&to_posix(relative_path), &error);
            }
            if state.limiter.quit.load(Ordering::Acquire) {
                ignore::WalkState::Quit
            } else {
                ignore::WalkState::Continue
            }
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "single-file behavior mirrors walker policy"
)]
fn search_one_file(
    matcher: &RegexMatcher,
    root: &mut ResolvedRoot,
    relative: &str,
    include: &Option<PathGlob>,
    exclude: &Option<PathGlob>,
    state: &Arc<SearchState>,
    cap: usize,
    cancel: &CancellationToken,
    limits: &Limits,
) {
    if matches!(state.limiter.check(cancel), ignore::WalkState::Quit)
        || include
            .as_ref()
            .is_some_and(|glob| !glob.is_match(relative))
        || exclude.as_ref().is_some_and(|glob| glob.is_match(relative))
    {
        return;
    }
    let mut searcher = build_searcher(limits);
    let raw = root.root.as_os_str().to_os_string();
    let file = match root.target_file_mut() {
        Ok(file) => file,
        Err(error) => {
            state.io_errors.record(relative, &error);
            return;
        }
    };
    if let Err(error) = search_open_file(
        &mut searcher,
        matcher,
        file,
        PathOrderKey::from_rendered_and_raw(relative.to_owned(), raw),
        state,
        cap,
        cancel,
        limits,
    ) && !state.limiter.quit.load(Ordering::Acquire)
        && !cancel.is_cancelled()
    {
        state.io_errors.record(relative, &error);
    }
}

fn build_searcher(limits: &Limits) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .line_terminator(LineTerminator::crlf())
        .binary_detection(BinaryDetection::quit(0))
        .memory_map(MmapChoice::never())
        .heap_limit(Some(limits.line_heap))
        .build()
}

struct PolledReader<'a> {
    file: &'a mut File,
    state: &'a SearchState,
    cancel: &'a CancellationToken,
    scan_cap: u64,
    eof_seen: bool,
    nul_seen: bool,
}

impl PolledReader<'_> {
    fn drain_classification(&mut self) -> io::Result<()> {
        let mut buffer = [0u8; 8 * 1024];
        while !self.eof_seen && !self.nul_seen {
            let bytes = self.read(&mut buffer)?;
            if bytes == 0 {
                break;
            }
        }
        Ok(())
    }
}

impl Read for PolledReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if matches!(
                self.state.limiter.check(self.cancel),
                ignore::WalkState::Quit
            ) {
                return Err(io::Error::other("search stopped early"));
            }
            match self.state.limiter.reserve_scan(buffer.len(), self.scan_cap) {
                ScanReservation::Granted(reserved) => {
                    if let Err(error) = super::blocking::wait_for_worker_readable(self.file) {
                        self.state.limiter.settle_scan(reserved, 0);
                        return Err(error);
                    }
                    let result = self.file.read(&mut buffer[..reserved]);
                    match result {
                        Ok(actual) => {
                            self.state.limiter.settle_scan(reserved, actual);
                            if actual == 0 {
                                self.eof_seen = true;
                            } else {
                                self.nul_seen |= buffer[..actual].contains(&0);
                            }
                            return Ok(actual);
                        }
                        Err(error) => {
                            self.state.limiter.settle_scan(reserved, 0);
                            return Err(error);
                        }
                    }
                }
                ScanReservation::Pending => std::thread::yield_now(),
                ScanReservation::Exhausted => {
                    // Reaching the hard cap is itself an early stop. We do
                    // not use metadata as a synthetic EOF: only an actual
                    // zero-byte read can finish text classification.
                    self.state.limiter.stop("scanned-bytes limit reached");
                    return Err(io::Error::other("search stopped early"));
                }
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "opened-file processing receives all immutable limits explicitly"
)]
fn search_open_file(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    file: &mut File,
    path: PathOrderKey,
    state: &SearchState,
    cap: usize,
    cancel: &CancellationToken,
    limits: &Limits,
) -> io::Result<()> {
    let path = Arc::new(path);
    let mut sink = FileSink {
        path,
        lines: Vec::new(),
        binary: false,
        reserved_matches: 0,
        callbacks_stopped: false,
        line_truncated: false,
        settled: false,
        path_charged: false,
        state,
        cap,
        limits,
    };
    let (nul_seen, eof_seen) = {
        let mut reader = PolledReader {
            file,
            state,
            cancel,
            scan_cap: limits.scan_bytes,
            eof_seen: false,
            nul_seen: false,
        };

        let search_result = searcher.search_reader(matcher.clone(), &mut reader, &mut sink);
        if reader.nul_seen || sink.binary {
            sink.discard();
            return Ok(());
        }
        search_result?;

        // A sink stop (count budget) can return success before EOF. Finish only
        // binary classification through this reader and the same byte budget;
        // no matcher callbacks are made during the drain.
        if !reader.eof_seen {
            reader.drain_classification()?;
        }
        (reader.nul_seen, reader.eof_seen)
    };
    if nul_seen {
        sink.discard();
    } else if eof_seen {
        if opened_file_is_hidden(file)? {
            sink.discard();
            return Ok(());
        }
        sink.commit_text();
    } else {
        return Err(io::Error::other("file classification did not reach EOF"));
    }
    Ok(())
}

fn assemble_report(
    root: &Path,
    state: &Arc<SearchState>,
    cap: usize,
    limits: &Limits,
) -> ToolResult {
    let heap = std::mem::take(&mut *state.heap.lock());
    let matches = heap.into_sorted_vec();
    let entries: Vec<String> = matches
        .iter()
        .map(|matched| {
            format!(
                "{}:{}:{}",
                matched.path.rendered(),
                matched.line_no,
                matched.line
            )
        })
        .collect();

    let total = state.total_matches.load(Ordering::Acquire);
    let count_incomplete = state.count_incomplete.load(Ordering::Acquire);
    let stop_reason = state
        .limiter
        .stopped_reason()
        .or(count_incomplete.then_some("match-count budget reached"))
        .or(state
            .limiter
            .result_store_truncated()
            .then_some("result store limit reached"));
    let lines_truncated = state.lines_truncated.load(Ordering::Acquire);
    let mut extra_details = vec![(
        "files_searched",
        json!(state.files_searched.load(Ordering::Relaxed)),
    )];
    if lines_truncated {
        extra_details.push(("lines_truncated", json!(true)));
    }
    render_report(
        root,
        limits.output_bytes,
        ReportSpec {
            entries,
            total,
            stop_reason,
            io: state.io_errors.summary(),
            noun: "matching lines",
            truncated_advice: "narrow the pattern or raise max_results",
            output_advice: "narrow the search or lower max_results",
            extra_notices: {
                let mut notices = Vec::new();
                if lines_truncated {
                    notices.push(format!(
                        "[some matching lines truncated to {} bytes; use read to see the full line]",
                        limits.line_bytes
                    ));
                }
                let files_searched = state.files_searched.load(Ordering::Relaxed);
                if total == 0 && files_searched > 0 {
                    notices.push(format!(
                        "[searched {files_searched} files; hidden and gitignored files are skipped]"
                    ));
                }
                if state.widened {
                    notices.push(
                        "[pattern has regex syntax but is_regex is not set; it matched both literally and as a regular expression]"
                            .to_owned(),
                    );
                } else if state.literal_meta && total == 0 && files_searched > 0 {
                    notices.push(
                        "[pattern contains `|`/`.*` but is_regex is not set; it matched literally — pass is_regex to use it as a regular expression]"
                            .to_owned(),
                    );
                }
                notices
            },
            extra_details,
            cap,
        },
    )
}

/// `a|b`, `foo.*bar`, and escapes like `\s+` or `\(` are how a model
/// writes a regex; as a literal they almost never exist in real text.
/// Bare metacharacters (`(`, `[`, `^`) appear in ordinary text too often to
/// be a signal.
fn pattern_looks_regex(pattern: &str) -> bool {
    const SIGNALS: [&str; 10] = [
        "|", ".*", ".+", "\\b", "\\s", "\\w", "\\d", "\\(", "\\[", "\\.",
    ];
    SIGNALS.iter().any(|signal| pattern.contains(signal))
}

/// Whether a literal search also matches `pattern` as a regex. A model that
/// forgets `is_regex` otherwise gets an empty report for a pattern that is
/// plainly a regex. The literal reading still matches, and a regex that
/// matches the empty string (`a || b` read as alternation) would match
/// every line, so it stays literal.
fn widens_to_regex(pattern: &str, is_regex: bool) -> bool {
    !is_regex
        && pattern_looks_regex(pattern)
        && regex::Regex::new(pattern).is_ok_and(|regex| !regex.is_match(""))
}

/// Builds the line matcher. Case is smart: an all-lowercase pattern ignores
/// case, one with an uppercase letter matches it exactly.
fn compile_matcher(pattern: &str, is_regex: bool) -> Result<RegexMatcher, ToolError> {
    reject_pattern_bytes(pattern, "pattern")?;
    let (spelled, fixed) = if widens_to_regex(pattern, is_regex) {
        (format!("{}|(?:{pattern})", regex::escape(pattern)), false)
    } else {
        (pattern.to_owned(), !is_regex)
    };
    RegexMatcherBuilder::new()
        .fixed_strings(fixed)
        .case_smart(true)
        .crlf(true)
        .ban_byte(Some(0))
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build(&spelled)
        .map_err(|error| ToolError::InvalidArgs(format!("invalid regex: {error}")))
}

fn compile_glob(glob: Option<&str>, label: &str) -> Result<Option<PathGlob>, ToolError> {
    match glob {
        None => Ok(None),
        Some(pattern) => Ok(Some(compile_glob_labeled(pattern, label)?)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::{GrepArgs, GrepTool};
    use crate::builtin::fs_search::SearchAccess;
    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::Tool;

    #[tokio::test]
    async fn grep_finds_lf_text_without_an_external_rg() {
        let root = std::env::temp_dir().join(format!(
            "mycode-grep-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(root.join("sub")).expect("dir");
        std::fs::write(root.join("sub/a.txt"), "alpha needle\nsecond line\n").expect("file");
        let prepared = crate::builtin::fs_search::prepare_search_async_with_access(
            root.clone(),
            None,
            CancellationToken::new(),
            SearchAccess::Content,
        )
        .await
        .expect("prepare");
        let ctx = ToolCtx::new(&root).with_prepared_search(Arc::new(prepared));
        let (mut out, _rx) = ToolStream::channel();
        let result = GrepTool
            .execute(
                GrepArgs {
                    pattern: "needle".to_owned(),
                    is_regex: false,
                    path: Some("sub".to_owned()),
                    include: None,
                    exclude: None,
                    max_results: None,
                },
                &ctx,
                &mut out,
            )
            .await
            .expect("grep");
        let text: String = result
            .content
            .iter()
            .filter_map(|block| match block {
                mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(!result.is_error, "grep failed on LF files: {text}");
        assert!(text.contains("needle"), "expected the LF match, got {text}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A `|`/`.*` pattern sent without `is_regex` matches literally — and
    /// almost always finds nothing. The report must say so, instead of
    /// leaving a wrong "the file is empty" conclusion.
    #[tokio::test]
    async fn a_regex_looking_literal_pattern_gets_a_hint() {
        let root = std::env::temp_dir().join(format!(
            "mycode-grep-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("dir");
        std::fs::write(root.join("a.txt"), "alpha needle\nsecond line\n").expect("file");
        // The prepared search handle is consumed per execute, so each call
        // builds its own.
        let run = |pattern: &str, is_regex: bool, path: &str| {
            let root = root.clone();
            let pattern = pattern.to_owned();
            let path = path.to_owned();
            async move {
                let prepared = crate::builtin::fs_search::prepare_search_async_with_access(
                    root.clone(),
                    None,
                    CancellationToken::new(),
                    SearchAccess::Content,
                )
                .await
                .expect("prepare");
                let ctx = ToolCtx::new(&root).with_prepared_search(Arc::new(prepared));
                let (mut out, _rx) = ToolStream::channel();
                GrepTool
                    .execute(
                        GrepArgs {
                            pattern,
                            is_regex,
                            path: Some(path),
                            include: None,
                            exclude: None,
                            max_results: None,
                        },
                        &ctx,
                        &mut out,
                    )
                    .await
                    .expect("grep")
            }
        };
        let text = |result: &crate::tool::ToolResult| {
            result
                .content
                .iter()
                .filter_map(|block| match block {
                    mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        };

        // A regex-looking literal is matched as a regex too, and says so.
        let literal = run("alpha|needle", false, "a.txt").await;
        let literal_text = text(&literal);
        assert!(literal_text.contains("alpha needle"), "{literal_text}");
        assert!(literal_text.contains("is_regex"), "{literal_text}");

        let regex = run("alpha|needle", true, "a.txt").await;
        let regex_text = text(&regex);
        assert!(regex_text.contains("alpha needle"), "{regex_text}");
        assert!(!regex_text.contains("is_regex"), "{regex_text}");

        // A pattern whose regex reading matches the empty string stays
        // literal: `a || b` as alternation would match every line.
        std::fs::write(root.join("b.txt"), "a || b\nplain\n").expect("file b");
        let hit = run("a || b", false, "b.txt").await;
        let hit_text = text(&hit);
        assert!(hit_text.contains("a || b"), "{hit_text}");
        assert!(!hit_text.contains("plain"), "{hit_text}");
        assert!(!hit_text.contains("is_regex"), "{hit_text}");

        // Nothing found still says the search ran.
        let miss = run("absent", false, "a.txt").await;
        assert!(
            text(&miss).starts_with("No matching lines found."),
            "{}",
            text(&miss)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn grep_args(pattern: &str) -> GrepArgs {
        GrepArgs {
            pattern: pattern.to_owned(),
            is_regex: false,
            path: None,
            include: None,
            exclude: None,
            max_results: None,
        }
    }

    async fn grep_text(root: &std::path::Path, args: GrepArgs) -> String {
        let prepared = crate::builtin::fs_search::prepare_search_async_with_access(
            root.to_path_buf(),
            args.path.clone(),
            CancellationToken::new(),
            SearchAccess::Content,
        )
        .await
        .expect("prepare");
        let ctx = ToolCtx::new(root).with_prepared_search(Arc::new(prepared));
        let (mut out, _rx) = ToolStream::channel();
        let result = GrepTool.execute(args, &ctx, &mut out).await.expect("grep");
        result
            .content
            .iter()
            .filter_map(|block| match block {
                mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The ways a model asks for text that is there and used to get an
    /// empty report: a bare file name as `include`, a comma list, a
    /// lowercase pattern for a capitalized word, and an explicit hidden
    /// path.
    #[tokio::test]
    async fn common_model_spellings_find_the_text() {
        let root = std::env::temp_dir().join(format!(
            "mycode-grep-spell-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(root.join("src/deep")).expect("dir");
        std::fs::create_dir_all(root.join(".github")).expect("hidden dir");
        std::fs::write(root.join("src/deep/main.rs"), "fn Needle() {}\n").expect("rs");
        std::fs::write(root.join("Cargo.toml"), "needle = 1\n").expect("toml");
        std::fs::write(root.join(".github/ci.yml"), "needle\n").expect("yml");

        let text = grep_text(
            &root,
            GrepArgs {
                include: Some("main.rs".to_owned()),
                ..grep_args("Needle")
            },
        )
        .await;
        assert!(text.contains("src/deep/main.rs:1:"), "{text}");

        let text = grep_text(
            &root,
            GrepArgs {
                include: Some("*.rs, *.toml".to_owned()),
                ..grep_args("needle")
            },
        )
        .await;
        assert!(text.contains("src/deep/main.rs:1:"), "smart case: {text}");
        assert!(text.contains("Cargo.toml:1:"), "{text}");

        // An uppercase letter makes the match exact.
        let text = grep_text(&root, grep_args("NEEDLE")).await;
        assert!(text.starts_with("No matching lines found."), "{text}");

        let text = grep_text(
            &root,
            GrepArgs {
                path: Some(".github".to_owned()),
                ..grep_args("needle")
            },
        )
        .await;
        assert!(text.contains("hidden or gitignored"), "{text}");
        let _ = std::fs::remove_dir_all(root);
    }
}
