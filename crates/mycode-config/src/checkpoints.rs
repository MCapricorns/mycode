//! Per-session file checkpoints for tool-driven edits.
//!
//! Before a mutating tool (write/edit) touches a file, the host records the
//! current bytes into `<home>/checkpoints/<session-id>/`. Snapshots are
//! content-addressed (blake3), bounded per session, and recorded in an
//! append-only JSONL manifest. A file that does not exist yet is recorded as
//! absent and deleted on rollback; its later contents are not snapshotted.
//! Files over the size limit fail the checkpoint visibly.
//!
//! Bounds: 256 records per session, 8 MiB per snapshotted file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use blake3::Hasher;
use serde::{Deserialize, Serialize};

use crate::{ConfigError, ConfigErrorKind, HomeLayout};

/// Maximum records retained per session.
pub(crate) const MAX_SNAPSHOTS_PER_SESSION: usize = 256;
/// Maximum snapshotted file size in bytes.
pub(crate) const MAX_SNAPSHOT_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Exact manifest file name.
pub const MANIFEST_NAME: &str = "manifest.jsonl";

/// Whether a checkpoint stores prior bytes or records that the path was absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckpointKind {
    /// Prior file bytes are stored in a content-addressed blob.
    #[default]
    Snapshot,
    /// The path did not exist; rollback deletes it instead of writing bytes.
    Absent,
}

/// One recorded checkpoint in a session manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointEntry {
    /// Monotonic sequence number within the session.
    pub seq: u64,
    /// Absolute workspace path of the file.
    pub path: String,
    /// Snapshot of prior bytes, or an absent marker for a file created later.
    #[serde(default)]
    pub kind: CheckpointKind,
    /// blake3 digest of the snapshotted bytes. Empty for [`CheckpointKind::Absent`].
    pub digest: String,
    /// Snapshotted byte length. Zero for [`CheckpointKind::Absent`].
    pub bytes: u64,
}

/// One rollback step planned from the earliest record of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RollbackAction {
    /// Replace the path with these prior bytes through the no-follow write path.
    Restore {
        /// Absolute path recorded in the manifest.
        path: String,
        /// Exact prior bytes.
        bytes: Vec<u8>,
    },
    /// Delete a path that did not exist at the checkpoint.
    Delete {
        /// Absolute path recorded in the manifest.
        path: String,
    },
}

/// Records one file before mutation.
///
/// A missing file is recorded as [`CheckpointKind::Absent`] with no blob.
/// When an absent record already exists, this returns `Ok(None)` without
/// reading the path, so a file created after the checkpoint can grow past
/// the size limit without being snapshotted. An existing file over
/// [`MAX_SNAPSHOT_FILE_BYTES`] returns [`ConfigErrorKind::Oversized`].
///
/// # Errors
///
/// Returns [`ConfigError`] for oversized files, symlinks, snapshot IO
/// failures, or a corrupted manifest.
pub fn checkpoint_file(
    home: &HomeLayout,
    session_id: &str,
    path: &Path,
) -> Result<Option<CheckpointEntry>, ConfigError> {
    let path_text = path_text(path)?;
    let dir = checkpoint_dir(home, session_id);
    std::fs::create_dir_all(&dir).map_err(|_| ConfigError::authority_rejection())?;
    let manifest_path = dir.join(MANIFEST_NAME);
    let entries = read_manifest(&manifest_path)?;
    if entries
        .iter()
        .any(|entry| entry.path == path_text && entry.kind == CheckpointKind::Absent)
    {
        return Ok(None);
    }

    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(append_absent(&manifest_path, &entries, &path_text)?));
        }
        Err(_) => return Err(ConfigError::for_path(ConfigErrorKind::Io, path)),
    };
    if meta.file_type().is_symlink() {
        return Err(ConfigError::for_path(ConfigErrorKind::LinkEscape, path)
            .with_detail("checkpoint target is a symlink"));
    }
    if !meta.file_type().is_file() {
        return Err(ConfigError::for_path(ConfigErrorKind::Io, path)
            .with_detail("checkpoint target is not a regular file"));
    }
    if meta.len() > MAX_SNAPSHOT_FILE_BYTES as u64 {
        return Err(
            ConfigError::new(ConfigErrorKind::Oversized).with_detail(format!(
                "file is {} bytes; checkpoint limit is {MAX_SNAPSHOT_FILE_BYTES} bytes",
                meta.len()
            )),
        );
    }
    let bytes =
        std::fs::read(path).map_err(|_| ConfigError::for_path(ConfigErrorKind::Io, path))?;
    if bytes.len() > MAX_SNAPSHOT_FILE_BYTES {
        return Err(
            ConfigError::new(ConfigErrorKind::Oversized).with_detail(format!(
                "file is {} bytes; checkpoint limit is {MAX_SNAPSHOT_FILE_BYTES} bytes",
                bytes.len()
            )),
        );
    }
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    let digest = hasher.finalize().to_hex().to_string();
    if entries
        .iter()
        .any(|entry| entry.path == path_text && entry.digest == digest)
    {
        return Ok(None);
    }
    if entries.len() >= MAX_SNAPSHOTS_PER_SESSION {
        return Err(ConfigError::new(ConfigErrorKind::CheckpointLimit));
    }
    let seq = entries.last().map(|entry| entry.seq + 1).unwrap_or(1);
    let blob = format!("{seq}-{digest}.bin");
    std::fs::write(dir.join(&blob), &bytes).map_err(|_| ConfigError::authority_rejection())?;
    let entry = CheckpointEntry {
        seq,
        path: path_text,
        kind: CheckpointKind::Snapshot,
        digest,
        bytes: bytes.len() as u64,
    };
    append_manifest(&manifest_path, &entry)?;
    Ok(Some(entry))
}

