//! Picks which workspace root an absolute tool path belongs to.
//!
//! Relative paths stay on the session cwd. An absolute path under an extra
//! workspace root is re-anchored to that root so the existing single-root
//! walk still enforces containment. When several extra roots match, the
//! deepest prefix wins; equal depths keep the earlier root. Paths outside
//! every root stay on the session cwd and fail the existing escape check.

use std::path::{Path, PathBuf};

use crate::builtin::fs_search::{lexical_normalize, strip_prefix_lexical, strip_verbatim_prefix};

/// Returns the directory a tool path should be prepared against, and the
/// path spelling to pass into that prepare call.
///
/// Extra-root hits return a relative spelling (empty when the argument is
/// the root itself). Every other argument is returned unchanged so the
/// session-cwd prepare path keeps its current errors.
#[must_use]
pub fn anchor_tool_path(primary: &Path, extras: &[PathBuf], raw: &str) -> (PathBuf, String) {
    let argument = Path::new(raw);
    if !argument.is_absolute() {
        return (primary.to_path_buf(), raw.to_owned());
    }
    let normalized = lexical_normalize(&strip_verbatim_prefix(argument));
    let mut best: Option<(usize, PathBuf, String)> = None;
    for extra in extras {
        if extra.as_os_str().is_empty() {
            continue;
        }
        let extra_norm = lexical_normalize(&strip_verbatim_prefix(extra));
        let Some(relative) = strip_prefix_lexical(&extra_norm, &normalized) else {
            continue;
        };
        let depth = extra_norm.components().count();
        if best
            .as_ref()
            .is_some_and(|(best_depth, _, _)| *best_depth >= depth)
        {
            continue;
        }
        let spelled = if relative.as_os_str().is_empty() {
            String::new()
        } else {
            crate::builtin::fs_search::to_posix(&relative)
        };
        best = Some((depth, extra.clone(), spelled));
    }
    match best {
        Some((_, root, relative)) => (root, relative),
        None => (primary.to_path_buf(), raw.to_owned()),
    }
}

/// The OS temp directory in its resolved long spelling.
///
/// `TEMP` can hold an 8.3 short form (`C:\Users\LONGNA~1\...`) while the
/// model writes the long form it sees elsewhere; the anchor match above is
/// lexical, so the root is resolved once here. A failed resolve keeps the
/// raw value.
#[must_use]
pub fn os_scratch_root() -> PathBuf {
    let temp = std::env::temp_dir();
    std::fs::canonicalize(&temp)
        .map(|resolved| strip_verbatim_prefix(&resolved))
        .unwrap_or(temp)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::anchor_tool_path;

    #[test]
    fn anchor_tool_path_selects_deepest_extra_root_and_normalizes_separators() {
        // Use a native absolute root: `/workspace` alone is drive-relative
        // on Windows and cannot exercise the absolute-path anchor contract.
        let base = std::env::temp_dir().join("mycode-anchor-root");
        let primary = base.join("main");
        let extra = base.join("extra");
        let nested = extra.join("nested");
        let extras = vec![extra, nested.clone()];
        let raw = nested
            .join(Path::new("dir/../file.rs"))
            .display()
            .to_string();
        let (root, rel) = anchor_tool_path(&primary, &extras, &raw);
        assert_eq!(root, nested);
        assert_eq!(rel, "file.rs");

        let (root_exact, rel_exact) =
            anchor_tool_path(&primary, &extras, &nested.display().to_string());
        assert_eq!(root_exact, nested);
        assert_eq!(rel_exact, "");

        let unmatched = base.join("outside/file.rs").display().to_string();
        let (unmatched_root, unmatched_rel) = anchor_tool_path(&primary, &extras, &unmatched);
        assert_eq!(unmatched_root, primary);
        assert_eq!(unmatched_rel, unmatched);
    }
}
