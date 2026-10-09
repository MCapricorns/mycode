//! Defines the relocatable, lexical MYCode home layout.
//!
//! [`HomeLayout`] resolves an absolute owned root from explicit or process
//! environment values. Resolution and path construction perform no filesystem
//! I/O, never canonicalize or follow links, and do not depend on the current
//! directory.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::{ConfigError, ConfigErrorKind};

/// Names the environment variable that relocates the entire owned home tree.
pub const MYCODE_HOME_ENV: &str = "MYCODE_HOME";

/// Names the lowercase product directory under a user home.
pub const MYCODE_DIR_NAME: &str = ".mycode";
/// Durable session ledgers and compaction checkpoints.
pub const SESSIONS_DIR: &str = "sessions";
/// Tool working directory when no project folder is bound.
pub const SCRATCH_DIR: &str = "scratch";

/// Relative owned path of one file inside a session directory.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::AuthorityValidation`] when `session_id` or
/// `file` is empty or contains a path separator.
pub fn session_relative(session_id: &str, file: &str) -> Result<String, ConfigError> {
    if session_id.is_empty()
        || session_id.contains(['/', '\\', '\0'])
        || file.is_empty()
        || file.contains(['/', '\\', '\0'])
    {
        return Err(ConfigError::authority_rejection());
    }
    Ok(format!("{SESSIONS_DIR}/{session_id}/{file}"))
}

/// Contains caller-supplied values used to resolve the owned home.
#[derive(Debug, Clone, Default)]
pub struct HomeEnv {
    /// Overrides the entire owned home when nonempty.
    pub mycode_home: Option<OsString>,
    /// Supplies the user home when the override is empty or absent.
    pub home: Option<OsString>,
    /// Supplies the Windows-only fallback when `HOME` is empty or absent.
    ///
    /// Non-Windows resolution never consumes this value.
    pub user_profile: Option<OsString>,
}

impl HomeEnv {
    /// Reads home-resolution values from the current process.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            mycode_home: std::env::var_os(MYCODE_HOME_ENV),
            home: std::env::var_os("HOME"),
            user_profile: {
                #[cfg(windows)]
                {
                    std::env::var_os("USERPROFILE")
                }
                #[cfg(not(windows))]
                {
                    None
                }
            },
        }
    }
}

/// Constructs paths in one relocatable MYCode home.
///
/// Every returned path is rooted below [`Self::root`]. The lowercase `.mycode`
/// directory is appended only when resolution uses a user home; `MYCODE_HOME`
/// replaces the root entirely. Path accessor methods construct paths without
/// creating or opening any filesystem object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeLayout {
    root: PathBuf,
}

impl HomeLayout {
    /// Resolves the owned home from explicit environment values.
    ///
    /// Empty values are ignored. A nonempty `MYCODE_HOME` completely replaces
    /// all fallback values. Otherwise `HOME` is used, with `USERPROFILE` as a
    /// Windows-only fallback. An invalid higher-priority value fails closed.
    /// Resolution remains lexical even when the selected path is absent,
    /// inaccessible, or names a link.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::InvalidHome`] when the selected value is not
    /// an absolute, normalized, well-formed owned home.
    pub fn from_env(env: HomeEnv) -> Result<Self, ConfigError> {
        if let Some(root) = nonempty(env.mycode_home) {
            return Self::from_root(root);
        }

        let user_home = nonempty(env.home);
        #[cfg(windows)]
        let user_home = user_home.or_else(|| nonempty(env.user_profile));

        let Some(user_home) = user_home else {
            return Err(ConfigError::new(ConfigErrorKind::InvalidHome));
        };
        let user_home = match normalize_absolute_root(PathBuf::from(user_home)) {
            Ok(path) => path,
            Err(path) => return Err(invalid_home_path(&path)),
        };
        let root = user_home.join(MYCODE_DIR_NAME);
        Ok(Self { root })
    }

    /// Creates a layout from an already-resolved owned root.
    ///
    /// This operation is purely lexical and does not canonicalize or follow
    /// links. The stored path is absolute and separator-normalized.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::InvalidHome`] when `root` is relative, is a
    /// filesystem, drive, or share root, has no normal component, contains a
    /// parent component, or has an unsafe platform root or component.
    pub fn from_root(root: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        match normalize_absolute_root(root.into()) {
            Ok(root)
                if root
                    .components()
                    .any(|component| matches!(component, Component::Normal(_))) =>
            {
                Ok(Self { root })
            }
            Ok(root) | Err(root) => Err(invalid_home_path(&root)),
        }
    }

