//! `mycode-tools` — the `Tool` trait, registry, and builtin tools.
//!
//! One JSON Schema per tool (schemars) serves both the LLM tool spec and
//! runtime argument validation; [`ToolRegistry`] is last-wins per name.
//! Trusted builtins (read/write/edit/shell/grep/find) provide the minimal
//! recovery surface and spawn no external search binaries.

pub mod builtin;
pub mod code_mode;
pub mod ctx;
mod invoke;
pub mod registry;
pub mod roots;
pub mod stream;
pub mod tool;

// Chain re-export: successors see every lower layer through this crate only.
pub use mycode_providers::*;

pub use builtin::fs_io::{FileAccess, PreparedFile, prepare_file_async};
pub use builtin::fs_search::{PreparedSearch, SearchAccess, prepare_search_async_with_access};
pub use builtin::shell::{
    DetectedShell, ShellKind, detect_default_shell, render_environment_block, resolved_shell,
};
pub use builtin::{ShellTool, register_builtins};
pub use code_mode::{
    CODE_MODE_PROGRESS_PREFIX, CellPhase, CellSnapshot, CodeCellState, CodeCells, CodeModeProgress,
    InnerCallPhase, InnerCallSummary, JournalEntry, RunCodeTool, ScriptCell, ScriptEvent,
    ScriptEventKind, ScriptJournal, WaitCodeTool,
};
pub use ctx::ToolCtx;
pub use invoke::prepare_tool_ctx;
pub use registry::ToolRegistry;
pub use roots::{anchor_tool_path, os_scratch_root};
pub use stream::{ToolProgress, ToolStream, ToolStreamItem, ToolStreamReceiver};
pub use tool::{Tool, ToolDyn, ToolError, ToolResult};

/// Windows stdio child resumed from `PROCESS_INFORMATION.hThread`.
#[cfg(windows)]
pub use builtin::process::WindowsStdioProcess;
pub use builtin::process::first_program_on_path;
