//! Coordinates validated owned-file reads and locked atomic updates.
//!
//! This module deliberately contains no document schema. It validates every
//! relative path through [`HomeLayout`], then delegates handle-relative native
//! operations to the active platform implementation.

use std::ffi::OsString;
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

    pub(super) fn ensure_directory(
        _root: &Path,
        _components: &[OsString],
    ) -> Result<(), ConfigError> {
        Err(unavailable())
    }

    pub(super) fn read_file(
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

        pub(super) fn require_private_lock(&self) -> Result<(), ConfigError> {
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

/// Creates only the owned directories named by `relative`.
///
/// Every component is created no-follow and private, so callers can
/// materialize authority directories below the owned root on demand.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::PathEscape`] for unsafe components and native
/// security, access, identity, or durability failures otherwise.
pub fn ensure_owned_directory(
    home: &HomeLayout,
    relative: impl AsRef<Path>,
) -> Result<(), ConfigError> {
    let path = OwnedPath::new(home, relative.as_ref())?;
    platform::ensure_directory(&path.root, &path.components)
}

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

/// Runs one read-modify-replace callback under a persistent advisory lock.
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
    transaction.require_private_lock()?;
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
