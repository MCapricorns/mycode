//! Working-tree refresh driven by filesystem events.
//!
//! macOS reports changes through FSEvents and Windows through
//! `ReadDirectoryChangesW`. A change wakes the workspace, which collapses
//! the burst and reads `git status` on a worker thread.

use std::path::Path;
use std::time::Duration;

use gpui_kit::Context;
use notify::Watcher as _;

use crate::git_status::{self, GitSnapshot};
use crate::i18n::t;
use crate::workspace::Workspace;

/// Quiet period after the last filesystem event before `git status` runs.
const GIT_REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);

pub(super) type Watcher = notify::RecommendedWatcher;

impl Workspace {
    /// Watches `project_dir` and reads its status. A folder change drops any
    /// in-flight read for the previous folder.
    pub(super) fn rewatch_git(&mut self, cx: &mut Context<Self>) {
        self.git_generation = self.git_generation.wrapping_add(1);
        let generation = self.git_generation;
        self.git_watcher = None;
        self.git_diff_generation = self.git_diff_generation.wrapping_add(1);
        self.git_diff_path = None;
        self.git_diff.clear();
        self.git_diff_panel_open = false;

        let Some(root) = self.vm.project_dir.clone() else {
            self.git = GitSnapshot::empty(t("No folder", "未打开目录"));
            return;
        };
        self.git = GitSnapshot::empty(t("Loading…", "正在加载…"));
        if let Some((watcher, events)) = watch_root(&root) {
            self.git_watcher = Some(watcher);
            self.spawn_git_watch(generation, events, cx);
        }
        self.spawn_git_status(cx);
    }

    /// Opens one dirty path in the dedicated diff panel.
    ///
    /// The changes list (inspector preview or the review-all drawer) only
    /// names files. The diff itself is a separate panel so the patch is
    /// readable. Clicking a file always opens that panel, including when
    /// the full changes drawer is already open.
    pub(crate) fn on_select_git_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.git_diff_panel_open = true;
        if self.git_diff_path.as_deref() == Some(path) {
            cx.notify();
            return;
        }
        let Some(root) = self.vm.project_dir.clone() else {
            return;
        };
        let path = path.to_owned();
        self.git_diff_path = Some(path.clone());
        self.git_diff = t("Loading diff…", "正在加载差异…").to_owned();
        self.git_diff_generation = self.git_diff_generation.wrapping_add(1);
        let generation = self.git_diff_generation;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let diff = git_status::read_diff(Path::new(&root), &path);
            let _ = tx.try_send((path, diff));
        });
        cx.spawn(async move |this, cx| {
            let Ok((path, diff)) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                if workspace.git_diff_generation != generation {
                    return;
                }
                if workspace.git_diff_path.as_deref() == Some(path.as_str()) {
                    workspace.git_diff = diff;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn spawn_git_watch(
        &self,
        generation: u64,
        events: async_channel::Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                if events.recv().await.is_err() {
                    return;
                }
                cx.background_executor().timer(GIT_REFRESH_DEBOUNCE).await;
                while events.try_recv().is_ok() {}
                let keep = this.update(cx, |workspace, cx| {
                    if workspace.git_generation != generation {
                        return false;
                    }
                    workspace.spawn_git_status(cx);
                    true
                });
                if !matches!(keep, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    fn spawn_git_status(&mut self, cx: &mut Context<Self>) {
        if self.git_status_inflight {
            self.git_status_pending = true;
            return;
        }
        let Some(root) = self.vm.project_dir.clone() else {
            self.git = GitSnapshot::empty(t("No folder", "未打开目录"));
            return;
        };
        self.git_status_inflight = true;
        let generation = self.git_generation;
        let (tx, rx) = async_channel::bounded(1);
        let read_root = root.clone();
        std::thread::spawn(move || {
            let snapshot = git_status::read_status(Path::new(&read_root));
            let _ = tx.try_send(snapshot);
        });
        cx.spawn(async move |this, cx| {
            let Ok(snapshot) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.finish_git_status(generation, &root, snapshot, cx);
            });
        })
        .detach();
    }

    fn finish_git_status(
        &mut self,
        generation: u64,
        root: &str,
        snapshot: GitSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.git_status_inflight = false;
        let pending = self.git_status_pending;
        self.git_status_pending = false;
        let current =
            self.git_generation == generation && self.vm.project_dir.as_deref() == Some(root);
        if current && self.git != snapshot {
            self.git = snapshot;
            cx.notify();
        }
        if pending && self.vm.project_dir.is_some() {
            self.spawn_git_status(cx);
        }
    }
}

fn watch_root(root: &str) -> Option<(Watcher, async_channel::Receiver<()>)> {
    let (tx, rx) = async_channel::bounded(1);
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
