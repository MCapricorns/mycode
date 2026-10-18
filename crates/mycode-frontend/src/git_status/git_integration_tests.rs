//! End-to-end panel reads against a real git repository.
//!
//! These tests require a `git` binary, exactly like the changes panel, and
//! pin the `-z` porcelain contract used by `read_status` plus the added-file
//! diff fallbacks in `read_diff`.

use super::{read_diff, read_status};
use std::path::PathBuf;
use std::process::Command;

struct Repo(PathBuf);

impl Repo {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "mycode-git-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        let repo = Self(root);
        repo.git(&["init", "-q"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(["-c", "user.email=panel@example.invalid"])
            .args(["-c", "user.name=Panel Test"])
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn write(&self, path: &str, body: &str) {
        let file = self.0.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(file, body).expect("write");
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git_present() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn fresh_repo_staged_and_untracked_files_show_added_line_diffs() {
    if !git_present() {
        return;
    }
    let repo = Repo::new("fresh");
    repo.write("staged.txt", "alpha\n");
    repo.write("untracked.txt", "beta\n");
    repo.git(&["add", "staged.txt"]);

    // No HEAD commit exists yet, so `git diff HEAD` fails and the reader
    // falls back to added-line diffs from the index and the worktree.
    let staged = read_diff(&repo.0, "staged.txt");
    assert!(staged.contains("new file mode"), "{staged}");
    assert!(staged.contains("+alpha"), "{staged}");

    let untracked = read_diff(&repo.0, "untracked.txt");
    assert!(untracked.contains("+beta"), "{untracked}");
}

#[test]
fn a_staged_rename_reports_the_destination_and_the_previous_path() {
    if !git_present() {
        return;
    }
    let repo = Repo::new("rename");
    repo.write("old.txt", "same body\n");
    repo.git(&["add", "old.txt"]);
    repo.git(&["commit", "-q", "-m", "seed"]);
    repo.git(&["mv", "old.txt", "new.txt"]);

    let snapshot = read_status(&repo.0);
    // `init.defaultBranch` may name the branch differently; the panel only
    // displays it, so any nonempty spelling passes.
    assert!(!snapshot.branch.is_empty(), "{snapshot:?}");
    let renamed = snapshot
        .files
        .iter()
        .find(|file| file.status == "R")
        .expect("rename row");
    assert_eq!(renamed.path, "new.txt");
    assert_eq!(renamed.previous.as_deref(), Some("old.txt"));

    let diff = read_diff(&repo.0, "new.txt");
    assert!(diff.contains("diff --git"), "{diff}");
}

#[test]
fn non_ascii_filenames_survive_the_round_trip() {
    if !git_present() {
        return;
    }
    let repo = Repo::new("unicode");
    repo.write("中文 目录/note 文件.txt", "内容\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "seed"]);
    repo.write("中文 目录/note 文件.txt", "内容 changed\n");

    let snapshot = read_status(&repo.0);
    assert!(
        snapshot
            .files
            .iter()
            .any(|file| file.path == "中文 目录/note 文件.txt"),
        "{:?}",
        snapshot.files
    );

    let diff = read_diff(&repo.0, "中文 目录/note 文件.txt");
    assert!(diff.contains("+内容 changed"), "{diff}");
}

#[test]
fn opening_a_subdirectory_still_resolves_repository_paths() {
    if !git_present() {
        return;
    }
    let repo = Repo::new("subdir");
    repo.write("nested/file.txt", "one\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "seed"]);
    repo.write("nested/file.txt", "two\n");

    // The panel roots every read at the repository top level, so paths stay
    // valid no matter which folder the user opened.
    let subdir = repo.0.join("nested");
    let snapshot = read_status(&subdir);
    assert!(
        snapshot
            .files
            .iter()
            .any(|file| file.path == "nested/file.txt"),
        "{:?}",
        snapshot.files
    );

    let diff = read_diff(&subdir, "nested/file.txt");
    assert!(diff.contains("+two"), "{diff}");
}
