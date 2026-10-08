//! Lexical path helpers and report path rendering.
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::tool::ToolError;

/// Normalizes the session cwd to an absolute lexical path.
///
/// Relative `cwd` is made absolute against the process cwd; an already
/// absolute `cwd` does not consult the process cwd. A result that is not
/// absolute or still contains `..` is rejected fail-closed.
pub(crate) fn normalize_session_cwd(cwd: &Path) -> Result<PathBuf, ToolError> {
    let cwd = strip_verbatim_prefix(cwd);
    let absolute = if cwd.is_absolute() {
        lexical_normalize(&cwd)
    } else {
        let process_cwd = std::env::current_dir().map_err(|error| {
            ToolError::Execution(format!("process cwd is not accessible: {error}"))
        })?;
        lexical_normalize(&process_cwd.join(&cwd))
    };
    if !absolute.is_absolute()
        || absolute
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ToolError::InvalidArgs(format!(
            "session cwd is not an absolute resolvable path: {}",
            cwd.display()
        )));
    }
    Ok(absolute)
}

/// Lexically resolves `.` and `..` without accessing the filesystem.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                _ => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Returns whether `candidate` is component-wise inside `root`.
///
/// Handle-proven paths use this exact comparison. User-typed aliases use
/// `strip_prefix_lexical` (Unicode-aware on Windows, never a string prefix).
#[cfg(windows)]
pub(crate) fn is_within(root: &Path, candidate: &Path) -> bool {
    components_within(root, candidate, |a: &OsStr, b: &OsStr| a == b)
}

/// Lexical containment is `strip_prefix_lexical` succeeding.
/// Windows user-path equality: NT ordinal case-insensitive UTF-16.
///
/// Each UTF-16 code unit is mapped with `RtlUpcaseUnicodeChar` and
/// compared. That is a 1:1 NT upcase table lookup, not Unicode full
/// lowercase and not `to_string_lossy`. Full lowercase expands `İ` to
/// `i\u{307}`; lossy UTF-16 collapses unpaired surrogates to U+FFFD.
/// Unix keeps byte-exact names.
pub(crate) fn os_str_eq_lexical(left: &OsStr, right: &OsStr) -> bool {
    #[cfg(windows)]
    {
        windows_os_str_eq_ignore_case(left, right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// NT ordinal case-insensitive compare of two OS strings as UTF-16.
#[cfg(windows)]
pub(crate) fn windows_os_str_eq_ignore_case(left: &OsStr, right: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Wdk::System::SystemServices::RtlUpcaseUnicodeChar;

    let left: Vec<u16> = left.encode_wide().collect();
    let right: Vec<u16> = right.encode_wide().collect();
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(&right).all(|(left_unit, right_unit)| {
        // SAFETY: `RtlUpcaseUnicodeChar` is a pure 1:1 mapping on a
        // UTF-16 code unit and has no pointer preconditions.
        unsafe { RtlUpcaseUnicodeChar(*left_unit) == RtlUpcaseUnicodeChar(*right_unit) }
    })
}

/// Component-kind-safe equality used by lexical containment and extraction.
pub(crate) fn lexical_components_equal(left: &Component<'_>, right: &Component<'_>) -> bool {
    match (*left, *right) {
        (Component::Prefix(_), Component::Prefix(_))
        | (Component::Normal(_), Component::Normal(_)) => {
            os_str_eq_lexical(left.as_os_str(), right.as_os_str())
        }
        (Component::RootDir, Component::RootDir)
        | (Component::CurDir, Component::CurDir)
        | (Component::ParentDir, Component::ParentDir) => true,
        _ => false,
    }
}

#[cfg(windows)]
pub(crate) fn components_within(
    root: &Path,
    candidate: &Path,
    eq: impl Fn(&OsStr, &OsStr) -> bool,
) -> bool {
    let root: Vec<_> = root.components().collect();
    let candidate: Vec<_> = candidate.components().collect();
    candidate.len() >= root.len()
        && root
            .iter()
            .zip(candidate.iter())
            .all(|(left, right)| eq(left.as_os_str(), right.as_os_str()))
}

/// Returns the relative suffix of `candidate` under `root`, if contained.
///
/// Prefix, root-directory, and name components must each match as the same
/// kind. A leftover `Prefix` or `RootDir` (for example `C:` vs `C:\foo`)
/// is not a relative path and fails closed. Windows name and prefix text
/// uses `os_str_eq_lexical`.
pub(crate) fn strip_prefix_lexical(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let root: Vec<_> = root.components().collect();
    let candidate: Vec<_> = candidate.components().collect();
    if candidate.len() < root.len() {
        return None;
    }
    if !root
        .iter()
        .zip(candidate.iter())
        .all(|(left, right)| lexical_components_equal(left, right))
    {
        return None;
    }
    let suffix = &candidate[root.len()..];
    if suffix
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(suffix.iter().collect())
}

/// Converts Windows verbatim paths to stable plain forms.
///
/// Verbatim disks become `C:\...`, verbatim UNC paths preserve both the
/// server and share as `\\server\share\...`, and generic verbatim
/// prefixes such as `\\?\Volume{GUID}\...` become the absolute device
/// namespace `\\.\Volume{GUID}\...`. Device namespace and already
/// plain paths are unchanged.
pub(crate) fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::path::Prefix;

        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return path.to_path_buf();
        };
        match prefix.kind() {
            Prefix::VerbatimDisk(letter) => {
                let mut out = PathBuf::from(format!(r"{}:\", letter as char));
                push_non_root_components(&mut out, components);
                out
            }
            Prefix::VerbatimUNC(server, share) => {
                let mut out = PathBuf::from(r"\\");
                out.push(server);
                out.push(share);
                push_non_root_components(&mut out, components);
                out
            }
            Prefix::Verbatim(name) => {
                // Keep generic verbatim paths absolute. A bare
                // `Volume{GUID}\...` is relative, and `resolve_search_root`
                // would join it onto the process cwd.
                let mut raw = OsString::from(r"\\.\");
                raw.push(name);
                let mut out = PathBuf::from(raw);
                push_non_root_components(&mut out, components);
                out
            }
            Prefix::Disk(_) | Prefix::UNC(_, _) | Prefix::DeviceNS(_) => path.to_path_buf(),
        }
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(windows)]
pub(crate) fn push_non_root_components<'a>(
    out: &mut PathBuf,
    components: impl Iterator<Item = Component<'a>>,
) {
    for component in components {
        if !matches!(component, Component::RootDir) {
            out.push(component.as_os_str());
        }
    }
}

/// Converts absolute DOS and UNC paths to Win32 extended-length forms.
#[cfg(windows)]
pub(crate) fn windows_extended_length_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::path::Prefix;

    if !path.is_absolute() {
        return path.to_path_buf();
    }
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    match prefix.kind() {
        Prefix::Disk(_) => {
            let mut extended = OsString::from(r"\\?\");
            extended.push(path.as_os_str());
            PathBuf::from(extended)
        }
        Prefix::UNC(server, share) => {
            let mut authority = OsString::from(r"\\?\UNC\");
            authority.push(server);
            authority.push(r"\");
            authority.push(share);
            let mut extended = PathBuf::from(authority);
            push_non_root_components(&mut extended, components);
            extended
        }
        Prefix::Verbatim(_)
        | Prefix::VerbatimDisk(_)
        | Prefix::VerbatimUNC(_, _)
        | Prefix::DeviceNS(_) => path.to_path_buf(),
    }
}

pub(crate) fn posix_relative_key(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        to_posix(relative)
    }
}

pub(crate) fn resolve_relative_argument(
    root: &Path,
    alias_root: Option<&Path>,
    raw: &str,
) -> Result<PathBuf, ()> {
    let argument = strip_verbatim_prefix(Path::new(raw));
    if argument.is_absolute() {
        let normalized = lexical_normalize(&argument);
        // One predicate: containment and relative extraction cannot diverge.
        // `alias_root` is the session-given spelling of the tree that `root`
        // proves by handle: Windows 8.3 aliases (`RUNNER~1` vs
        // `runneradmin`) can differ from the on-disk long name, and an
        // argument echoing the session spelling stays contained. The walk
        // still opens component-by-component from the retained handle, so
        // permissiveness here cannot escape the anchored tree.
        strip_prefix_lexical(root, &normalized)
            .or_else(|| alias_root.and_then(|alias| strip_prefix_lexical(alias, &normalized)))
            .ok_or(())
    } else {
        normalize_anchored_relative(&argument)
    }
}

pub(crate) fn normalize_anchored_relative(path: &Path) -> Result<PathBuf, ()> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                if !out.pop() {
                    return Err(());
                }
            }
            Component::Prefix(_) | Component::RootDir => return Err(()),
        }
    }
    Ok(out)
}

