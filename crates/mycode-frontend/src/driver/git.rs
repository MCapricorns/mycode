//! The open folder's working-tree status, kept fresh by filesystem events.
//!
//! macOS reports changes through FSEvents and Windows through
//! `ReadDirectoryChangesW`. A burst of changes collapses into one
//! `git status` read on a worker thread, and results land on the driver
//! inbox. Only a frontend that shows changes turns this on
//! ([`FrontendDriver::watch_git`]).

use std::path::Path;
use std::time::Duration;

use notify::Watcher as _;

use super::{DriverMsg, FrontendDriver};
use crate::git_status::{self, GitSnapshot};
use crate::i18n::t;

/// Quiet period after the last filesystem event before `git status` runs.
const GIT_REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);

/// Git state for the open folder.
pub(super) struct GitWatch {
    /// A frontend asked for it ([`FrontendDriver::watch_git`]).
    enabled: bool,
    /// Latest git status for the open folder.
    snapshot: GitSnapshot,
    /// Platform watcher for the open folder. Dropping it stops refresh.
    watcher: Option<notify::RecommendedWatcher>,
    /// Bumped when the watched folder changes so a stale read is dropped.
    generation: u64,
    /// A `git status` process is running.
    status_inflight: bool,
    /// The tree changed again while a status read was in flight.
    status_pending: bool,
    /// Path whose diff is shown.
    diff_path: Option<String>,
    diff: String,
    /// Bumped when the diff target changes so a stale diff is dropped.
    diff_generation: u64,
    /// The diff is on screen, so a watched read refreshes it.
    diff_open: bool,
}

impl Default for GitWatch {
    fn default() -> Self {
        Self {
            enabled: false,
            snapshot: GitSnapshot::empty(t("No folder", "未打开目录")),
            watcher: None,
            generation: 0,
            status_inflight: false,
            status_pending: false,
            diff_path: None,
            diff: String::new(),
            diff_generation: 0,
            diff_open: false,
        }
    }
}

impl FrontendDriver {
    /// Starts keeping the open folder's git status, and follows the folder
    /// from now on. A frontend without a changes view never calls it, so it
    /// watches nothing.
    pub fn watch_git(&mut self) {
        self.git.enabled = true;
        self.rewatch_git();
    }

    /// Latest git status for the open folder.
    #[must_use]
    pub fn git(&self) -> &GitSnapshot {
        &self.git.snapshot
    }

    /// The path whose diff was opened last, open or not.
    #[must_use]
    pub fn git_diff_path(&self) -> Option<&str> {
        self.git.diff_path.as_deref()
    }

    /// The opened path's diff, or a loading note.
    #[must_use]
    pub fn git_diff(&self) -> &str {
        &self.git.diff
    }

    /// Whether the opened diff is on screen.
    #[must_use]
    pub fn git_diff_open(&self) -> bool {
        self.git.diff_open
    }

    /// Opens one dirty path's diff and reads it. Opening the path already
    /// shown reads it again without blanking it first.
    pub fn select_git_file(&mut self, path: &str) {
        self.git.diff_open = true;
        let Some(root) = self.vm.project_dir.clone() else {
            return;
        };
        if self.git.diff_path.as_deref() != Some(path) {
            self.git.diff = t("Loading diff…", "正在加载差异…").to_owned();
        }
        self.git.diff_path = Some(path.to_owned());
        self.git.diff_generation = self.git.diff_generation.wrapping_add(1);
        let generation = self.git.diff_generation;
        let path = path.to_owned();
        self.in_background(move || {
            let diff = git_status::read_diff(Path::new(&root), &path);
            DriverMsg::GitDiffRead {
                generation,
                path,
                diff,
            }
        });
    }

    /// Closes the diff. The path stays selected in the changes list.
    pub fn close_git_diff(&mut self) {
        self.git.diff_open = false;
    }

    /// Watches `project_dir` and reads its status, when a frontend asked for
    /// it. A folder change drops any in-flight read for the previous folder.
    pub(super) fn rewatch_git(&mut self) {
        if !self.git.enabled {
            return;
        }
        let git = &mut self.git;
        git.generation = git.generation.wrapping_add(1);
        git.watcher = None;
        git.diff_generation = git.diff_generation.wrapping_add(1);
        git.diff_path = None;
        git.diff.clear();
        git.diff_open = false;
        let generation = git.generation;
        let Some(root) = self.vm.project_dir.clone() else {
            self.git.snapshot = GitSnapshot::empty(t("No folder", "未打开目录"));
            return;
        };
        self.git.snapshot = GitSnapshot::empty(t("Loading…", "正在加载…"));
        if let Some((watcher, events)) = watch_root(&root) {
            self.git.watcher = Some(watcher);
            let tx = self.inbox_tx.clone();
            let wake = self.wake.clone();
            // Ends with the watcher: its callback held the only sender.
            std::thread::spawn(move || {
                while events.recv().is_ok() {
                    std::thread::sleep(GIT_REFRESH_DEBOUNCE);
                    while events.try_recv().is_ok() {}
                    if tx.send(DriverMsg::GitTreeChanged { generation }).is_err() {
                        return;
                    }
                    wake();
                }
            });
        }
        self.read_git_status();
    }

