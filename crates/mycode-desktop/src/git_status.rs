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

/// Unified diff for one path. Untracked files have no HEAD diff.
pub(crate) fn read_diff(root: &Path, path: &str) -> String {
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["diff", "--", path])
        .output();
    let Ok(output) = output else {
        return t("git is not installed", "未安装 git").to_owned();
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        t("No diff against HEAD.", "与 HEAD 无差异。").to_owned()
    } else if text.len() > 12_000 {
        format!("{}…", &text[..12_000])
    } else {
        text
    }
}
