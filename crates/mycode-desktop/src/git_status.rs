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
    let output = git_command()
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-b", "--untracked-files=all"])
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

/// Parses `git status --porcelain=v1 -b` text.
///
/// Rename and copy rows (`R` or `C` in the two-character status) use
/// `old -> new`. The new path is [`GitFile::path`] so diffs open the
/// destination. Exactly [`STATUS_CAP`] files with no further file row is
/// complete; another file row sets [`GitSnapshot::stopped_early`].
fn parse_porcelain(text: &str) -> GitSnapshot {
    let mut branch = String::new();
    let mut files = Vec::new();
    let mut stopped_early = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = rest.split("...").next().unwrap_or(rest).trim().to_owned();
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        if files.len() == STATUS_CAP {
            stopped_early = true;
            break;
        }
        let status_xy = &line[..2];
        let status = status_xy.trim().to_owned();
        let (path, previous) = split_rename(status_xy, line[3..].trim());
        if path.is_empty() {
            continue;
        }
        files.push(GitFile {
            path,
            status,
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

fn split_rename(status_xy: &str, payload: &str) -> (String, Option<String>) {
    let renamed = status_xy.chars().any(|ch| ch == 'R' || ch == 'C');
    if renamed && let Some((old, new)) = split_arrow(payload) {
        let path = unquote_path(new);
        if !path.is_empty() {
            return (path, Some(unquote_path(old)));
        }
    }
    (unquote_path(payload), None)
}

fn split_arrow(payload: &str) -> Option<(&str, &str)> {
    let bytes = payload.as_bytes();
    let mut quoted = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' && (index == 0 || bytes[index - 1] != b'\\') {
            quoted = !quoted;
            index += 1;
            continue;
        }
        if !quoted && bytes[index..].starts_with(b" -> ") {
            return Some((payload[..index].trim(), payload[index + 4..].trim()));
        }
        index += 1;
    }
    None
}

fn unquote_path(raw: &str) -> String {
    let raw = raw.trim();
    let Some(inner) = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return raw.to_owned();
    };
    let mut bytes = Vec::new();
    let source = inner.as_bytes();
    let mut index = 0;
    while index < source.len() {
        if source[index] == b'\\' && index + 1 < source.len() {
            match source[index + 1] {
                b'n' => {
                    bytes.push(b'\n');
                    index += 2;
                }
                b't' => {
                    bytes.push(b'\t');
                    index += 2;
                }
                b'\\' | b'"' => {
                    bytes.push(source[index + 1]);
                    index += 2;
                }
                b'0'..=b'7' => {
                    let mut value = 0u16;
                    let mut consumed = 0;
                    while consumed < 3 && index + 1 + consumed < source.len() {
                        let digit = source[index + 1 + consumed];
                        if !digit.is_ascii_digit() || digit > b'7' {
                            break;
                        }
                        value = value * 8 + u16::from(digit - b'0');
                        consumed += 1;
                    }
                    if consumed == 0 {
                        bytes.push(b'\\');
                        index += 1;
                    } else {
                        bytes.push(value as u8);
                        index += 1 + consumed;
                    }
                }
                _ => {
                    bytes.push(source[index + 1]);
                    index += 2;
                }
            }
        } else {
            bytes.push(source[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
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

    #[test]
    fn an_untracked_directory_lists_each_file() {
        let root = std::env::temp_dir().join(format!(
            "mycode-untracked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
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
        let snapshot = super::parse_porcelain("## main\nR  old.txt -> new.txt\n");
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "new.txt");
        assert_eq!(snapshot.files[0].previous.as_deref(), Some("old.txt"));
        assert_eq!(snapshot.files[0].status, "R");
        assert!(!snapshot.stopped_early);

        let quoted = super::parse_porcelain("C  \"old file.txt\" -> \"new file.txt\"\n");
        assert_eq!(quoted.files[0].path, "new file.txt");
        assert_eq!(quoted.files[0].previous.as_deref(), Some("old file.txt"));

        let plain = super::parse_porcelain(" M keep -> name.txt\n");
        assert_eq!(plain.files[0].path, "keep -> name.txt");
        assert!(plain.files[0].previous.is_none());
    }

    #[test]
    fn porcelain_marks_the_list_incomplete_only_past_the_cap() {
        let mut eighty = String::from("## main\n");
        for index in 0..80 {
            eighty.push_str(&format!(" M file{index}.txt\n"));
        }
        let complete = super::parse_porcelain(&eighty);
        assert_eq!(complete.files.len(), 80);
        assert!(!complete.stopped_early);

        eighty.push_str(" M extra.txt\n");
        let truncated = super::parse_porcelain(&eighty);
        assert_eq!(truncated.files.len(), 80);
        assert!(truncated.stopped_early);
        assert_eq!(truncated.files[0].path, "file0.txt");
    }
}
