//! `read` — read a UTF-8 text file with optional line windowing and
//! output truncation (pi-style: ~2000 lines / 50 KiB with a notice).
//!
//! Execution uses the host file kernel: a prepared handle-relative capability,
//! chunked read under scan/line/deadline/cancel caps, UTF-8 (BOM stripped
//! from displayed text; hash of raw bytes), and an opaque revision token.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::builtin::fs_io::{FileAccess, read_file_async};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// The `read` builtin.
pub struct ReadTool;

/// Arguments for [`ReadTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// Path of the file to read. Relative paths resolve against the
    /// session cwd.
    pub path: String,
    /// 1-based line number to start reading from (default: the first
    /// line).
    pub offset: Option<usize>,
    /// Maximum number of lines to return (default: all, up to the
    /// tool's output cap).
    pub limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    type Args = ReadArgs;
    type Output = ();

    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the local filesystem. Use offset/limit to \
         window into large files; output is truncated with a notice beyond \
         2000 lines or 50 KiB per call. Hidden files are readable. Returns an \
         opaque revision token for later writes."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("read: fetch file contents (path, optional 1-based offset / limit).")
    }

    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingContent)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let outcome = read_file_async(
            ctx.prepared_file.clone(),
            ctx.cwd.clone(),
            args.path,
            args.offset,
            args.limit,
            ctx.cancel.clone(),
        )
        .await?;
        Ok(ToolResult::text(outcome.displayed).with_details(json!({
            "path": outcome.path_key,
            "total_lines": outcome.total_lines,
            "returned_lines": outcome.returned_lines,
            "truncated": outcome.truncated,
            "revision": outcome.revision.as_str(),
        })))
    }
}
