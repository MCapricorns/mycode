//! Unix no-follow owned-directory primitives with private modes and durability.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::home::validate_path_component;
use crate::{ConfigError, ConfigErrorKind};

#[path = "unix_file.rs"]
pub(super) mod unix_file;

const DIRECTORY_MODE: rfs::RawMode = 0o700;

pub(super) fn find_wrong_case_child(
    directory: &File,
    expected: &str,
) -> Result<Option<OsString>, ConfigError> {
    let entries = rfs::Dir::read_from(directory.as_fd())
        .map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    for entry in entries {
        let entry = entry.map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
        let name = OsStr::from_bytes(entry.file_name().to_bytes());
        let Some(text) = name.to_str() else {
            continue;
        };
        if text != expected && text.eq_ignore_ascii_case(expected) {
            return Ok(Some(name.to_os_string()));
        }
    }
    Ok(None)
}

pub(super) fn create_owned_root(
    root: &Path,
    expected_root_name: Option<&str>,
) -> Result<File, ConfigError> {
    if !root.is_absolute()
        || !root
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    }
    let Some(parent_path) = root.parent() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    let Some(name) = root.file_name() else {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, root));
    };
    validate_path_component(name)?;
    let parent = open_trailing_directory(parent_path)?;
    if let Some(expected) = expected_root_name {
        reject_wrong_case_child(&parent, expected, ConfigErrorKind::InvalidHome)?;
    }
    let root = create_or_open_directory(&parent, name, true)?;
    if let Some(expected) = expected_root_name {
        reject_wrong_case_child(&parent, expected, ConfigErrorKind::InvalidHome)?;
    }
    Ok(root)
}

pub(super) fn open_trailing_directory(path: &Path) -> Result<File, ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::for_path(ConfigErrorKind::InvalidHome, path));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    match rfs::open(path, flags, Mode::empty()) {
        Ok(descriptor) => Ok(File::from(descriptor)),
        Err(error) => Err(map_path_errno(path, error, ConfigErrorKind::Io)),
    }
}

pub(super) fn reject_wrong_case_child(
    directory: &File,
    expected: &str,
    kind: ConfigErrorKind,
) -> Result<(), ConfigError> {
    if find_wrong_case_child(directory, expected)?.is_some() {
        return Err(ConfigError::new(kind));
    }
    Ok(())
}

pub(super) fn create_or_open_directory(
    parent: &File,
    name: &OsStr,
    owned: bool,
) -> Result<File, ConfigError> {
    reject_link_or_wrong_type(parent, name, true)?;
    let created = match rfs::mkdirat(parent.as_fd(), name, Mode::from_raw_mode(DIRECTORY_MODE)) {
        Ok(()) => true,
        Err(Errno::EXIST) => false,
        Err(error) => return Err(map_errno(error, ConfigErrorKind::Io)),
    };
    let directory = open_existing_directory(parent, name)?;
    if created || owned {
        enforce_owned_directory(&directory)?;
    }
    if created {
        sync_created_directory(&directory, parent)?;
    }
    Ok(directory)
}

pub(super) fn open_existing_directory(parent: &File, name: &OsStr) -> Result<File, ConfigError> {
    reject_link_or_wrong_type(parent, name, false)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    match open_component(parent, name, flags) {
        Ok(descriptor) => {
            let directory = File::from(descriptor);
            let stat = rfs::fstat(directory.as_fd())
                .map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
            if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::Directory {
                return Err(ConfigError::new(ConfigErrorKind::Io)
                    .with_io_kind(io::ErrorKind::NotADirectory));
            }
            Ok(directory)
        }
        Err(error) => {
            reject_link_or_wrong_type(parent, name, false)?;
            Err(error)
        }
    }
}

fn reject_link_or_wrong_type(
    parent: &File,
    name: &OsStr,
    missing_allowed: bool,
) -> Result<(), ConfigError> {
    match rfs::statat(parent.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            match rfs::FileType::from_raw_mode(stat.st_mode) {
                rfs::FileType::Symlink => Err(ConfigError::new(ConfigErrorKind::LinkEscape)),
                rfs::FileType::Directory => Ok(()),
                _ => Err(ConfigError::new(ConfigErrorKind::Io)
                    .with_io_kind(io::ErrorKind::NotADirectory)),
            }
        }
        Err(Errno::NOENT) if missing_allowed => Ok(()),
        Err(error) => Err(map_errno(error, ConfigErrorKind::Io)),
    }
}

pub(super) fn open_component(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    open_component_with_mounts(parent, name, flags, Mode::empty())
}

