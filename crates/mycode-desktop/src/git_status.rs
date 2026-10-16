//! Working-tree snapshot for the changes panel.
//!
//! Reads `git status` and `git diff` in the session folder. A missing git
//! install or a non-repository is a panel note, not a chat error.

use crate::i18n::t;
use std::fmt::Write as _;

#[cfg(test)]
mod git_integration_tests;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `git` with no console window. The desktop process has no console, so a
/// plain spawn of `git.exe` allocates a black window on Windows.
fn git_command() -> Command {
    let mut command = Command::new("git");
    // Every panel operation is read-only and treats filenames as literals.
    command.args(["--no-optional-locks", "--no-pager", "--literal-pathspecs"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

enum RepoRoot {
    Root(PathBuf),
    NotARepository,
    GitUnavailable,
}

fn repository_root(folder: &Path) -> RepoRoot {
    let Ok(output) = git_command()
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    else {
        return RepoRoot::GitUnavailable;
    };
    if !output.status.success() {
        return RepoRoot::NotARepository;
    }
    match std::str::from_utf8(&output.stdout)
        .ok()
        .map(|root| root.trim_end_matches(['\n', '\r']))
        .filter(|root| !root.is_empty())
    {
        Some(root) => RepoRoot::Root(PathBuf::from(root)),
        None => RepoRoot::NotARepository,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitFile {
    pub path: String,
    pub status: String,
    /// Previous path for a rename or copy. Diffs use [`Self::path`].
    pub previous: Option<String>,
}

/// Branch plus dirty files for the open folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitSnapshot {
    pub branch: String,
    pub files: Vec<GitFile>,
    pub note: Option<String>,
    /// True when another dirty path existed past the file cap.
    pub stopped_early: bool,
}

impl GitSnapshot {
    pub(crate) fn empty(note: impl Into<String>) -> Self {
        Self {
            branch: String::new(),
            files: Vec::new(),
            note: Some(note.into()),
            stopped_early: false,
        }
    }
}

/// `git status --porcelain=v1 -b --untracked-files=all` for one directory.
///
/// `--no-optional-locks` keeps a refresh from taking `.git/index.lock` and
/// opportunistically rewriting the index, which would contend with the user's
/// own git commands and subagent worktree updates. The file list keeps 80
/// dirty paths and sets [`GitSnapshot::stopped_early`] when another remains.
pub(crate) fn read_status(root: &Path) -> GitSnapshot {
    let root = match repository_root(root) {
        RepoRoot::Root(root) => root,
        RepoRoot::NotARepository => {
            return GitSnapshot::empty(t("Not a git repository", "不是 git 仓库"));
        }
        RepoRoot::GitUnavailable => {
            return GitSnapshot::empty(t("git is not installed", "未安装 git"));
        }
    };
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "-b",
            "--untracked-files=all",
        ])
        .output();
    let output = match output {
        Ok(output) => output,
        Err(_) => return GitSnapshot::empty(t("git is not installed", "未安装 git")),
    };
    if !output.status.success() {
        return GitSnapshot::empty(t("Not a git repository", "不是 git 仓库"));
    }
    parse_porcelain(&String::from_utf8_lossy(&output.stdout))
}

const STATUS_CAP: usize = 80;

/// Parses NUL-delimited porcelain. Paths are never quoted or whitespace-trimmed;
/// rename/copy records carry the destination first and then the old name.
fn parse_porcelain(text: &str) -> GitSnapshot {
    let mut branch = String::new();
    let mut files = Vec::new();
    let mut stopped_early = false;
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        if let Some(rest) = record.strip_prefix("## ") {
            branch = rest.split("...").next().unwrap_or(rest).trim().to_owned();
            continue;
        }
        let (Some(status_xy), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        if path.is_empty() || record.as_bytes().get(2) != Some(&b' ') {
            continue;
        }
        let previous = status_xy
            .contains(['R', 'C'])
            .then(|| records.next().unwrap_or_default().to_owned());
        if files.len() == STATUS_CAP {
            stopped_early = true;
            break;
        }
        files.push(GitFile {
            path: path.to_owned(),
            status: status_xy.trim().to_owned(),
            previous,
        });
    }
    GitSnapshot {
        branch,
        files,
        note: None,
        stopped_early,
    }
}

/// Unified diff for one path.
///
/// `git diff HEAD` includes staged edits. An untracked file has no HEAD
/// object, so the whole file is shown as added lines.
pub(crate) fn read_diff(root: &Path, path: &str) -> String {
    let root = match repository_root(root) {
        RepoRoot::Root(root) => root,
        RepoRoot::NotARepository => {
            return t("Not a git repository", "不是 git 仓库").to_owned();
        }
        RepoRoot::GitUnavailable => {
            return t("git is not installed", "未安装 git").to_owned();
        }
    };
    let output = git_command()
        .arg("-C")
        .arg(&root)
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
            path,
        ])
        .output();
    let Ok(output) = output else {
        return t("git is not installed", "未安装 git").to_owned();
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !text.is_empty() {
        return cap_diff(&text);
    }
    if is_new_file(&root, path, !output.status.success()) {
        return added_file_diff(&root, path);
    }
    if !output.status.success() {
        return cap_diff(&String::from_utf8_lossy(&output.stderr));
    }
    t("No diff against HEAD.", "与 HEAD 无差异。").to_owned()
}

/// Byte cap for one diff-panel read.
const DIFF_CAP_BYTES: usize = 12_000;

/// Truncates past [`DIFF_CAP_BYTES`]. Diff text is UTF-8 file content, so the
/// cut backs up to the nearest character boundary instead of slicing through
/// a multi-byte sequence.
fn cap_diff(text: &str) -> String {
    if text.len() <= DIFF_CAP_BYTES {
        return text.to_owned();
    }
    format!("{}…", &text[..text.floor_char_boundary(DIFF_CAP_BYTES)])
}

fn is_new_file(root: &Path, path: &str, allow_staged: bool) -> bool {
    let Ok(output) = git_command()
        .arg("-C")
        .arg(root)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            path,
        ])
        .output()
    else {
        return false;
    };
    output.status.success()
        && parse_porcelain(&String::from_utf8_lossy(&output.stdout))
            .files
            .iter()
            .any(|file| {
                file.path == path
                    && (file.status == "??" || (allow_staged && file.status.contains('A')))
            })
}

