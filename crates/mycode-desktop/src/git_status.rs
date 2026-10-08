//! Working-tree snapshot for the changes panel.
//!
//! Reads `git status` and `git diff` in the session folder. A missing git
//! install or a non-repository is a panel note, not a chat error.

use crate::i18n::t;

use std::path::Path;
use std::process::Command;

/// `git` with no console window. The desktop process has no console, so a
/// plain spawn of `git.exe` allocates a black window on Windows.
fn git_command() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: do not allocate a console for this child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new("git");
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new("git")
    }
}

/// One dirty path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitFile {
    pub path: String,
    pub status: String,
}

/// Branch plus dirty files for the open folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitSnapshot {
    pub branch: String,
    pub files: Vec<GitFile>,
    pub note: Option<String>,
}

impl GitSnapshot {
    pub(crate) fn empty(note: impl Into<String>) -> Self {
        Self {
            branch: String::new(),
            files: Vec::new(),
            note: Some(note.into()),
        }
    }
}

/// `git status --porcelain=v1 -b` for one directory.
///
/// `--no-optional-locks` keeps a refresh from taking `.git/index.lock` and
/// opportunistically rewriting the index, which would contend with the user's
/// own git commands and subagent worktree updates.
pub(crate) fn read_status(root: &Path) -> GitSnapshot {
    let output = git_command()
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-b"])
        .output();
    let output = match output {
        Ok(output) => output,
        Err(_) => return GitSnapshot::empty(t("git is not installed", "未安装 git")),
    };
    if !output.status.success() {
        return GitSnapshot::empty(t("Not a git repository", "不是 git 仓库"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut branch = String::new();
    let mut files = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = rest.split("...").next().unwrap_or(rest).trim().to_owned();
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let status = line[..2].trim().to_owned();
        let path = line[3..].trim().to_owned();
        if path.is_empty() {
            continue;
        }
        files.push(GitFile { path, status });
        if files.len() == 80 {
            break;
        }
    }
    GitSnapshot {
        branch,
        files,
        note: None,
    }
}

/// Unified diff for one path.
///
/// `git diff HEAD` includes staged edits. An untracked file has no HEAD
/// object, so the whole file is shown as added lines.
pub(crate) fn read_diff(root: &Path, path: &str) -> String {
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["diff", "HEAD", "--", path])
        .output();
    let Ok(output) = output else {
        return t("git is not installed", "未安装 git").to_owned();
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !text.is_empty() {
        return cap_diff(&text);
    }
    if untracked(root, path) {
        return added_file_diff(root, path);
    }
    t("No diff against HEAD.", "与 HEAD 无差异。").to_owned()
}

fn cap_diff(text: &str) -> String {
    if text.len() > 12_000 {
        format!("{}…", &text[..12_000])
    } else {
        text.to_owned()
    }
}

fn untracked(root: &Path, path: &str) -> bool {
    let Ok(output) = git_command()
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "--", path])
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .any(|line| line.starts_with("?? ") && line[3..].trim() == path)
}

/// The whole file as a new-file unified diff. Binary files stay a one-line note.
fn added_file_diff(root: &Path, path: &str) -> String {
    if path.is_empty() || path.contains('\0') || Path::new(path).is_absolute() {
        return t("No diff against HEAD.", "与 HEAD 无差异。").to_owned();
    }
    let full = root.join(path);
    let Ok(bytes) = std::fs::read(&full) else {
        return t("No diff against HEAD.", "与 HEAD 无差异。").to_owned();
    };
    if bytes.contains(&0) {
        return t("Binary file (not shown).", "二进制文件（未显示）。").to_owned();
    }
    let body = String::from_utf8_lossy(&bytes);
    cap_diff(&added_lines(path, &body))
}

/// Unified diff that adds every line of `body`.
fn added_lines(path: &str, body: &str) -> String {
    let mut lines: Vec<&str> = body.split('\n').collect();
    if body.ends_with('\n') {
        lines.pop();
    }
    if lines.len() == 1 && lines[0].is_empty() && body.is_empty() {
        lines.clear();
    }
    let count = lines.len();
    let mut out = String::new();
    out.push_str(&format!("diff --git a/{path} b/{path}\n"));
    out.push_str("new file mode 100644\n");
    out.push_str("--- /dev/null\n");
    out.push_str(&format!("+++ b/{path}\n"));
    if count == 0 {
        out.push_str("@@ -0,0 +0,0 @@\n");
        return out;
    }
    out.push_str(&format!("@@ -0,0 +1,{count} @@\n"));
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod diff_tests {
    use super::added_lines;

    #[test]
    fn an_untracked_file_is_shown_as_added_lines() {
        let diff = added_lines("f.txt", "alpha\nbeta\n");
        assert!(diff.contains("+++ b/f.txt"), "{diff}");
        assert!(diff.contains("+alpha\n"), "{diff}");
        assert!(diff.contains("+beta\n"), "{diff}");
        assert!(!diff.contains("No diff"), "{diff}");
    }

    #[test]
    fn a_crlf_new_file_does_not_keep_the_carriage_return_in_the_row() {
        let diff = added_lines("f.txt", "one\r\ntwo\r\n");
        assert!(diff.contains("+one\n"), "{diff}");
        assert!(!diff.contains("+one\r"), "{diff}");
    }
}