/// Lists one session's checkpoints in manifest order.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO or manifest corruption.
pub(crate) fn list_checkpoints(
    home: &HomeLayout,
    session_id: &str,
) -> Result<Vec<CheckpointEntry>, ConfigError> {
    read_manifest(&checkpoint_dir(home, session_id).join(MANIFEST_NAME))
}

/// Plans rollback to the earliest record of each path.
///
/// Absent records become [`RollbackAction::Delete`]. Snapshot records load
/// their blob; a missing blob is an error. The caller applies each action
/// through the no-follow file kernel and stops at the first failure.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO, a missing blob, or manifest corruption.
pub fn plan_rollback(
    home: &HomeLayout,
    session_id: &str,
) -> Result<Vec<RollbackAction>, ConfigError> {
    let entries = list_checkpoints(home, session_id)?;
    let dir = checkpoint_dir(home, session_id);
    let mut seen = HashSet::new();
    let mut actions = Vec::new();
    for entry in entries {
        if !seen.insert(entry.path.clone()) {
            continue;
        }
        let CheckpointEntry {
            path,
            kind,
            seq,
            digest,
            ..
        } = entry;
        match kind {
            CheckpointKind::Absent => actions.push(RollbackAction::Delete { path }),
            CheckpointKind::Snapshot => {
                let blob_name = format!("{seq}-{digest}.bin");
                let blob_path = dir.join(&blob_name);
                let bytes = std::fs::read(&blob_path).map_err(|_| {
                    ConfigError::for_path(ConfigErrorKind::Io, &blob_path)
                        .with_detail("checkpoint blob is missing")
                })?;
                actions.push(RollbackAction::Restore { path, bytes });
            }
        }
    }
    Ok(actions)
}

fn append_absent(
    manifest_path: &Path,
    entries: &[CheckpointEntry],
    path_text: &str,
) -> Result<CheckpointEntry, ConfigError> {
    if entries.len() >= MAX_SNAPSHOTS_PER_SESSION {
        return Err(ConfigError::new(ConfigErrorKind::CheckpointLimit));
    }
    let seq = entries.last().map(|entry| entry.seq + 1).unwrap_or(1);
    let entry = CheckpointEntry {
        seq,
        path: path_text.to_owned(),
        kind: CheckpointKind::Absent,
        digest: String::new(),
        bytes: 0,
    };
    append_manifest(manifest_path, &entry)?;
    Ok(entry)
}

