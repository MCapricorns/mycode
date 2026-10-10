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
//!   The process tool is `powershell`, `bash`, or `cmd`, whichever matches
//!   the shell resolved at startup. `mode` `script` runs that interpreter
//!   and `mode` `program` spawns a kernel-loadable image with no shell.

pub mod builtin;
pub mod ctx;
pub mod registry;
pub mod roots;
pub mod stream;
pub mod tool;

pub use builtin::fs_io::{FileAccess, PreparedFile, prepare_file_async};
pub use builtin::fs_search::{PreparedSearch, SearchAccess, prepare_search_async_with_access};
pub use builtin::shell::{
    DetectedShell, MYCODE_SHELL_ENV, ShellKind, ShellOverride, active_shell,
    classify_shell_override, detect_default_shell, detect_shell_kind, render_environment_block,
    resolved_shell, script_shell_line, set_runtime_shell,
};
pub use builtin::{ShellTool, register_builtins};
pub use ctx::ToolCtx;
pub use registry::{ToolCatalog, ToolRegistry};
pub use roots::anchor_tool_path;
pub use stream::{ToolProgress, ToolStream, ToolStreamItem, ToolStreamReceiver};
pub use tool::{Tool, ToolDyn, ToolError, ToolResult};
