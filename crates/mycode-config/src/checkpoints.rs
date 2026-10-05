//! Per-session file checkpoints for tool-driven edits.
//!
//! Before a mutating tool (write/edit) touches a file, the host records the
//! current bytes into `<home>/checkpoints/<session-id>/`. Snapshots are
//! content-addressed (blake3), bounded per session, and recorded in an
//! append-only JSONL manifest. Each record is tagged with the branch-head
//! event id at snapshot time so a later recall can restore the first image
//! taken after that event. A file that does not exist yet is recorded as
//! absent and deleted on rollback. A later turn that finds the file again
//! stores a fresh image under the new head. Files over the size limit fail
//! the checkpoint visibly, except a path already marked absent that has since
//! grown past the limit: that later turn is not snapshotted.
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
    /// Branch-head event id (`evt1-...`) when this image was taken.
    ///
    /// Empty for records written before point-in-time rollback. Those rows
    /// still participate in a whole-session rollback and are ignored by
    /// [`plan_rollback_after`].
    #[serde(default)]
    pub head: String,
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
/// `head` is the branch-head event id at snapshot time. The first record for
/// a `(path, head)` pair is kept; a later call with the same pair returns
/// `Ok(None)` without reading the path. A missing file is recorded as
/// [`CheckpointKind::Absent`] with no blob. When some earlier head already
/// marked the path absent and the file has since grown past
/// [`MAX_SNAPSHOT_FILE_BYTES`], this returns `Ok(None)` instead of failing,
/// because those bytes cannot be stored. Any other oversized file returns
/// [`ConfigErrorKind::Oversized`].
///
/// The same bytes under a new `head` are recorded again so a mid-thread
/// recall can find the image taken before that later turn.
///
/// # Errors
///
/// Returns [`ConfigError`] for oversized files, symlinks, snapshot IO
/// failures, or a corrupted manifest.
pub fn checkpoint_file(
    home: &HomeLayout,
    session_id: &str,
    path: &Path,
    head: &str,
) -> Result<Option<CheckpointEntry>, ConfigError> {
    let path_text = path_text(path)?;
    let dir = checkpoint_dir(home, session_id);
    std::fs::create_dir_all(&dir).map_err(|_| ConfigError::authority_rejection())?;
    let manifest_path = dir.join(MANIFEST_NAME);
    let entries = read_manifest(&manifest_path)?;
    // The first image at this head is the pre-mutation state for the turn.
    if entries
        .iter()
        .any(|entry| entry.path == path_text && entry.head == head)
    {
        return Ok(None);
    }

    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(append_absent(
                &manifest_path,
                &entries,
                &path_text,
                head,
            )?));
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
        if absent_already(&entries, &path_text) {
            return Ok(None);
        }
        return Err(oversized(meta.len()));
    }
    let bytes =
        std::fs::read(path).map_err(|_| ConfigError::for_path(ConfigErrorKind::Io, path))?;
    if bytes.len() > MAX_SNAPSHOT_FILE_BYTES {
        if absent_already(&entries, &path_text) {
            return Ok(None);
        }
        return Err(oversized(bytes.len() as u64));
    }
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    let digest = hasher.finalize().to_hex().to_string();
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
        head: head.to_owned(),
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
    plan_selected(home, session_id, |_| true)
}

/// Plans rollback to the first record of each path whose `head` is in `heads_after`.
///
/// Records with an empty head are ignored. For each path, the earliest
/// matching record is the image taken immediately before the first write
/// after the recall point: a snapshot restores those bytes, and an absent
/// record deletes the path. Paths with no matching record are left alone.
///
/// # Errors
///
/// Returns [`ConfigError`] for IO, a missing blob, or manifest corruption.
pub fn plan_rollback_after(
    home: &HomeLayout,
    session_id: &str,
    heads_after: &HashSet<String>,
) -> Result<Vec<RollbackAction>, ConfigError> {
    if heads_after.is_empty() {
        return Ok(Vec::new());
    }
    plan_selected(home, session_id, |head| {
        !head.is_empty() && heads_after.contains(head)
    })
}