/// Creates one component with the private mode in the creating call itself.
///
/// Darwin resolves a contested `O_CREAT` without `O_EXCL` into `EACCES` (after
/// another thread created a zero-mode entry) or a spurious `ENOENT`, so a
/// caller that must create passes the final mode up front and treats an
/// `EEXIST` from its `O_EXCL` attempt as a lost race to retry as an opener.
pub(super) fn create_component(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
    mode: Mode,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    open_component_with_mounts(parent, name, flags, mode)
}

fn open_component_with_mounts(
    parent: &File,
    name: &OsStr,
    flags: OFlags,
    mode: Mode,
) -> Result<std::os::fd::OwnedFd, ConfigError> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let resolve = rfs::ResolveFlags::BENEATH | rfs::ResolveFlags::NO_SYMLINKS;
        rfs::openat2(parent.as_fd(), name, flags, mode, resolve).map_err(|error| {
            if error == Errno::NOSYS {
                ConfigError::new(ConfigErrorKind::AccessControl)
                    .with_io_kind(io::ErrorKind::Unsupported)
            } else {
                map_errno(error, ConfigErrorKind::Io)
            }
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        // The caller supplies one validated component and O_NOFOLLOW, so the
        // portable openat fallback remains anchored to `parent`.
        rfs::openat(parent.as_fd(), name, flags, mode)
            .map_err(|error| map_errno(error, ConfigErrorKind::Io))
    }
}

/// Pins a directory owned by the current user to mode `0o700`.
///
/// The owner and file type are checked before any change. A wrong owner or a
/// non-directory still fails closed, and nothing is chmod'd. A directory that
/// already has mode `0o700` is left untouched. Any other mode is replaced
/// with `0o700` and checked again.
///
/// [`verify_owned_directory`] is the non-mutating form. Read paths that only
/// called it rejected a home created as `0o755` (the usual umask) and Settings
/// never left its loading error.
pub(super) fn enforce_owned_directory(directory: &File) -> Result<(), ConfigError> {
    let stat = verify_owned_directory_owner(directory)?;
    if stat.st_mode & 0o777 != DIRECTORY_MODE {
        rfs::fchmod(directory.as_fd(), Mode::from_raw_mode(DIRECTORY_MODE))
            .map_err(|error| map_errno(error, ConfigErrorKind::AccessControl))?;
    }
    verify_owned_directory(directory)
}

/// Rejects a directory that is not owned by the current user or whose
/// permission bits are not `0o700`.
///
/// This check does not change the directory. Callers that should repair a
/// mode on a directory they own use [`enforce_owned_directory`].
pub(super) fn verify_owned_directory(directory: &File) -> Result<(), ConfigError> {
    let stat = verify_owned_directory_owner(directory)?;
    if stat.st_mode & 0o777 != DIRECTORY_MODE {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(())
}

fn verify_owned_directory_owner(directory: &File) -> Result<rfs::Stat, ConfigError> {
    let stat =
        rfs::fstat(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))?;
    if rfs::FileType::from_raw_mode(stat.st_mode) != rfs::FileType::Directory {
        return Err(
            ConfigError::new(ConfigErrorKind::Io).with_io_kind(io::ErrorKind::NotADirectory)
        );
    }
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(ConfigError::new(ConfigErrorKind::AccessControl));
    }
    Ok(stat)
}

fn sync_created_directory(directory: &File, parent: &File) -> Result<(), ConfigError> {
    sync_directory(directory)?;
    sync_directory(parent)
}

#[cfg(target_vendor = "apple")]
pub(super) fn sync_directory(directory: &File) -> Result<(), ConfigError> {
    rfs::fcntl_fullfsync(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))
}

#[cfg(not(target_vendor = "apple"))]
pub(super) fn sync_directory(directory: &File) -> Result<(), ConfigError> {
    rfs::fsync(directory.as_fd()).map_err(|error| map_errno(error, ConfigErrorKind::Io))
}

pub(super) fn map_errno(error: Errno, kind: ConfigErrorKind) -> ConfigError {
    if error == Errno::LOOP {
        return ConfigError::new(ConfigErrorKind::LinkEscape);
    }
    ConfigError::new(kind).with_io_kind(io::Error::from(error).kind())
}

