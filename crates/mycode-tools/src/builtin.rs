//! Builtin tools — the trusted reference implementation of the
//! [`crate::tool::Tool`] trait. File discovery and content search stay in-process and never spawn
//! external `fd` or `rg` executables.

pub mod agent;
pub mod ask;
pub(crate) mod blocking;
pub mod edit;
pub(crate) mod exec;
pub mod find;
pub(crate) mod fs_io;
pub(crate) mod fs_search;
pub(crate) mod fs_walk;
pub mod grep;
pub(crate) mod process;
pub mod ptc;
pub mod python;
pub mod read;
pub(crate) mod search_report;
pub mod shell;
pub mod web;
pub mod write;

pub use agent::{AGENT_PROGRESS_PREFIX, AgentHost, AgentTool, SubagentRequest};
pub use ask::{
    AskAnswer, AskChannel, AskQuestion, AskTool, ask_choice_selected, render_ask_answers,
    toggle_ask_choice, user_dismissed,
};
pub use edit::EditTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use ptc::{RUN_CODE_EXAMPLE_CODE, RUN_CODE_EXAMPLE_DESCRIPTION, RunCodeTool};
pub use read::ReadTool;
pub use shell::ShellTool;
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
        Arc::new(GrepTool),
        Arc::new(FindTool),
        Arc::new(RunCodeTool),
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
    (text[..text.floor_char_boundary(max_bytes)].to_owned(), true)
}
