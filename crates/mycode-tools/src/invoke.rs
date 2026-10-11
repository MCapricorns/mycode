//! Shared tool preflight used by a direct call and by a `run_code` inner call.
//!
//! Both paths anchor the path, bind the file or search capability, and then
//! execute the same tool. There is no separate permission prompt: a
//! schema-valid call runs, and a path outside the workspace fails here.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ctx::ToolCtx;
use crate::roots::anchor_tool_path;
use crate::tool::ToolDyn;

/// Builds the [`ToolCtx`] a tool executes with, including filesystem preflight.
///
/// # Errors
///
/// Returns the preflight error text when a declared file or search root
/// cannot be prepared. The caller turns that into a tool error.
pub async fn prepare_tool_ctx(
    cwd: &Path,
    extra_roots: &[PathBuf],
    cancel: CancellationToken,
    call_id: impl Into<String>,
    tool: &dyn ToolDyn,
    args: &Value,
) -> Result<ToolCtx, String> {
    let mut ctx = ToolCtx::new(cwd.to_path_buf())
        .with_cancel(cancel.clone())
        .with_call_id(call_id)
        .with_extra_roots(extra_roots.to_vec());
    if let Some(access) = tool.search_access() {
        let path = args.get("path").and_then(Value::as_str).map(str::to_owned);
        let (root, path) = anchored_search(cwd, extra_roots, path.as_deref());
        let prepared = crate::prepare_search_async_with_access(root, path, cancel, access)
            .await
            .map_err(|error| error.to_string())?;
        ctx = ctx.with_prepared_search(std::sync::Arc::new(prepared));
    } else if let Some(access) = tool.file_access() {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "file tool is missing a path argument".to_owned())?;
        let (root, path) = anchored_file(cwd, extra_roots, path);
        let prepared = crate::prepare_file_async(root, path, cancel, access)
            .await
            .map_err(|error| error.to_string())?;
        ctx = ctx.with_prepared_file(std::sync::Arc::new(prepared));
    }
    Ok(ctx)
}

fn anchored_search(cwd: &Path, extras: &[PathBuf], raw: Option<&str>) -> (PathBuf, Option<String>) {
    let Some(raw) = raw else {
        return (cwd.to_path_buf(), None);
    };
    let (root, relative) = anchor_tool_path(cwd, extras, raw);
    if root == cwd {
        return (cwd.to_path_buf(), Some(raw.to_owned()));
    }
    if relative.is_empty() {
        (root, None)
    } else {
        (root, Some(relative))
    }
}

fn anchored_file(cwd: &Path, extras: &[PathBuf], raw: &str) -> (PathBuf, String) {
    let (root, relative) = anchor_tool_path(cwd, extras, raw);
    if root == cwd {
        (cwd.to_path_buf(), raw.to_owned())
    } else if relative.is_empty() {
        (root, ".".to_owned())
    } else {
        (root, relative)
    }
}
