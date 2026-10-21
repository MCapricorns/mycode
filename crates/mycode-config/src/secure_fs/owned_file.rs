//! Coordinates validated owned-file reads and locked atomic updates.
//!
//! This module deliberately contains no document schema. It validates every
//! relative path through [`HomeLayout`], then delegates handle-relative native
//! operations to the active platform implementation.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use zeroize::Zeroizing;

use crate::{ConfigError, ConfigErrorKind, HomeLayout};

#[cfg(unix)]
use super::unix::unix_file as platform;
#[cfg(windows)]
use super::windows::windows_file as platform;

#[cfg(not(any(unix, windows)))]
mod fallback {
    use super::{ConfigError, ConfigErrorKind, OsString, Path, Zeroizing};

    pub(super) fn read_file(
        _root: &Path,
        _components: &[OsString],
        _maximum_bytes: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
        Err(unavailable())
    }

    pub(super) fn quarantine_file(
        _root: &Path,
        _components: &[OsString],
        _backup: &std::ffi::OsStr,
    ) -> Result<bool, ConfigError> {
        Err(unavailable())
    }

    pub(super) fn read_backup(
        _root: &Path,
        _components: &[OsString],
        _maximum_bytes: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
        Err(unavailable())
    }

    pub(super) struct Transaction;

    impl Transaction {
        pub(super) fn begin(_root: &Path, _components: &[OsString]) -> Result<Self, ConfigError> {
            Err(unavailable())
        }

        pub(super) fn read(
            &mut self,
            _maximum_bytes: usize,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
            Err(unavailable())
        }

        pub(super) fn replace(&mut self, _bytes: &[u8]) -> Result<(), ConfigError> {
            Err(unavailable())
        }
    }

    fn unavailable() -> ConfigError {
        ConfigError::new(ConfigErrorKind::AccessControl)
    }
}

#[cfg(not(any(unix, windows)))]
use fallback as platform;

/// Reads a private regular file without creating any filesystem object.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::Oversized`] when content exceeds
/// `maximum_bytes` and [`ConfigError`] for owned-path security, access,
/// identity, or I/O failures.
pub fn read_owned_file(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
    maximum_bytes: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    require_file_name(&path)?;
    platform::read_file(&path.root, &path.components, maximum_bytes)
}

/// Moves one owned file aside to `backup` (a sibling name) without reading it.
///
/// Used to recover a document the strict read path rejects for its own
/// permissions, owner, or type: the bytes are kept under the backup name and
/// the original name is free for a fresh default. The owned home and its
/// directories are still opened under the strict checks, so an untrusted
/// home fails here instead of being repaired. `Ok(false)` means nothing was
/// at the path.
///
/// # Errors
///
/// Returns [`ConfigError`] when the home cannot be opened safely or the
/// rename fails.
pub(crate) fn quarantine_owned_file(
    home: &HomeLayout,
    relative: &str,
    backup: &str,
) -> Result<bool, ConfigError> {
    let path = OwnedPath::new(home, Path::new(relative))?;
    require_file_name(&path)?;
    let backup_path = OwnedPath::new(home, Path::new(backup))?;
    let (Some((backup_name, backup_dirs)), Some((_, dirs))) = (
        backup_path.components.split_last(),
        path.components.split_last(),
    ) else {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    };
    if backup_dirs != dirs {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    platform::quarantine_file(&path.root, &path.components, backup_name)
}

/// Reads a quarantined backup without the DACL-mode check.
///
/// [`quarantine_owned_file`] renames the file, so a permissions mismatch
/// (an external writer's inherited ACL) moves with the bytes and would fail
/// [`read_owned_file`] again. Ownership is still verified — a file the
/// current user does not own is never salvaged — and the read stays bounded.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::Oversized`] when content exceeds
/// `maximum_bytes` and [`ConfigError`] for owned-path security, ownership,
/// or I/O failures.
pub(crate) fn read_quarantined_backup(
    home: &HomeLayout,
    backup: &str,
    maximum_bytes: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, ConfigError> {
    let path = OwnedPath::new(home, Path::new(backup))?;
    require_file_name(&path)?;
    platform::read_backup(&path.root, &path.components, maximum_bytes)
}

/// Runs one read-modify-replace callback under an advisory lock.
///
/// The lock lives in the operating system (a lock on the directory handle on
/// Unix, a named mutex on Windows), so no `<name>.lock` file is left beside
/// the documents.
///
/// # Errors
///
/// Returns the callback error unchanged plus [`ConfigError`] for lock,
/// access, identity, or durability failures.
pub fn locked_update_owned_file(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
    maximum_bytes: usize,
    update: impl FnOnce(Option<&[u8]>) -> Result<Vec<u8>, ConfigError>,
) -> Result<(), ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    require_file_name(&path)?;
    let mut transaction = platform::Transaction::begin(&path.root, &path.components)?;
    let current = transaction.read(maximum_bytes)?;
    let replacement = update(current.as_ref().map(|bytes| bytes.as_slice()))?;
    if replacement.len() > maximum_bytes {
        return Err(ConfigError::new(ConfigErrorKind::Oversized));
    }
    transaction.replace(&replacement)
}

struct OwnedPath {
    root: PathBuf,
    components: Vec<OsString>,
}

impl OwnedPath {
    fn new(home: &HomeLayout, relative: &Path) -> Result<Self, ConfigError> {
        let joined = home.owned_join(relative)?;
        let relative = joined
            .strip_prefix(home.root())
            .map_err(|_| ConfigError::new(ConfigErrorKind::PathEscape))?;
        let components = relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name.to_os_string()),
                _ => Err(ConfigError::new(ConfigErrorKind::PathEscape)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            root: home.root().to_path_buf(),
            components,
        })
    }
}

fn require_file_name(path: &OwnedPath) -> Result<(), ConfigError> {
    if path.components.is_empty() {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(())
}

/// Name of the `<name>.lock` file older builds kept beside each document.
/// Transactions delete one when they see it.
pub(in crate::secure_fs) fn legacy_lock_name(destination: &OsStr) -> OsString {
    let mut name = destination.to_os_string();
    name.push(".lock");
    name
}

/// Fresh hidden temporary name beside the file being replaced.
pub(in crate::secure_fs) fn temporary_name(destination: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(destination);
    name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    name
}