fn plan_selected(
    home: &HomeLayout,
    session_id: &str,
    include: impl Fn(&str) -> bool,
) -> Result<Vec<RollbackAction>, ConfigError> {
    let entries = list_checkpoints(home, session_id)?;
    let dir = checkpoint_dir(home, session_id);
    let mut seen = HashSet::new();
    let mut actions = Vec::new();
    for entry in entries {
        if !include(&entry.head) {
            continue;
        }
        if !seen.insert(entry.path.clone()) {
            continue;
        }
        actions.push(action_for(&dir, entry)?);
    }
    Ok(actions)
}

fn action_for(dir: &Path, entry: CheckpointEntry) -> Result<RollbackAction, ConfigError> {
    let CheckpointEntry {
        path,
        kind,
        seq,
        digest,
        ..
    } = entry;
    match kind {
        CheckpointKind::Absent => Ok(RollbackAction::Delete { path }),
        CheckpointKind::Snapshot => {
            let blob_name = format!("{seq}-{digest}.bin");
            let blob_path = dir.join(&blob_name);
            let bytes = std::fs::read(&blob_path).map_err(|_| {
                ConfigError::for_path(ConfigErrorKind::Io, &blob_path)
                    .with_detail("checkpoint blob is missing")
            })?;
            Ok(RollbackAction::Restore { path, bytes })
        }
    }
}

fn append_absent(
    manifest_path: &Path,
    entries: &[CheckpointEntry],
    path_text: &str,
    head: &str,
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
        head: head.to_owned(),
    };
    append_manifest(manifest_path, &entry)?;
    Ok(entry)
}

fn absent_already(entries: &[CheckpointEntry], path_text: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.path == path_text && entry.kind == CheckpointKind::Absent)
}

