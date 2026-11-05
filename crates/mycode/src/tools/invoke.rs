//! Shared tool preflight for every tool execution.
//!
//! The path is anchored, the file or search capability is bound, and the
//! same tool executes. There is no separate permission prompt: a
//! schema-valid call runs, and paths are not confined to the workspace.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::tools::ctx::ToolCtx;
use crate::tools::roots::{anchor_file_path, anchor_tool_path};
use crate::tools::tool::ToolDyn;

/// Builds the [`ToolCtx`] a tool executes with, including filesystem preflight.
///
/// `read_roots` are host-published anchor roots outside the workspace (the
/// skills catalog). Only a tool whose whole file access is a read anchors
/// against them, which keeps its path key relative to that root; any other
/// path outside the workspace is bound from its resolved location.
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
    let mut args = crate::tools::tool::normalize_tool_args(args.clone());
    crate::tools::tool::repair_args(&mut args, &tool.spec().params_schema);
    if let Some(access) = tool.search_access() {
        let path = args.get("path").and_then(Value::as_str).map(str::to_owned);
        let (root, path) = anchored_search(cwd, extra_roots, path.as_deref());
        let prepared =
            crate::tools::prepare_search_async_with_access(root.clone(), path, cancel, access)
                .await
                .map_err(|error| error.to_string())?;
        ctx.cwd = root;
        ctx = ctx.with_prepared_search(std::sync::Arc::new(prepared));
    } else if let Some(access) = tool.file_access() {
        // Bind the same path that execute_dyn will use after schema-aware
        // alias repair (for example `file` or `target_file` -> `path`).
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "file tool is missing a path argument".to_owned())?;
        let (root, path) = anchor_file_path(cwd, extra_roots, read_roots, tool.reads_only(), path);
        let prepared = crate::tools::prepare_file_async(root.clone(), path, cancel, access)
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
    use crate::tools::builtin::{EditTool, ReadTool};

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
            crate::core::unix_now().as_nanos()
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
    async fn file_aliases_bind_the_same_path_before_execution() {
        let workspace = temp("path-aliases");
        fs::write(workspace.0.join("note.txt"), "alias content\n").unwrap();
        for args in [
            json!({"file": "note.txt"}),
            json!({"target_file": "note.txt"}),
            json!(r#"{"filePath":"note.txt"}"#),
        ] {
            let text = run(&workspace.0, &ReadTool, args).await;
            assert!(text.contains("alias content"), "{text}");
        }
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
    async fn read_reaches_paths_outside_every_root() {
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
        let ctx = ctx.expect("read outside every root must prepare");
        let key = ctx
            .prepared_file
            .as_ref()
            .expect("prepared")
            .key()
            .to_owned();
        assert!(
            std::path::Path::new(&key).is_absolute() && key.ends_with("skills/probe/SKILL.md"),
            "an outside read is keyed by its absolute spelling: {key}"
        );
    }

    #[tokio::test]
    async fn edit_reaches_files_under_read_roots() {
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
        assert!(ctx.is_ok(), "edit is not confined to the workspace");
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
        let mut stream = crate::tools::stream::ToolStream::closed();
        let result = crate::tools::tool::ToolDyn::execute_dyn(&EditTool, args, &ctx, &mut stream)
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
            &crate::tools::builtin::FindTool,
            &find_args,
        )
        .await
        .expect("prepare find in extra root");
        assert_eq!(find_ctx.cwd, extra.0);
    }

    fn result_text(result: &crate::tools::tool::ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                crate::core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    async fn run(
        workspace: &std::path::Path,
        tool: &dyn crate::tools::tool::ToolDyn,
        args: serde_json::Value,
    ) -> String {
        let ctx = prepare_tool_ctx(
            workspace,
            &[],
            &[],
            CancellationToken::new(),
            "call",
            tool,
            &args,
        )
        .await
        .expect("an outside path must prepare");
        let mut stream = crate::tools::stream::ToolStream::closed();
        let result = tool
            .execute_dyn(args, &ctx, &mut stream)
            .await
            .expect("an outside path must execute");
        let text = result_text(&result);
        assert!(!result.is_error, "{text}");
        text
    }

    #[tokio::test]
    async fn search_and_parent_relative_paths_reach_outside_the_workspace() {
        let base = temp("outside");
        let workspace = base.0.join("ws");
        let other = base.0.join("other");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(other.join("src")).unwrap();
        fs::write(other.join("src").join("lib.rs"), "fn probe() {}\n").unwrap();
        let other_path = other.display().to_string();

        let grep = run(
            &workspace,
            &crate::tools::builtin::GrepTool,
            json!({"pattern": "probe", "path": other_path}),
        )
        .await;
        assert!(grep.contains("lib.rs:1:"), "{grep}");

        let find = run(
            &workspace,
            &crate::tools::builtin::FindTool,
            json!({"pattern": "*.rs", "path": other_path}),
        )
        .await;
        assert!(find.contains("lib.rs"), "{find}");

        let read = run(
            &workspace,
            &ReadTool,
            json!({"path": "../other/src/lib.rs"}),
        )
        .await;
        assert!(read.contains("fn probe() {}"), "{read}");
    }
}
