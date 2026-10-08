//! Picks which workspace root an absolute tool path belongs to.
//!
//! Relative paths stay on the session cwd. An absolute path under an extra
//! workspace root is re-anchored to that root so the existing single-root
//! walk still enforces containment. Paths outside every root stay on the
//! session cwd and fail the existing escape check.

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
            relative.to_string_lossy().replace('\\', "/")
        };
        best = Some((depth, extra.clone(), spelled));
    }
    match best {
        Some((_, root, relative)) => (root, relative),
        None => (primary.to_path_buf(), raw.to_owned()),
    }
}
