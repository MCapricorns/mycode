//! Tool schemas, dispatch context, file operations, processes and CodeMode.
//!
//! A tool schema drives both model-facing arguments and runtime validation.
//! The registry exposes only the capabilities installed for the current run;
//! CodeMode calls the same dispatch and cancellation paths.

pub mod builtin;
pub mod code_mode;
pub mod ctx;
mod invoke;
pub mod registry;
pub mod roots;
pub mod stream;
pub mod tool;

pub use builtin::fs_io::{FileAccess, PreparedFile, prepare_file_async};
pub use builtin::fs_search::{PreparedSearch, SearchAccess, prepare_search_async_with_access};
pub use builtin::shell::{
    DetectedShell, ShellKind, detect_default_shell, render_environment_block, resolved_shell,
};
pub use builtin::{ShellTool, register_builtins};
pub use code_mode::{
    CODE_MODE_PROGRESS_PREFIX, CellPhase, CellSnapshot, CodeCells, CodeModeProgress,
    InnerCallPhase, InnerCallSummary, JournalEntry, RunCodeTool, ScriptCell, ScriptEvent,
    ScriptEventKind, ScriptJournal, WaitCodeTool, enable_code_mode,
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
