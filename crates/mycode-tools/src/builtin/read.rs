//! `read` — read a UTF-8 text file with optional line windowing and
//! output truncation (pi-style: ~2000 lines / 50 KiB with a notice), or hand
//! a picture to a model that can see one.
//!
//! Execution uses the host file kernel: a prepared handle-relative capability,
//! chunked read under scan/line/deadline/cancel caps, UTF-8 (BOM stripped
//! from displayed text; hash of raw bytes), and an opaque revision token.
//! Pictures are detected by their magic bytes and returned as an image block,
//! the same way OpenCode's `read` tool presents media directly to the model.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use mycode_core::{BinaryData, ContentBlock, MAX_IMAGE_BASE64_BYTES, TextBlock};

use crate::builtin::fs_io::{FileAccess, ReadOutcome, anchor_file_path, read_any_async};
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
    /// line). Values below 1 are rejected. Text only.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub offset: Option<usize>,
    /// Maximum number of lines to return (default: all, up to the
    /// tool's output cap). Text only.
    pub limit: Option<usize>,
}

impl Tool for ReadTool {
    type Args = ReadArgs;
    type Output = ();

    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the local filesystem. Text files come back with each \
         line prefixed by its 1-based number; use offset/limit to window a \
         large file, and output is truncated with a notice beyond 2000 lines \
         or 50 KiB per call. Pictures (PNG, JPEG, GIF, WebP) are presented \
         directly to the model instead of as text, so a screenshot saved to \
         disk can be read back and inspected; offset/limit do not apply to \
         them. Hidden files are readable. \
         Returns an opaque revision token for later writes."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "read: fetch file contents (path, optional 1-based offset / limit); \
             images are shown to the model.",
        )
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
        let outcome = read_any_async(
            ctx.prepared_file.clone(),
            root,
            path,
            args.offset,
            args.limit,
            ctx.cancel.clone(),
        )
        .await?;
        match outcome {
            ReadOutcome::Text(outcome) => {
                Ok(ToolResult::text(outcome.displayed).with_details(json!({
                    "path": outcome.path_key,
                    "total_lines": outcome.total_lines,
                    "returned_lines": outcome.returned_lines,
                    "truncated": outcome.truncated,
                    "revision": outcome.revision.as_str(),
                })))
            }
            ReadOutcome::Image { bytes, mime, key } => image_result(&key, &bytes, mime),
        }
    }
}

/// One picture as a text note plus the image block the adapter forwards.
fn image_result(key: &str, bytes: &[u8], mime: &str) -> Result<ToolResult, ToolError> {
    let data = BASE64_STANDARD.encode(bytes);
    if data.len() > MAX_IMAGE_BASE64_BYTES {
        return Err(ToolError::Execution(format!(
            "{key}: encoded image is {} bytes, over the {MAX_IMAGE_BASE64_BYTES}-byte \
             limit; crop or resize it to a smaller picture first",
            data.len()
        )));
    }
    let content = vec![
        ContentBlock::Text(TextBlock::new(format!(
            "Image: {key} ({mime}, {} bytes). The picture is presented directly to you.",
            bytes.len()
        ))),
        ContentBlock::Image(BinaryData {
            data,
            mime_type: mime.to_owned(),
        }),
    ];
    Ok(ToolResult {
        is_error: false,
        content,
        details: Some(json!({
            "path": key,
            "mime": mime,
            "bytes": bytes.len(),
        })),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use base64::Engine as _;
    use serde_json::json;

    use super::{ReadArgs, ReadTool};
    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolError, validate_args};
    use mycode_core::ContentBlock;

    #[test]
    fn offset_below_one_is_rejected_by_the_schema() {
        let error = validate_args::<ReadArgs>(&json!({"path": "a.txt", "offset": 0}))
            .expect_err("offset 0");
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        validate_args::<ReadArgs>(&json!({"path": "a.txt", "offset": 1})).expect("offset 1");
        validate_args::<ReadArgs>(&json!({"path": "a.txt"})).expect("omitted offset");
    }

    #[tokio::test]
    async fn an_image_file_comes_back_as_an_image_block() {
        let base = std::env::temp_dir().join(format!(
            "mycode-read-image-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&base).expect("workspace");
        let path = base.join("shot.png");
        // A real PNG signature is all the tool inspects; the bytes are not
        // decoded here, the provider decodes them.
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        std::fs::write(&path, png).expect("write png");

        let ctx = ToolCtx::new(&base);
        let (mut stream, _rx) = ToolStream::channel();
        let result = ReadTool
            .execute(
                ReadArgs {
                    path: path.display().to_string(),
                    offset: None,
                    limit: None,
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("read image");
        assert!(!result.is_error, "{result:?}");
        let image = result
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Image(image) => Some(image),
                _ => None,
            })
            .expect("image block");
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(
            image.data,
            base64::engine::general_purpose::STANDARD.encode(png)
        );
        assert_eq!(result.details.expect("details")["bytes"], png.len());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn an_image_refuses_a_line_window() {
        let base = std::env::temp_dir().join(format!(
            "mycode-read-image-window-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&base).expect("workspace");
        let path = base.join("shot.png");
        std::fs::write(
            &path,
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0],
        )
        .expect("write png");

        let ctx = ToolCtx::new(&base);
        let (mut stream, _rx) = ToolStream::channel();
        let error = ReadTool
            .execute(
                ReadArgs {
                    path: path.display().to_string(),
                    offset: Some(1),
                    limit: Some(5),
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect_err("offset on an image");
        assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");

        let _ = std::fs::remove_dir_all(&base);
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