    /// Returns the owned home root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Joins a controlled relative path below the owned root.
    ///
    /// Every component must be portable across supported platforms. Empty
    /// components, absolute paths, prefixes, traversal, and unsafe Windows
    /// aliases are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::PathEscape`] when any component is unsafe.
    pub fn owned_join(&self, relative: impl AsRef<Path>) -> Result<PathBuf, ConfigError> {
        let Some(relative) = relative.as_ref().to_str() else {
            return Err(ConfigError::new(ConfigErrorKind::PathEscape));
        };
        if relative.is_empty() {
            return Err(ConfigError::new(ConfigErrorKind::PathEscape));
        }

        let mut joined = self.root.clone();
        for component in relative.split(['/', '\\']) {
            validate_path_component(OsStr::new(component))?;
            joined.push(component);
        }
        Ok(joined)
    }
}

fn nonempty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

fn invalid_home_path(path: &Path) -> ConfigError {
    ConfigError::for_path(ConfigErrorKind::InvalidHome, path)
}

fn normalize_absolute_root(root: PathBuf) -> Result<PathBuf, PathBuf> {
    if !root.is_absolute() || has_parent(&root) || !has_well_formed_platform_root(&root) {
        return Err(root);
    }
    Ok(root.components().collect())
}

fn has_parent(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn has_well_formed_platform_root(path: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_drive_unc_or_verbatim_root(path)
    }
    #[cfg(not(windows))]
    {
        matches!(path.components().next(), Some(Component::RootDir))
    }
}

#[cfg(windows)]
fn windows_drive_unc_or_verbatim_root(path: &Path) -> bool {
    use std::path::Prefix;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return false;
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return false;
    }

    let prefix_is_safe = match prefix.kind() {
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) => true,
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            is_safe_path_component(server) && is_safe_path_component(share)
        }
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => false,
    };
    prefix_is_safe
        && components.all(|component| match component {
            Component::Normal(name) => is_safe_path_component(name),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => false,
        })
}

pub(crate) fn is_valid_portable_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= 128
        && first.is_ascii_lowercase()
        && rest
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && is_safe_path_component(OsStr::new(value))
}

/// Reports whether a value is a usable subagent role name.
///
/// Role names reach the filesystem as `<name>.md` and the model as the `agent`
/// tool argument, so they use the portable-id grammar with a shorter bound
/// that keeps routing lines single-line.
#[must_use]
pub(crate) fn is_portable_role_name(value: &str) -> bool {
    value.len() <= 64 && is_valid_portable_id(value)
}

pub(crate) fn validate_path_component(name: &OsStr) -> Result<(), ConfigError> {
    if !is_safe_path_component(name) {
        return Err(ConfigError::new(ConfigErrorKind::PathEscape));
    }
    Ok(())
}

fn is_safe_path_component(name: &OsStr) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    let Some(text) = name.to_str() else {
        return false;
    };
    !text.contains(['/', '\\', '\0', ':', '*', '?', '"', '<', '>', '|'])
        && !text.chars().any(char::is_control)
        && !text.ends_with('.')
        && !text.ends_with(' ')
        && !is_windows_device_name(text)
}

fn is_windows_device_name(name: &str) -> bool {
    // Windows strips trailing dots and spaces before reserved-device matching.
    let stripped = name.trim_end_matches([' ', '.']);
    if stripped.is_empty() {
        return false;
    }
    let basename = stripped.split('.').next().unwrap_or(stripped);
    ["con", "prn", "aux", "nul", "conin$", "conout$", "clock$"]
        .iter()
        .any(|reserved| basename.eq_ignore_ascii_case(reserved))
        || is_com_or_lpt_device(basename)
}

fn is_com_or_lpt_device(basename: &str) -> bool {
    let Some((prefix, unit)) = split_ascii_prefix(basename, 3) else {
        return false;
    };
    (prefix.eq_ignore_ascii_case("com") || prefix.eq_ignore_ascii_case("lpt"))
        && matches_dos_device_unit(unit)
}

fn split_ascii_prefix(value: &str, prefix_len: usize) -> Option<(&str, &str)> {
    (value.len() >= prefix_len && value.is_char_boundary(prefix_len))
        .then(|| value.split_at(prefix_len))
}

fn matches_dos_device_unit(unit: &str) -> bool {
    matches!(unit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        || unit == "\u{00B9}"
        || unit == "\u{00B2}"
        || unit == "\u{00B3}"
}