    /// The watched tree settled after a change.
    pub(super) fn on_git_tree_changed(&mut self, generation: u64) {
        if generation == self.git.generation {
            self.read_git_status();
        }
    }

    fn read_git_status(&mut self) {
        if self.git.status_inflight {
            self.git.status_pending = true;
            return;
        }
        let Some(root) = self.vm.project_dir.clone() else {
            self.git.snapshot = GitSnapshot::empty(t("No folder", "未打开目录"));
            return;
        };
        self.git.status_inflight = true;
        let generation = self.git.generation;
        self.in_background(move || {
            let snapshot = git_status::read_status(Path::new(&root));
            DriverMsg::GitStatusRead {
                generation,
                root,
                snapshot: Box::new(snapshot),
            }
        });
    }

    pub(super) fn finish_git_status(&mut self, generation: u64, root: &str, snapshot: GitSnapshot) {
        self.git.status_inflight = false;
        let pending = std::mem::take(&mut self.git.status_pending);
        let current =
            self.git.generation == generation && self.vm.project_dir.as_deref() == Some(root);
        if current {
            self.git.snapshot = snapshot;
            // Contents can change without changing porcelain status (`M`
            // stays `M`), so refresh an open diff after every watched read.
            if self.git.diff_open
                && let Some(path) = self.git.diff_path.clone()
            {
                self.select_git_file(&path);
            }
        }
        if pending && self.vm.project_dir.is_some() {
            self.read_git_status();
        }
    }

    pub(super) fn finish_git_diff(&mut self, generation: u64, path: &str, diff: String) {
        if self.git.diff_generation == generation && self.git.diff_path.as_deref() == Some(path) {
            self.git.diff = diff;
        }
    }
}

fn watch_root(root: &str) -> Option<(notify::RecommendedWatcher, std::sync::mpsc::Receiver<()>)> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if watch_signal(&result) {
            let _ = tx.try_send(());
        }
    })
    .ok()?;
    watcher
        .watch(Path::new(root), notify::RecursiveMode::Recursive)
        .ok()?;
    Some((watcher, rx))
}

fn watch_signal(result: &notify::Result<notify::Event>) -> bool {
    match result {
        Ok(event) => git_watch_relevant(event),
        // An overflow or backend error means some changes were not delivered.
        Err(_) => true,
    }
}

fn git_watch_relevant(event: &notify::Event) -> bool {
    if !matches!(
        event.kind,
        notify::EventKind::Any
            | notify::EventKind::Create(_)
            | notify::EventKind::Modify(_)
            | notify::EventKind::Remove(_)
    ) {
        return false;
    }
    event.paths.is_empty() || event.paths.iter().any(|path| path_affects_status(path))
}

/// Worktree edits matter. Object-store and lock churn inside `.git` does not.
fn path_affects_status(path: &Path) -> bool {
    let mut components = path.components();
    let Some(git_at) = components.position(|component| component.as_os_str() == ".git") else {
        return true;
    };
    let Some(next) = path.components().nth(git_at + 1) else {
        return true;
    };
    let name = next.as_os_str();
    if name.to_str().is_some_and(|name| name.ends_with(".lock")) {
        return false;
    }
    !matches!(
        name.to_str(),
        Some("objects" | "logs" | "hooks" | "info" | "worktrees")
    )
}

#[cfg(test)]
mod tests {
    use super::path_affects_status;
    use std::path::Path;

    #[test]
    fn worktree_edits_refresh_status() {
        assert!(path_affects_status(Path::new("/repo/src/main.rs")));
        assert!(path_affects_status(Path::new("/repo/.git/HEAD")));
        assert!(path_affects_status(Path::new("/repo/.git/index")));
        assert!(path_affects_status(Path::new("/repo/.git")));
    }

    #[test]
    fn git_internals_do_not_refresh_status() {
        assert!(!path_affects_status(Path::new("/repo/.git/objects/ab/cd")));
        assert!(!path_affects_status(Path::new("/repo/.git/logs/HEAD")));
        assert!(!path_affects_status(Path::new(
            "/repo/.git/worktrees/agent/HEAD"
        )));
        assert!(!path_affects_status(Path::new("/repo/.git/index.lock")));
        assert!(!path_affects_status(Path::new(
            "/repo/.git/hooks/pre-commit"
        )));
    }
}
