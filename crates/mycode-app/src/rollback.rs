//! Applies a session checkpoint plan through the no-follow file kernel.
//!
//! Planning stays in `mycode-config`. This module is the only writer: restore
//! uses the file-tool publish path, and a file created after the checkpoint
//! is deleted rather than snapshotted.

use std::path::Path;

use mycode_config::{HomeLayout, RollbackAction, plan_rollback};
use mycode_tools::{remove_file_nofollow, restore_file_nofollow};

/// Restores or deletes every path in the session checkpoint.
///
/// Returns the paths touched, in plan order. The first failure stops the
/// rollback and is returned to the caller.
///
/// # Errors
///
/// Returns a visible message when planning fails, a path cannot be split, or
/// a no-follow restore or delete fails. Nothing is skipped quietly.
pub(crate) fn apply_rollback(home: &HomeLayout, session_id: &str) -> Result<Vec<String>, String> {
    let actions = plan_rollback(home, session_id).map_err(|error| error.summary())?;
    let mut touched = Vec::new();
    for action in actions {
        match action {
            RollbackAction::Restore { path, bytes } => {
                let (parent, name) = split_file(&path)?;
                restore_file_nofollow(parent, name, &bytes)
                    .map_err(|error| format!("rollback restore failed for {path}: {error}"))?;
                touched.push(path);
            }
            RollbackAction::Delete { path } => {
                let (parent, name) = split_file(&path)?;
                remove_file_nofollow(parent, name)
                    .map_err(|error| format!("rollback delete failed for {path}: {error}"))?;
                touched.push(path);
            }
        }
    }
    Ok(touched)
}

fn split_file(path: &str) -> Result<(&Path, &str), String> {
    let path_buf = Path::new(path);
    let parent = path_buf
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("rollback path has no parent: {path}"))?;
    let name = path_buf
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("rollback path has no Unicode file name: {path}"))?;
    Ok((parent, name))
}

#[cfg(test)]
mod tests {
    use super::apply_rollback;
    use mycode_config::{ConfigErrorKind, HomeLayout, checkpoint_file};

    fn scratch(label: &str) -> (std::path::PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-rollback-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let home = HomeLayout::from_root(&root).unwrap();
        (root, home)
    }

    #[test]
    fn rollback_deletes_a_file_created_after_the_checkpoint() {
        let (root, home) = scratch("delete");
        let file = root.join("created.txt");
        checkpoint_file(&home, "session", &file).unwrap();
        std::fs::write(&file, b"new contents").unwrap();
        let touched = apply_rollback(&home, "session").unwrap();
        assert_eq!(touched, vec![file.display().to_string()]);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rollback_restores_prior_bytes_without_stdio_write() {
        let (root, home) = scratch("restore");
        let file = root.join("tracked.txt");
        std::fs::write(&file, b"before").unwrap();
        checkpoint_file(&home, "session", &file).unwrap();
        std::fs::write(&file, b"after").unwrap();
        apply_rollback(&home, "session").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"before");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn checkpoint_hook_surfaces_oversized_files() {
        let (root, home) = scratch("hook");
        let file = root.join("huge.bin");
        std::fs::write(&file, vec![7u8; 8 * 1024 * 1024 + 1]).unwrap();
        let hook = crate::turn::checkpoint_hook(home, root.clone(), "session".to_owned());
        let args = serde_json::json!({ "path": "huge.bin" });
        let error = hook("write", &args).await.unwrap_err();
        assert!(
            error.starts_with("checkpoint failed:"),
            "oversized checkpoint must fail the tool visibly, got {error}"
        );
        let direct = checkpoint_file(&HomeLayout::from_root(&root).unwrap(), "other", &file);
        assert_eq!(direct.unwrap_err().kind(), ConfigErrorKind::Oversized);
        let _ = std::fs::remove_dir_all(&root);
    }
}
