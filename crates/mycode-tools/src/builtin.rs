//! Builtin tools — the trusted reference implementation of the
//! [`crate::tool::Tool`] trait. File discovery and content search stay in-process and never spawn
//! external `fd` or `rg` executables.

pub mod ask;
pub(crate) mod blocking;
pub mod edit;
pub mod exec;
pub mod find;
pub(crate) mod fs_io;
pub(crate) mod fs_search;
pub(crate) mod fs_walk;
pub mod grep;
pub(crate) mod process;
pub mod read;
pub(crate) mod search_report;
pub mod shell;
pub mod task;
pub mod web;
pub mod write;

pub use ask::{AskAnswer, AskChannel, AskQuestion, AskTool, user_dismissed};
pub use edit::EditTool;
pub use exec::ExecTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use shell::ShellTool;
pub use task::{SubagentRequest, TaskHost, TaskTool};
pub use web::{FetchContentTool, WebHit, WebHost, WebPage, WebSearchTool};
pub use write::WriteTool;

use std::sync::Arc;

use crate::registry::ToolRegistry;
use crate::tool::ToolDyn;

/// All builtin tools as type-erased, registry-ready handles.
pub(crate) fn builtin_tools() -> Vec<Arc<dyn ToolDyn>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(ShellTool::default()),
        Arc::new(ExecTool::default()),
        Arc::new(GrepTool),
        Arc::new(FindTool),
    ]
}

/// Register all builtin tools into a registry.
pub fn register_builtins(registry: &ToolRegistry) {
    for tool in builtin_tools() {
        registry.register(tool);
    }
}

/// Byte-cap text truncation that respects char boundaries.
///
/// Returns `(text, truncated)`; callers append their own notice so the
/// model knows how to fetch the rest (offset/limit, narrower glob, …).
pub(crate) fn truncate_bytes(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}