fn path_text(path: &Path) -> Result<String, ConfigError> {
    path.to_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            ConfigError::for_path(ConfigErrorKind::NonUtf8, path)
                .with_detail("checkpoint path is not valid Unicode")
        })
}

fn checkpoint_dir(home: &HomeLayout, session_id: &str) -> PathBuf {
    home.root().join("checkpoints").join(session_id)
}

fn read_manifest(path: &Path) -> Result<Vec<CheckpointEntry>, ConfigError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ConfigError::for_path(ConfigErrorKind::Io, path)),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| ConfigError::authority_rejection())?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: CheckpointEntry =
            serde_json::from_str(line).map_err(|_| ConfigError::authority_rejection())?;
        entries.push(entry);
    }
    Ok(entries)
}

fn append_manifest(path: &Path, entry: &CheckpointEntry) -> Result<(), ConfigError> {
    use std::io::Write as _;
    let line = serde_json::to_string(entry).map_err(|_| ConfigError::authority_rejection())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| ConfigError::authority_rejection())?;
    writeln!(file, "{line}").map_err(|_| ConfigError::authority_rejection())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> (PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-ckpt-{label}-{}-{}",
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
    fn exact_limit_snapshots_and_over_limit_is_visible() {
        let (root, home) = scratch("size");
        let file = root.join("blob.bin");
        std::fs::write(&file, vec![1u8; MAX_SNAPSHOT_FILE_BYTES]).unwrap();
        let entry = checkpoint_file(&home, "s", &file).unwrap().unwrap();
        assert_eq!(entry.kind, CheckpointKind::Snapshot);
        assert_eq!(entry.bytes, MAX_SNAPSHOT_FILE_BYTES as u64);

        let huge = root.join("huge.bin");
        std::fs::write(&huge, vec![2u8; MAX_SNAPSHOT_FILE_BYTES + 1]).unwrap();
        let error = checkpoint_file(&home, "s", &huge).unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::Oversized);
        let summary = error.summary();
        assert!(
            summary.contains("checkpoint limit"),
            "oversized failure must be visible, got {summary}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_file_is_absent_and_not_reread_when_it_grows() {
        let (root, home) = scratch("new");
        let file = root.join("created.txt");
        let entry = checkpoint_file(&home, "s", &file).unwrap().unwrap();
        assert_eq!(entry.kind, CheckpointKind::Absent);
        std::fs::write(&file, vec![9u8; MAX_SNAPSHOT_FILE_BYTES + 64]).unwrap();
        assert!(checkpoint_file(&home, "s", &file).unwrap().is_none());
        let actions = plan_rollback(&home, "s").unwrap();
        assert_eq!(
            actions,
            vec![RollbackAction::Delete {
                path: file.to_str().unwrap().to_owned(),
            }]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_manifest_line_restores_as_snapshot() {
        let (root, home) = scratch("legacy");
        let file = root.join("old.txt");
        std::fs::write(&file, b"prior").unwrap();
        let dir = checkpoint_dir(&home, "s");
        std::fs::create_dir_all(&dir).unwrap();
        let digest = {
            let mut hasher = Hasher::new();
            hasher.update(b"prior");
            hasher.finalize().to_hex().to_string()
        };
        std::fs::write(dir.join(format!("1-{digest}.bin")), b"prior").unwrap();
        let line = format!(
            r#"{{"seq":1,"path":"{}","digest":"{digest}","bytes":5}}"#,
            file.display()
        );
        std::fs::write(dir.join(MANIFEST_NAME), format!("{line}\n")).unwrap();
        let actions = plan_rollback(&home, "s").unwrap();
        match &actions[0] {
            RollbackAction::Restore { bytes, .. } => assert_eq!(bytes, b"prior"),
            RollbackAction::Delete { path } => panic!("legacy line became delete of {path}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_checkpoint_is_link_escape() {
        let (root, home) = scratch("link");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"safe").unwrap();
        let link = root.join("link.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let error = checkpoint_file(&home, "s", &link).unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        assert!(error.summary().contains("symlink"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"safe");
        let _ = std::fs::remove_dir_all(&root);
    }
}
