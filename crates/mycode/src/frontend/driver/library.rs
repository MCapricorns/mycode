//! Keep Library-backed menus current while magpie edits their source files.
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::Watcher as _;

use super::{DriverMsg, FrontendDriver};

impl FrontendDriver {
    pub(super) fn watch_library(&mut self) {
        // FSEvents reports canonical paths (for example /private/var instead
        // of /var). Watch and filter with the same spelling on every backend,
        // including when attaching new skills/agents directories below.
        let root = watcher_root(self.home.root());
        let watched_root = root.clone();
        let (tx, events) = std::sync::mpsc::sync_channel(1);
        let Ok(mut watcher) =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let relevant = match event {
                    Ok(event) => {
                        !matches!(event.kind, notify::EventKind::Access(_))
                            && (event.paths.is_empty()
                                || event
                                    .paths
                                    .iter()
                                    .any(|path| library_path(&watched_root, path)))
                    }
                    Err(_) => true,
                };
                if relevant {
                    let _ = tx.try_send(());
                }
            })
        else {
            return;
        };
        // Do not recursively watch sessions, downloads, and tool output. The
        // home watch notices newly created/replaced Library directories; each
        // refresh reattaches their recursive watches.
        if watcher
            .watch(&root, notify::RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }
        for name in ["skills", "agents"] {
            let path = root.join(name);
            if path.is_dir() {
                let _ = watcher.watch(&path, notify::RecursiveMode::Recursive);
            }
        }
        self.library_watcher = Some(watcher);
        let tx = self.inbox_tx.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            while events.recv().is_ok() {
                std::thread::sleep(Duration::from_millis(150));
                while events.try_recv().is_ok() {}
                if tx.send(DriverMsg::LibraryChanged).is_err() {
                    return;
                }
                wake();
            }
        });
    }
}

fn watcher_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn library_path(root: &Path, path: &Path) -> bool {
    // Some backends coalesce child changes into a directory notification.
    if path == root {
        return true;
    }
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|first| {
            matches!(
                first.as_os_str().to_str(),
                Some("settings.json" | "mcp.json" | "skills" | "agents" | "AGENTS.md")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::library_path;
    use std::path::Path;

    #[test]
    fn only_library_files_trigger_refreshes() {
        let root = Path::new("home");
        assert!(library_path(root, root));
        for path in [
            "mcp.json",
            "settings.json",
            "skills/new/SKILL.md",
            "agents/scout.md",
            "AGENTS.md",
        ] {
            assert!(library_path(root, &root.join(path)), "{path}");
        }
        for path in [
            "ui.json",
            "sessions/chat.jsonl",
            "update/payload",
            "settings.json.tmp",
        ] {
            assert!(!library_path(root, &root.join(path)), "{path}");
        }
        assert!(!library_path(root, Path::new("outside/mcp.json")));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_watch_paths_match_events_even_after_the_changed_file_is_removed() {
        let root = std::env::temp_dir().join(format!(
            "mycode-library-root-{}-{}",
            std::process::id(),
            crate::core::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        let watched = super::watcher_root(&root.join("alias"));
        let canonical = root.join("real").canonicalize().unwrap();
        assert_eq!(watched, canonical);
        let file = canonical.join("mcp.json");
        std::fs::write(&file, "{}").unwrap();
        assert!(library_path(&watched, &file));
        std::fs::remove_file(&file).unwrap();
        // Normalize the watch root once, not each event: removals no longer
        // exist and cannot be canonicalized when the callback receives them.
        assert!(library_path(&watched, &file));
        assert!(library_path(
            &watched,
            &canonical.join("skills/new/SKILL.md")
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
