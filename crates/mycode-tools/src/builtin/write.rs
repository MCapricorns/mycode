//! `write` — create or replace a UTF-8 text file through the host file kernel.
//!
//! Missing targets are atomic create-only (missing parents are created
//! safely). Existing targets require `expected_revision` or `overwrite=true`.
//! Hidden files are writable. Unconditional overwrite is not the default.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write as _;

use crate::builtin::fs_io::{FileAccess, anchor_file_path, write_file_with_lease};
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
    /// session cwd; writable absolute paths stay inside the session cwd,
    /// an added workspace root, or the OS temp directory. Missing parent
    /// directories are created.
    pub path: String,
    /// The complete new content of the file.
    pub content: String,
    /// Required when the file already exists (unless `overwrite` is true):
    /// the `mycode-rev1-…` token from the `[revision …]` line of your latest
    /// `read`, `edit`, or `write` of this file. Omit for a new file.
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

impl Tool for WriteTool {
    type Args = WriteArgs;
    type Output = ();

    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write a UTF-8 text file inside the session cwd, an added workspace \
         root, or the OS temp directory. Replacing an EXISTING file requires \
         either `expected_revision` (the token from the `[revision …]` line \
         of your latest read/edit/write of that file) or `overwrite: true`; \
         without one the call is refused. The two cannot be combined. New \
         files need neither and are created atomically (parents created as \
         needed). Prefer `edit` for partial changes. Hidden files are \
         writable. Does not follow symlinks or reparse points."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "write: create a new file, or replace an existing one in full — \
             replacing requires expected_revision (from the file's latest \
             `[revision …]` line) or overwrite=true. Prefer edit for partial \
             changes.",
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
        let (root, path) = anchor_file_path(
            &ctx.cwd,
            &ctx.extra_roots,
            &ctx.read_roots,
            false,
            &args.path,
        );
        let outcome = write_file_with_lease(
            ctx.prepared_file.clone(),
            root,
            path,
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
        let _ = write!(text, "\n[revision {}]", outcome.revision);
        Ok(ToolResult::text(text).with_details(json!({
            "path": outcome.path_key,
            "bytes_written": outcome.bytes_written,
            "revision": outcome.revision.as_str(),
            "detached_hardlink": outcome.detached_hardlink,
        })))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{WriteArgs, WriteTool};
    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::Tool;

    #[tokio::test]
    async fn write_without_preflight_anchors_extra_roots_and_refuses_read_roots() {
        let base = std::env::temp_dir().join(format!(
            "mycode-write-roots-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let cwd = base.join("ws");
        let extra = base.join("extra");
        let read_only = base.join("skills");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&extra).expect("extra");
        std::fs::create_dir_all(&read_only).expect("skills");
        let extra_file = extra.join("out.txt");
        let skill_file = read_only.join("SKILL.md");

        let ctx = ToolCtx::new(&cwd)
            .with_extra_roots(vec![PathBuf::from(&extra)])
            .with_read_roots(vec![PathBuf::from(&read_only)]);
        let (mut stream, _rx) = ToolStream::channel();
        let res = WriteTool
            .execute(
                WriteArgs {
                    path: extra_file.display().to_string(),
                    content: "written\n".to_owned(),
                    expected_revision: None,
                    overwrite: false,
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("write under extra_roots");
        assert!(!res.is_error);
        assert_eq!(
            std::fs::read_to_string(&extra_file).expect("read back"),
            "written\n"
        );

        let err = WriteTool
            .execute(
                WriteArgs {
                    path: skill_file.display().to_string(),
                    content: "nope\n".to_owned(),
                    expected_revision: None,
                    overwrite: false,
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect_err("write under read_roots must fail closed");
        assert!(err.to_string().contains("escapes the session cwd"), "{err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn replacing_without_a_revision_names_the_file_and_both_remedies() {
        let cwd = std::env::temp_dir().join(format!(
            "mycode-write-replace-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&cwd).expect("cwd");
        let ctx = ToolCtx::new(&cwd);
        let (mut stream, _rx) = ToolStream::channel();
        let args = |overwrite: bool| WriteArgs {
            path: "notes.txt".to_owned(),
            content: "v\n".to_owned(),
            expected_revision: None,
            overwrite,
        };
        WriteTool
            .execute(args(false), &ctx, &mut stream)
            .await
            .expect("create");
        // Models that skip the revision must learn the fix from the error.
        let err = WriteTool
            .execute(args(false), &ctx, &mut stream)
            .await
            .expect_err("replace without revision");
        let text = err.to_string();
        for needle in ["notes.txt", "expected_revision", "[revision", "overwrite=true"] {
            assert!(text.contains(needle), "{needle} missing from: {text}");
        }
        WriteTool
            .execute(args(true), &ctx, &mut stream)
            .await
            .expect("overwrite");
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
