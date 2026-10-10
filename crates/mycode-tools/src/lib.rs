//! `mycode-tools` — the MYCode tool system: `Tool` trait, registry, and
//! canonical builtin tools.
//!
//! ```text
//! model tool_call ──► ToolRegistry::get ──► ToolDyn::execute_dyn
//!                        (ToolDyn)           │ schema-validate args
//!                                            │ execute typed Tool
//!                                            ▼
//!                              ToolResult { content → LLM, details → UI }
//! ```
//!
//! * Every tool derives **one** JSON Schema via `schemars`, used both for
//!   the LLM tool spec and runtime argument validation.
//! * [`ToolRegistry`] is last-wins per name.
//! * A registered, schema-valid call executes directly. Unknown tools,
//!   invalid arguments, cancellation, and tool errors fail as lifecycle
//!   errors, not user authorization.
//! * Trusted builtin tools (read/write/edit/shell/grep/find) provide the
//!   minimal recovery surface and cannot depend on external search binaries.
//!   `shell` is the only process-launch tool: `mode` `script` uses the
//!   platform shell, and `mode` `program` spawns a kernel-loadable image
//!   with no shell.

pub mod builtin;
pub mod ctx;
pub mod registry;
pub mod roots;
pub mod stream;
pub mod tool;

pub use builtin::fs_io::{
    FileAccess, FileRead, FileRevision, FileSnapshot, PreparedFile, prepare_file_async,
    read_file_async, read_file_snapshot_async,
};
pub use builtin::fs_search::{PreparedSearch, SearchAccess, prepare_search_async_with_access};
pub use builtin::shell::{
    DetectedShell, ShellKind, detect_default_shell, detect_shell_kind, set_runtime_shell,
};
pub use builtin::{
    EditTool, FindTool, GrepTool, ReadTool, ShellTool, WriteTool, register_builtins,
};
pub use ctx::ToolCtx;
pub use registry::ToolRegistry;
pub use roots::anchor_tool_path;
pub use stream::{ToolProgress, ToolStream, ToolStreamItem, ToolStreamReceiver};
pub use tool::{Tool, ToolDyn, ToolError, ToolResult};
