//! Grep/find still match when ancestor ignore discovery stops at a mount.
use std::fs;
use std::path::{Path, PathBuf};

use crate::builtin::find::{FindArgs, FindTool};
use crate::builtin::grep::{GrepArgs, GrepTool};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolResult};

use super::force_ancestor_mount_boundary;

#[tokio::test]
async fn parent_gitignore_still_applies_on_the_same_mount() {
    let base = unique_dir("mycode-same-mount");
    let project = base.join("project");
    write_project(&base, &project);
    let grep_text = run_grep(&project).await;
    assert!(
        !grep_text.contains("hit.txt"),
        "parent gitignore should hide hit.txt when the mount check does not trip: {grep_text}"
    );
    let find_text = run_find(&project).await;
    assert!(
        !find_text.contains("hit.txt"),
        "parent gitignore should hide hit.txt when the mount check does not trip: {find_text}"
    );
    let _ = fs::remove_dir_all(&base);
}

#[tokio::test]
async fn grep_and_find_match_when_ancestor_mount_check_trips() {
    let base = unique_dir("mycode-ancestor-mount");
    let project = base.join("project");
    write_project(&base, &project);
    let _forced = force_ancestor_mount_boundary(&project).expect("force mount boundary");
    assert_hit_visible(&project).await;
    drop(_forced);
    let _ = fs::remove_dir_all(&base);
}

fn unique_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    fs::create_dir_all(&path).expect("base dir");
    path
}

/// Parent git root ignores `hit.txt`. The workspace `.ignore` hides
/// `hidden.txt`. A search that stops at the mount boundary still returns
/// `hit.txt` and still honors the workspace ignore file.
fn write_project(base: &Path, project: &Path) {
    fs::create_dir_all(project).expect("project");
    fs::create_dir_all(base.join(".git")).expect("parent git");
    fs::write(base.join(".gitignore"), "hit.txt\n").expect("parent gitignore");
    fs::write(base.join("outside.txt"), "needle outside\n").expect("outside");
    fs::write(project.join("hit.txt"), "needle inside\n").expect("hit");
    fs::write(project.join("hidden.txt"), "needle hidden\n").expect("hidden");
    fs::write(project.join(".ignore"), "hidden.txt\n").expect("workspace ignore");
}

async fn assert_hit_visible(project: &Path) {
    let grep_text = run_grep(project).await;
    assert!(
        !grep_text.contains("mount traversal"),
        "grep failed closed on a mount boundary: {grep_text}"
    );
    assert!(
        grep_text.contains("hit.txt"),
        "grep missed the workspace file: {grep_text}"
    );
    assert!(
        !grep_text.contains("hidden.txt"),
        "workspace .ignore was not applied: {grep_text}"
    );
    assert!(
        !grep_text.contains("outside.txt"),
        "search left the workspace: {grep_text}"
    );

    let find_text = run_find(project).await;
    assert!(
        !find_text.contains("mount traversal"),
        "find failed closed on a mount boundary: {find_text}"
    );
    assert!(
        find_text.contains("hit.txt"),
        "find missed the workspace file: {find_text}"
    );
    assert!(
        !find_text.contains("hidden.txt"),
        "workspace .ignore was not applied: {find_text}"
    );
    assert!(
        !find_text.contains("outside.txt"),
        "search left the workspace: {find_text}"
    );
}

async fn run_grep(project: &Path) -> String {
    let ctx = ToolCtx::new(project);
    let (mut out, _rx) = ToolStream::channel();
    let result = GrepTool
        .execute(
            GrepArgs {
                pattern: "needle".to_owned(),
                is_regex: false,
                path: None,
                include: None,
                exclude: None,
                max_results: None,
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("grep");
    assert_search_ok(&result, "grep");
    result_text(&result)
}

async fn run_find(project: &Path) -> String {
    let ctx = ToolCtx::new(project);
    let (mut out, _rx) = ToolStream::channel();
    let result = FindTool
        .execute(
            FindArgs {
                pattern: "*.txt".to_owned(),
                path: None,
                limit: None,
                max_depth: None,
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("find");
    assert_search_ok(&result, "find");
    result_text(&result)
}

fn assert_search_ok(result: &ToolResult, tool: &str) {
    let text = result_text(result);
    assert!(!result.is_error, "{tool} failed: {text}");
}

fn result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}
