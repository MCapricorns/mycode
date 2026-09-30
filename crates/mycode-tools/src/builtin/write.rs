//! `write` — create or replace a UTF-8 text file through the host file kernel.
//!
//! Missing targets are atomic create-only (missing parents are created
//! safely). Existing targets require `expected_revision` or `overwrite=true`.
//! Hidden files are writable. Unconditional overwrite is not the default.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::builtin::fs_io::{FileAccess, write_file_with_lease};
use crate::builtin::process::acquire_execution_lease;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// The `write` builtin.
pub struct WriteTool;

/// Arguments for [`WriteTool`].
#[derive(Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Path of the file to write. Relative paths resolve against the
    /// session cwd; missing parent directories are created.
    pub path: String,
    /// The complete new content of the file.
    pub content: String,
    /// Opaque revision from a prior `read`. Required to replace an existing
    /// file unless `overwrite` is true.
    pub expected_revision: Option<String>,
    /// Replace an existing file without a revision check. Default false.
    /// Cannot be combined with `expected_revision`.
    #[serde(default)]
    pub overwrite: bool,
}

impl std::fmt::Debug for WriteArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteArgs")
            .field("path", &self.path)
            .field("content", &"<redacted>")
            .field("expected_revision", &self.expected_revision)
            .field("overwrite", &self.overwrite)
            .finish()
    }
}

#[async_trait]
impl Tool for WriteTool {
    type Args = WriteArgs;
    type Output = ();

    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a UTF-8 text file inside the session cwd. Missing files are \
         created atomically (parents created as needed). Existing files are \
         replaced only when `expected_revision` matches a prior read, or \
         `overwrite` is true. The two options cannot be combined. Hidden \
         files are writable. Does not follow symlinks or reparse points."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "write: create or replace a file (path, content, optional \
             expected_revision/overwrite).",
        )
    }

    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingOrMissing)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let lease = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                return Err(ToolError::Execution("write cancelled before execution".into()));
            }
            lease = acquire_execution_lease() => lease,
        };
        let outcome = write_file_with_lease(
            ctx.prepared_file.clone(),
            ctx.cwd.clone(),
            args.path,
            args.content,
            args.expected_revision,
            args.overwrite,
            lease,
            ctx.cancel.clone(),
        )
        .await?;
        let mut text = format!(
            "Wrote {} bytes to {}",
            outcome.bytes_written, outcome.path_key
        );
        if outcome.detached_hardlink {
            text.push_str(" (detached_hardlink=true: this directory entry now names a new inode)");
        }
        text.push_str(&format!("\n[revision {}]", outcome.revision));
        Ok(ToolResult::text(text).with_details(json!({
            "path": outcome.path_key,
            "bytes_written": outcome.bytes_written,
            "revision": outcome.revision.as_str(),
            "detached_hardlink": outcome.detached_hardlink,
        })))
    }
}
