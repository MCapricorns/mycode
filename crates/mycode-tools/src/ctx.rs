//! `ToolCtx` — the per-invocation execution context handed to every tool.
//! See `docs/tools.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::builtin::fs_io::PreparedFile;
use crate::builtin::fs_search::PreparedSearch;

/// Everything a tool needs to execute an invocation.
///
/// The context carries the working directory, cancellation token, and
/// prepared filesystem capabilities as read-only ambient state.
#[derive(Clone)]
pub struct ToolCtx {
    /// Working directory for relative tool paths (see [`ToolCtx::resolve`]).
    pub cwd: PathBuf,
    /// Cooperative cancellation; long-running tools should poll it and
    /// abort early.
    pub cancel: CancellationToken,
    /// Provider call id for this invocation, when the dispatcher has one.
    pub call_id: String,
    /// Ready grep/find root bound at dispatch preflight, if any.
    ///
    /// Present only after a successful prepare. Execution takes that root
    /// once and never re-resolves it.
    pub prepared_search: Option<Arc<PreparedSearch>>,
    /// Ready file capability bound at dispatch preflight, if any.
    ///
    /// Host-owned. Execution takes the inner handles once and never
    /// re-resolves.
    pub prepared_file: Option<Arc<PreparedFile>>,
}

impl ToolCtx {
    /// Creates a context with a fresh token and no prepared capabilities.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            call_id: String::new(),
            prepared_search: None,
            prepared_file: None,
        }
    }

    /// Replace the cancellation token (builder style).
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Record the provider call id (builder style).
    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = call_id.into();
        self
    }

    /// Bind a ready preflight grep/find root (builder style).
    pub fn with_prepared_search(mut self, prepared: Arc<PreparedSearch>) -> Self {
        self.prepared_search = Some(prepared);
        self
    }

    /// Bind a ready preflight file capability (builder style).
    pub fn with_prepared_file(mut self, prepared: Arc<PreparedFile>) -> Self {
        self.prepared_file = Some(prepared);
        self
    }

    /// Resolve a tool-supplied path against `cwd`.
    ///
    /// Absolute paths pass through; relative paths are joined onto `cwd`.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}
