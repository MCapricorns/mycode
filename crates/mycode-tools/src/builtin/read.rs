//! `read` — read a UTF-8 text file with optional line windowing and
//! output truncation (pi-style: ~2000 lines / 50 KiB with a notice).
//!
//! Execution uses the host file kernel: a prepared handle-relative capability,
//! chunked read under scan/line/deadline/cancel caps, UTF-8 (BOM stripped
//! from displayed text; hash of raw bytes), and an opaque revision token.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::builtin::fs_io::{FileAccess, anchor_file_path, read_file_async};
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
    /// line). Values below 1 are rejected.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub offset: Option<usize>,
    /// Maximum number of lines to return (default: all, up to the
    /// tool's output cap).
    pub limit: Option<usize>,
}

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

    fn reads_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let (root, path) = anchor_file_path(
            &ctx.cwd,
            &ctx.extra_roots,
            &ctx.read_roots,
            true,
            &args.path,
        );
        let outcome = read_file_async(
            ctx.prepared_file.clone(),
            root,
            path,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{ReadArgs, ReadTool};
    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolError, validate_args};

    #[test]
    fn offset_below_one_is_rejected_by_the_schema() {
        let error = validate_args::<ReadArgs>(&json!({"path": "a.txt", "offset": 0}))
            .expect_err("offset 0");
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        validate_args::<ReadArgs>(&json!({"path": "a.txt", "offset": 1})).expect("offset 1");
        validate_args::<ReadArgs>(&json!({"path": "a.txt"})).expect("omitted offset");
    }

    #[tokio::test]
    async fn read_without_preflight_anchors_extra_and_read_roots() {
        let base = std::env::temp_dir().join(format!(
            "mycode-read-roots-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let cwd = base.join("ws");
        let extra = base.join("extra");
        let skill = base.join("skills");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&extra).expect("extra");
        std::fs::create_dir_all(&skill).expect("skill");
        let extra_file = extra.join("a.txt");
        let skill_file = skill.join("SKILL.md");
        std::fs::write(&extra_file, "from extra\n").expect("write extra");
        std::fs::write(&skill_file, "from skill\n").expect("write skill");

        let ctx = ToolCtx::new(&cwd)
            .with_extra_roots(vec![PathBuf::from(&extra)])
            .with_read_roots(vec![PathBuf::from(&skill)]);
        let (mut stream, _rx) = ToolStream::channel();
        let extra_res = ReadTool
            .execute(
                ReadArgs {
                    path: extra_file.display().to_string(),
                    offset: None,
                    limit: None,
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("read under extra_roots");
        assert!(!extra_res.is_error);

        let skill_res = ReadTool
            .execute(
                ReadArgs {
                    path: skill_file.display().to_string(),
                    offset: None,
                    limit: None,
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("read under read_roots");
        assert!(!skill_res.is_error);

        let _ = std::fs::remove_dir_all(&base);
    }
}
