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
pub mod read;
pub(crate) mod search_report;
pub mod shell;
pub mod write;

pub use agent::{
    AGENT_COMPACTED_STEP, AGENT_PROGRESS_PREFIX, AgentHost, AgentTool, SubagentRequest,
};
pub use ask::{
    AskAnswer, AskChannel, AskQuestion, AskTool, ask_choice_selected, render_ask_answers,
    toggle_ask_choice, user_dismissed,
};
pub use edit::EditTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use shell::ShellTool;
pub use write::WriteTool;

use std::sync::Arc;

use crate::registry::ToolRegistry;
use crate::tool::ToolDyn;

/// The host-independent builtin tools as type-erased, registry-ready
/// handles. `ask` and `agent` need host plumbing and are registered by
/// their callers.
pub(crate) fn builtin_tools() -> Vec<Arc<dyn ToolDyn>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(ShellTool::default()),
        Arc::new(GrepTool),
        Arc::new(FindTool),
    ]
}

/// Register the host-independent builtin tools into a registry.
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

/// Byte-cap truncation that keeps the start and the end of `text` and
/// drops the middle, for command output whose last lines carry the verdict.
///
/// Returns `(text, truncated)`. The kept parts respect char boundaries and
/// sum to at most `max_bytes`; the marker between them is extra.
pub(crate) fn truncate_middle(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let head_budget = max_bytes * 2 / 5;
    let head_end = text.floor_char_boundary(head_budget);
    let tail_start = text.ceil_char_boundary(text.len() - (max_bytes - head_budget));
    let omitted = tail_start - head_end;
    (
        format!(
            "{}\n[... {omitted} bytes omitted ...]\n{}",
            &text[..head_end],
            &text[tail_start..]
        ),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::truncate_middle;

    #[test]
    fn middle_truncation_keeps_both_ends_on_char_boundaries() {
        assert_eq!(truncate_middle("short", 10), ("short".to_owned(), false));
        let text = format!("{}{}", "头".repeat(40), "尾".repeat(40));
        let (cut, truncated) = truncate_middle(&text, 100);
        assert!(truncated);
        let (head, rest) = cut.split_once("\n[... ").expect("marker");
        let (marker, tail) = rest
            .split_once(" bytes omitted ...]\n")
            .expect("marker end");
        assert!(head.chars().all(|ch| ch == '头') && !head.is_empty());
        assert!(tail.chars().all(|ch| ch == '尾') && !tail.is_empty());
        assert!(head.len() + tail.len() <= 100);
        assert_eq!(
            marker.parse::<usize>().unwrap(),
            text.len() - head.len() - tail.len()
        );
    }
}