/// The whole file as a new-file unified diff. Binary files stay a one-line note.
fn added_file_diff(root: &Path, path: &str) -> String {
    if path.is_empty()
        || path.contains('\0')
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return t("No diff against HEAD.", "与 HEAD 无差异。").to_owned();
    }
    let full = root.join(path);
    if std::fs::symlink_metadata(&full).is_ok_and(|metadata| metadata.is_symlink()) {
        return std::fs::read_link(&full)
            .map(|target| cap_diff(&added_lines(path, &target.to_string_lossy())))
            .unwrap_or_else(|_| t("No diff against HEAD.", "与 HEAD 无差异。").to_owned());
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&full).and_then(|file| {
        file.take((DIFF_CAP_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
    });
    if read.is_err() {
        return t("No diff against HEAD.", "与 HEAD 无差异。").to_owned();
    };
    if bytes.contains(&0) {
        return t("Binary file (not shown).", "二进制文件（未显示）。").to_owned();
    }
    let body = String::from_utf8_lossy(&bytes);
    cap_diff(&added_lines(path, &body))
}

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
    let _ = writeln!(out, "diff --git a/{path} b/{path}");
    out.push_str("new file mode 100644\n");
    out.push_str("--- /dev/null\n");
    let _ = writeln!(out, "+++ b/{path}");
    if count == 0 {
        out.push_str("@@ -0,0 +0,0 @@\n");
        return out;
    }
    let _ = writeln!(out, "@@ -0,0 +1,{count} @@");
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
    use std::fmt::Write as _;

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

    #[test]
    fn an_untracked_directory_lists_each_file() {
        let root = std::env::temp_dir().join(format!(
            "mycode-untracked-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/note.txt"), "hello\n").unwrap();
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git");
        assert!(init.success(), "git init");
        let snapshot = super::read_status(&root);
        assert!(
            snapshot
                .files
                .iter()
                .any(|file| file.path == "nested/note.txt"),
            "expected the file inside the directory, got {:?}",
            snapshot.files
        );
        assert!(
            snapshot.files.iter().all(|file| file.path != "nested/"),
            "{:?}",
            snapshot.files
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn porcelain_rename_splits_old_and_new_paths() {
        let snapshot = super::parse_porcelain("## main\0R  new.txt\0old.txt\0");
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "new.txt");
        assert_eq!(snapshot.files[0].previous.as_deref(), Some("old.txt"));
        assert_eq!(snapshot.files[0].status, "R");
        assert!(!snapshot.stopped_early);

        let quoted = super::parse_porcelain("C  new file.txt\0old file.txt\0");
        assert_eq!(quoted.files[0].path, "new file.txt");
        assert_eq!(quoted.files[0].previous.as_deref(), Some("old file.txt"));

        let plain = super::parse_porcelain(" M keep -> name.txt\0");
        assert_eq!(plain.files[0].path, "keep -> name.txt");
        assert!(plain.files[0].previous.is_none());
    }

    #[test]
    fn a_cap_inside_a_multibyte_character_truncates_at_the_boundary() {
        let mut text = "a".repeat(super::DIFF_CAP_BYTES - 1);
        text.push_str("中文内容");
        let capped = super::cap_diff(&text);
        assert_eq!(capped.len(), super::DIFF_CAP_BYTES - 1 + "…".len());
        assert!(capped.starts_with(&"a".repeat(64)));
        assert!(capped.ends_with('…'));
        let short = super::cap_diff("tiny diff");
        assert_eq!(short, "tiny diff");
    }

    #[test]
    fn porcelain_marks_the_list_incomplete_only_past_the_cap() {
        let mut eighty = String::from("## main\0");
        for index in 0..80 {
            let _ = write!(eighty, " M file{index}.txt\0");
        }
        let complete = super::parse_porcelain(&eighty);
        assert_eq!(complete.files.len(), 80);
        assert!(!complete.stopped_early);

        eighty.push_str(" M extra.txt\0");
        let truncated = super::parse_porcelain(&eighty);
        assert_eq!(truncated.files.len(), 80);
        assert!(truncated.stopped_early);
        assert_eq!(truncated.files[0].path, "file0.txt");
    }
}