fn oversized(len: u64) -> ConfigError {
    ConfigError::new(ConfigErrorKind::Oversized).with_detail(format!(
        "file is {len} bytes; checkpoint limit is {MAX_SNAPSHOT_FILE_BYTES} bytes"
    ))
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
        let entry = checkpoint_file(&home, "s", &file, "evt-size")
            .unwrap()
            .unwrap();
        assert_eq!(entry.kind, CheckpointKind::Snapshot);
        assert_eq!(entry.bytes, MAX_SNAPSHOT_FILE_BYTES as u64);

        let huge = root.join("huge.bin");
        std::fs::write(&huge, vec![2u8; MAX_SNAPSHOT_FILE_BYTES + 1]).unwrap();
        let error = checkpoint_file(&home, "s", &huge, "evt-size").unwrap_err();
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
        let entry = checkpoint_file(&home, "s", &file, "evt-size")
            .unwrap()
            .unwrap();
        assert_eq!(entry.kind, CheckpointKind::Absent);
        std::fs::write(&file, vec![9u8; MAX_SNAPSHOT_FILE_BYTES + 64]).unwrap();
        assert!(
            checkpoint_file(&home, "s", &file, "evt-new")
                .unwrap()
                .is_none()
        );
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
        let error = checkpoint_file(&home, "s", &link, "evt-link").unwrap_err();
        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        assert!(error.summary().contains("symlink"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"safe");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn point_in_time_restores_the_first_image_after_the_turn() {
        let (root, home) = scratch("point");
        let file = root.join("note.txt");
        std::fs::write(&file, b"v0").unwrap();
        checkpoint_file(&home, "s", &file, "evt-1")
            .unwrap()
            .unwrap();
        std::fs::write(&file, b"v1").unwrap();
        checkpoint_file(&home, "s", &file, "evt-2")
            .unwrap()
            .unwrap();
        std::fs::write(&file, b"v2").unwrap();
        let created = root.join("created.txt");
        let absent = checkpoint_file(&home, "s", &created, "evt-2")
            .unwrap()
            .unwrap();
        assert_eq!(absent.kind, CheckpointKind::Absent);
        assert_eq!(absent.head, "evt-2");
        std::fs::write(&created, b"new").unwrap();

        let heads = HashSet::from(["evt-2".to_owned()]);
        let actions = plan_rollback_after(&home, "s", &heads).unwrap();
        assert_eq!(
            actions,
            vec![
                RollbackAction::Restore {
                    path: file.to_str().unwrap().to_owned(),
                    bytes: b"v1".to_vec(),
                },
                RollbackAction::Delete {
                    path: created.to_str().unwrap().to_owned(),
                },
            ]
        );
        let earliest = plan_rollback(&home, "s").unwrap();
        assert_eq!(
            earliest,
            vec![
                RollbackAction::Restore {
                    path: file.to_str().unwrap().to_owned(),
                    bytes: b"v0".to_vec(),
                },
                RollbackAction::Delete {
                    path: created.to_str().unwrap().to_owned(),
                },
            ]
        );
        let legacy_only = HashSet::from(["evt-missing".to_owned()]);
        assert!(
            plan_rollback_after(&home, "s", &legacy_only)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn same_bytes_at_a_later_head_are_recorded_again() {
        let (root, home) = scratch("digest");
        let file = root.join("note.txt");
        std::fs::write(&file, b"A").unwrap();
        checkpoint_file(&home, "s", &file, "evt-1")
            .unwrap()
            .unwrap();
        std::fs::write(&file, b"B").unwrap();
        checkpoint_file(&home, "s", &file, "evt-2")
            .unwrap()
            .unwrap();
        std::fs::write(&file, b"A").unwrap();
        let again = checkpoint_file(&home, "s", &file, "evt-3")
            .unwrap()
            .unwrap();
        assert_eq!(again.head, "evt-3");
        assert_eq!(again.bytes, 1);
        std::fs::write(&file, b"C").unwrap();

        let heads = HashSet::from(["evt-3".to_owned()]);
        let actions = plan_rollback_after(&home, "s", &heads).unwrap();
        assert_eq!(
            actions,
            vec![RollbackAction::Restore {
                path: file.to_str().unwrap().to_owned(),
                bytes: b"A".to_vec(),
            }]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn later_head_snapshots_a_file_created_earlier() {
        let (root, home) = scratch("later");
        let file = root.join("note.txt");
        let created = checkpoint_file(&home, "s", &file, "evt-1")
            .unwrap()
            .unwrap();
        assert_eq!(created.kind, CheckpointKind::Absent);
        std::fs::write(&file, b"hello").unwrap();
        let edited = checkpoint_file(&home, "s", &file, "evt-2")
            .unwrap()
            .unwrap();
        assert_eq!(edited.kind, CheckpointKind::Snapshot);
        assert_eq!(edited.head, "evt-2");
        std::fs::write(&file, b"world").unwrap();

        let heads = HashSet::from(["evt-2".to_owned()]);
        let actions = plan_rollback_after(&home, "s", &heads).unwrap();
        assert_eq!(
            actions,
            vec![RollbackAction::Restore {
                path: file.to_str().unwrap().to_owned(),
                bytes: b"hello".to_vec(),
            }]
        );
        assert_eq!(
            plan_rollback(&home, "s").unwrap(),
            vec![RollbackAction::Delete {
                path: file.to_str().unwrap().to_owned(),
            }]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_line_is_not_a_point_in_time_match() {
        let (root, home) = scratch("legacy-head");
        let file = root.join("old.txt");
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
        let listed = list_checkpoints(&home, "s").unwrap();
        assert_eq!(listed[0].head, "");
        let heads = HashSet::from([String::new()]);
        assert!(plan_rollback_after(&home, "s", &heads).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