/// Returns a forward-slash relative path, or the full path for a root file.
pub(crate) fn rel_posix(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative.as_os_str().is_empty() {
        to_posix(path)
    } else {
        to_posix(relative)
    }
}

/// Lossy display of one path component, matching frontier and top-N keys.
pub(crate) fn lossy_component(name: &OsStr) -> std::borrow::Cow<'_, str> {
    name.to_string_lossy()
}

/// Shared path order: lossy rendering first, original `OsString` for ties.
///
/// Directory listings, the walk frontier, and grep/find top-N heaps share
/// this key so two names that render identically stay deterministic.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct PathOrderKey {
    rendered: String,
    raw: OsString,
}

impl PathOrderKey {
    /// Builds a key from a relative or absolute path.
    pub(crate) fn from_path(path: &Path) -> Self {
        Self {
            rendered: to_posix(path),
            raw: path.as_os_str().to_os_string(),
        }
    }

    /// Builds a key when the display spelling is already known.
    pub(crate) fn from_rendered_and_raw(rendered: String, raw: impl Into<OsString>) -> Self {
        Self {
            rendered,
            raw: raw.into(),
        }
    }

    /// Lossy `/`-separated spelling used in reports.
    pub(crate) fn rendered(&self) -> &str {
        &self.rendered
    }

    /// Heap bytes charged for this key: rendered text plus the raw OS name.
    pub(crate) fn store_bytes(&self) -> usize {
        self.rendered
            .len()
            .saturating_add(self.raw.as_encoded_bytes().len())
    }
}

/// Renders a path with `/` separators on every platform.
///
/// Each component uses [`lossy_component`] so listing sort, frontier peek,
/// and reported top-N keys stay on one order.
pub(crate) fn to_posix(path: &Path) -> String {
    let path = strip_verbatim_prefix(path);
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                out.push_str(&lossy_component(prefix.as_os_str()).replace('\\', "/"));
            }
            Component::RootDir => {
                if out.is_empty() {
                    out.push('/');
                }
            }
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&lossy_component(other.as_os_str()));
            }
        }
    }
    out
}

/// Truncates bytes for display without splitting a UTF-8 character.
pub(crate) fn display_line(bytes: &[u8], cap: usize, truncated: &mut bool) -> String {
    let mut end = bytes.len().min(cap);
    while end > 0 && end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    if end < bytes.len() {
        *truncated = true;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}