fn map_path_errno(path: &Path, error: Errno, kind: ConfigErrorKind) -> ConfigError {
    if error == Errno::LOOP {
        return ConfigError::for_path(ConfigErrorKind::LinkEscape, path);
    }
    ConfigError::for_path(kind, path).with_io_kind(io::Error::from(error).kind())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::ErrorKind;
    use std::os::fd::AsFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, chown, symlink};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rustix::fs::{self as rfs};
    use rustix::process::geteuid;

    use super::{enforce_owned_directory, verify_owned_directory};
    use crate::{
        AppSettings, AuthorityRevision, ConfigError, ConfigErrorKind, HomeLayout,
        read_app_settings, read_owned_file, replace_app_settings,
    };

    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "mycode-secure-fs-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("temp directory");
            Self { path }
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    fn mode_bits(path: &Path) -> u32 {
        fs::symlink_metadata(path)
            .unwrap_or_else(|error| panic!("metadata {}: {error}", path.display()))
            .permissions()
            .mode()
            & 0o777
    }

    fn assert_access_control(result: Result<(), ConfigError>) {
        assert_eq!(
            result.expect_err("access control").kind(),
            ConfigErrorKind::AccessControl
        );
    }

    fn foreign_uid() -> u32 {
        let euid = geteuid().as_raw();
        if euid == 65_534 { 1 } else { 65_534 }
    }

    #[test]
    fn verify_rejects_wrong_mode_and_enforce_tightens_it() {
        let tree = TempTree::new("mode");
        let path = tree.path.join("home");
        fs::create_dir(&path).expect("directory");
        let directory = File::open(&path).expect("open");
        for mode in [0o755_u32, 0o775, 0o707, 0o500] {
            set_mode(&path, mode);
            assert_access_control(verify_owned_directory(&directory));
            assert_eq!(mode_bits(&path), mode, "verify must not chmod");
            enforce_owned_directory(&directory).expect("enforce");
            assert_eq!(mode_bits(&path), 0o700);
            verify_owned_directory(&directory).expect("private after enforce");
        }
    }

    #[test]
    fn verify_and_enforce_accept_an_already_private_directory() {
        let tree = TempTree::new("private");
        let path = tree.path.join("home");
        fs::create_dir(&path).expect("directory");
        set_mode(&path, 0o700);
        let directory = File::open(&path).expect("open");
        verify_owned_directory(&directory).expect("verify");
        enforce_owned_directory(&directory).expect("enforce");
        assert_eq!(mode_bits(&path), 0o700);
    }

    #[test]
    fn verify_and_enforce_reject_a_regular_file_without_chmod() {
        let tree = TempTree::new("file");
        let path = tree.path.join("note");
        fs::write(&path, b"kept").expect("write");
        set_mode(&path, 0o644);
        let file = File::open(&path).expect("open");
        for result in [
            verify_owned_directory(&file),
            enforce_owned_directory(&file),
        ] {
            let error = result.expect_err("regular file");
            assert_eq!(error.kind(), ConfigErrorKind::Io);
            assert_eq!(error.io_kind(), Some(ErrorKind::NotADirectory));
        }
        assert_eq!(mode_bits(&path), 0o644);
        assert_eq!(fs::read(&path).expect("bytes"), b"kept");
    }

    #[test]
    fn foreign_owned_directory_is_rejected_without_chmod() {
        let tree = TempTree::new("foreign");
        let path = tree.path.join("home");
        fs::create_dir(&path).expect("directory");
        set_mode(&path, 0o755);
        if chown(&path, Some(foreign_uid()), None).is_ok()
            && fs::metadata(&path).expect("meta").uid() != geteuid().as_raw()
        {
            let directory = File::open(&path).expect("open");
            assert_rejected_without_mode_change(&directory);
            let home = HomeLayout::from_root(&path).expect("layout");
            let error = read_owned_file(&home, "settings.json", 64).expect_err("foreign home");
            assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
            assert_eq!(mode_bits(&path), 0o755);
            assert_eq!(fs::metadata(&path).expect("meta").uid(), foreign_uid());
            return;
        }

        assert_ne!(
            geteuid().as_raw(),
            0,
            "root must chown a temp directory instead of touching a system path"
        );
        let (path, directory) = open_foreign_system_directory();
        let mode = mode_bits(&path);
        let uid = fs::symlink_metadata(&path).expect("meta").uid();
        assert_rejected_without_mode_change(&directory);
        let home = HomeLayout::from_root(&path).expect("layout");
        let error = read_owned_file(&home, "settings.json", 64).expect_err("foreign home");
        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        assert_eq!(mode_bits(&path), mode);
        assert_eq!(fs::symlink_metadata(&path).expect("meta").uid(), uid);
    }

    fn assert_rejected_without_mode_change(directory: &File) {
        let before = rfs::fstat(directory.as_fd()).expect("fstat");
        assert_ne!(before.st_uid, geteuid().as_raw());
        assert_access_control(verify_owned_directory(directory));
        assert_access_control(enforce_owned_directory(directory));
        let after = rfs::fstat(directory.as_fd()).expect("fstat");
        assert_eq!(after.st_uid, before.st_uid);
        assert_eq!(after.st_mode, before.st_mode);
    }

    fn open_foreign_system_directory() -> (PathBuf, File) {
        for candidate in ["/usr", "/etc", "/var"] {
            let path = Path::new(candidate);
            let Ok(metadata) = fs::symlink_metadata(path) else {
                continue;
            };
            if !metadata.file_type().is_dir() || metadata.uid() == geteuid().as_raw() {
                continue;
            }
            if let Ok(file) = File::open(path) {
                return (path.to_path_buf(), file);
            }
        }
        panic!("no foreign-owned directory available");
    }

    #[test]
    fn settings_read_tightens_an_owned_home_left_at_755() {
        let tree = TempTree::new("settings");
        let parent_mode = mode_bits(&tree.path);
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        set_mode(&root, 0o700);
        let home = HomeLayout::from_root(&root).expect("layout");
        let mut settings = AppSettings::default();
        settings.appearance.palette = "ocean".to_owned();
        replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("publish");
        set_mode(&root, 0o755);

        let loaded = read_app_settings(&home).expect("settings recover");
        assert_eq!(loaded.appearance.palette, "ocean");
        assert_eq!(mode_bits(&root), 0o700);
        assert_eq!(mode_bits(&tree.path), parent_mode);
    }

    #[test]
    fn empty_permissive_home_reads_defaults_without_creating_a_file() {
        let tree = TempTree::new("empty");
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        set_mode(&root, 0o755);
        let home = HomeLayout::from_root(&root).expect("layout");

        let loaded = read_app_settings(&home).expect("defaults");

        assert_eq!(loaded, AppSettings::default());
        assert_eq!(mode_bits(&root), 0o700);
        assert!(!root.join("settings.json").exists());
    }

    #[test]
    fn read_tightens_each_permissive_owned_ancestor() {
        let tree = TempTree::new("nested");
        let root = tree.path.join("home");
        let sessions = root.join("sessions");
        let session = sessions.join("ses-1");
        fs::create_dir_all(&session).expect("session");
        set_mode(&root, 0o755);
        set_mode(&sessions, 0o775);
        set_mode(&session, 0o751);
        let home = HomeLayout::from_root(&root).expect("layout");

        let value = read_owned_file(&home, "sessions/ses-1/note.json", 64).expect("read");

        assert!(value.is_none());
        assert_eq!(mode_bits(&root), 0o700);
        assert_eq!(mode_bits(&sessions), 0o700);
        assert_eq!(mode_bits(&session), 0o700);
        assert!(!session.join("note.json").exists());
    }

    #[test]
    fn permissive_settings_file_stays_fail_closed_after_the_directory_is_repaired() {
        let tree = TempTree::new("file-mode");
        let root = tree.path.join("home");
        fs::create_dir(&root).expect("home");
        set_mode(&root, 0o700);
        let home = HomeLayout::from_root(&root).expect("layout");
        replace_app_settings(&home, AuthorityRevision::ABSENT, &AppSettings::default())
            .expect("publish");
        let settings_path = root.join("settings.json");
        set_mode(&settings_path, 0o644);
        set_mode(&root, 0o755);

        let error = read_app_settings(&home).expect_err("permissive file");

        assert_eq!(error.kind(), ConfigErrorKind::AccessControl);
        assert_eq!(mode_bits(&root), 0o700);
        assert_eq!(mode_bits(&settings_path), 0o644);
    }

    #[test]
    fn symlink_home_is_rejected_without_tightening_the_target() {
        let tree = TempTree::new("symlink-root");
        let real = tree.path.join("real-home");
        fs::create_dir(&real).expect("target");
        set_mode(&real, 0o755);
        let link = tree.path.join("linked-home");
        symlink(&real, &link).expect("symlink");
        let home = HomeLayout::from_root(&link).expect("layout");

        let error = read_owned_file(&home, "settings.json", 64).expect_err("symlink root");

        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        assert_eq!(mode_bits(&real), 0o755);
    }

    #[test]
    fn symlink_ancestor_is_rejected_without_tightening_the_target() {
        let tree = TempTree::new("symlink-ancestor");
        let root = tree.path.join("home");
        let sessions = root.join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        set_mode(&root, 0o700);
        set_mode(&sessions, 0o755);
        let outside = tree.path.join("outside");
        fs::create_dir(&outside).expect("outside");
        set_mode(&outside, 0o755);
        symlink(&outside, sessions.join("ses-1")).expect("symlink");
        let home = HomeLayout::from_root(&root).expect("layout");

        let error =
            read_owned_file(&home, "sessions/ses-1/note.json", 64).expect_err("intermediate link");

        assert_eq!(error.kind(), ConfigErrorKind::LinkEscape);
        assert_eq!(mode_bits(&outside), 0o755);
        assert_eq!(mode_bits(&sessions), 0o700);
    }

    #[test]
    fn missing_home_read_does_not_create_it() {
        let tree = TempTree::new("missing");
        let root = tree.path.join("home");
        let home = HomeLayout::from_root(&root).expect("layout");

        let value = read_owned_file(&home, "settings.json", 64).expect("missing");

        assert!(value.is_none());
        assert!(!root.exists());
    }
}
