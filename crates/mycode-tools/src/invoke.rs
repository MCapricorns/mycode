//! Shared tool preflight for every tool execution.
//!
//! The path is anchored, the file or search capability is bound, and the
//! same tool executes. There is no separate permission prompt: a
//! schema-valid call runs, and a path outside the workspace fails here.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ctx::ToolCtx;
use crate::roots::{anchor_file_path, anchor_tool_path};
use crate::tool::ToolDyn;

/// Builds the [`ToolCtx`] a tool executes with, including filesystem preflight.
///
/// `read_roots` are host-published read-only anchor roots outside the
/// workspace (the skills catalog). Only a tool whose whole file access is
/// a read anchors against them; `edit` and `write` keep workspace-only
/// anchoring, so a read root can never be written through a tool.
///
/// # Errors
///
/// Returns the preflight error text when a declared file or search root
/// cannot be prepared. The caller turns that into a tool error.
pub async fn prepare_tool_ctx(
    cwd: &Path,
    extra_roots: &[PathBuf],
    read_roots: &[PathBuf],
    cancel: CancellationToken,
    call_id: impl Into<String>,
    tool: &dyn ToolDyn,
    args: &Value,
) -> Result<ToolCtx, String> {
    let mut ctx = ToolCtx::new(cwd.to_path_buf())
        .with_cancel(cancel.clone())
        .with_call_id(call_id)
        .with_extra_roots(extra_roots.to_vec())
        .with_read_roots(read_roots.to_vec());
    let args = crate::tool::normalize_tool_args(args.clone());
    if let Some(access) = tool.search_access() {
        let path = args.get("path").and_then(Value::as_str).map(str::to_owned);
        let (root, path) = anchored_search(cwd, extra_roots, path.as_deref());
        let prepared = crate::prepare_search_async_with_access(root.clone(), path, cancel, access)
            .await
            .map_err(|error| error.to_string())?;
        ctx.cwd = root;
        ctx = ctx.with_prepared_search(std::sync::Arc::new(prepared));
    } else if let Some(access) = tool.file_access() {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "file tool is missing a path argument".to_owned())?;
        let (root, path) =
            anchor_file_path(cwd, extra_roots, read_roots, tool.reads_only(), path);
        let prepared = crate::prepare_file_async(root.clone(), path, cancel, access)
            .await
            .map_err(|error| error.to_string())?;
        ctx.cwd = root;
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::prepare_tool_ctx;
    use crate::builtin::{EditTool, ReadTool};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp(label: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "mycode-invoke-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn skill_file(home: &std::path::Path) -> String {
        let dir = home.join(".agents").join("skills").join("probe");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        fs::write(
            &path, "# probe
",
        )
        .unwrap();
        path.display().to_string()
    }

    #[tokio::test]
    async fn read_anchors_on_read_roots_outside_cwd() {
        let workspace = temp("ws");
        let home = temp("home");
        let path = skill_file(&home.0);
        let roots = vec![
            home.0.join(".agents"),
            home.0.join(".agents").join("skills"),
        ];
        let args = json!({"path": path});
        let ctx = prepare_tool_ctx(
            &workspace.0,
            &[],
            &roots,
            CancellationToken::new(),
            "call",
            &ReadTool,
            &args,
        )
        .await;
        assert!(ctx.is_ok(), "read under a read root must prepare");
    }

    #[tokio::test]
    async fn read_still_refuses_paths_outside_every_root() {
        let workspace = temp("ws2");
        let home = temp("home2");
        let path = skill_file(&home.0);
        let args = json!({"path": path});
        let ctx = prepare_tool_ctx(
            &workspace.0,
            &[],
            &[],
            CancellationToken::new(),
            "call",
            &ReadTool,
            &args,
        )
        .await;
        assert!(ctx.is_err(), "read without the root must fail closed");
    }

    #[tokio::test]
    async fn edit_never_anchors_on_read_roots() {
        let workspace = temp("ws3");
        let home = temp("home3");
        let path = skill_file(&home.0);
        let roots = vec![
            home.0.join(".agents"),
            home.0.join(".agents").join("skills"),
        ];
        let args = json!({"path": path, "old_string": "a", "new_string": "b"});
        let ctx = prepare_tool_ctx(
            &workspace.0,
            &[],
            &roots,
            CancellationToken::new(),
            "call",
            &EditTool,
            &args,
        )
        .await;
        assert!(
            ctx.is_err(),
            "edit shares the read root spelling but must stay workspace-bound"
        );
    }

    #[tokio::test]
    async fn extra_root_updates_ctx_cwd_so_edit_and_search_execute_against_anchor() {
        let workspace = temp("ws-extra");
        let extra = temp("extra-root");
        let target = extra.0.join("sub").join("note.txt");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "alpha\n").unwrap();
        let extras = vec![extra.0.clone()];
        let args = json!({
            "path": target.display().to_string(),
            "old_string": "alpha",
            "new_string": "beta"
        });
        let ctx = prepare_tool_ctx(
            &workspace.0,
            &extras,
            &[],
            CancellationToken::new(),
            "call",
            &EditTool,
            &args,
        )
        .await
        .expect("prepare edit in extra root");
        assert_eq!(ctx.cwd, extra.0);
        let mut stream = crate::stream::ToolStream::closed();
        let result = crate::tool::ToolDyn::execute_dyn(&EditTool, args, &ctx, &mut stream)
            .await
            .expect("edit in extra root must succeed");
        assert!(!result.is_error);
        assert_eq!(fs::read_to_string(&target).unwrap(), "beta\n");

        let find_args = json!({
            "pattern": "*.txt",
            "path": extra.0.display().to_string()
        });
        let find_ctx = prepare_tool_ctx(
            &workspace.0,
            &extras,
            &[],
            CancellationToken::new(),
            "call",
            &crate::builtin::FindTool,
            &find_args,
        )
        .await
        .expect("prepare find in extra root");
        assert_eq!(find_ctx.cwd, extra.0);
    }
}
