//! Bare Windows program names, resolved in `PATHEXT` order.
//!
//! `CreateProcessW` only appends `.exe`. `npx` and `uvx` are installed as
//! `npx.cmd` / `uvx.exe`, so a bare name has to try the launchable extensions
//! itself. The order is the `PATHEXT` order of `.com`, `.exe`, `.bat`, and
//! `.cmd`. Extensions this process cannot execute directly (`.vbs`, `.js`)
//! are ignored. Any of the four that `PATHEXT` omits is still tried, after
//! the ones it lists, so a short `PATHEXT` cannot hide `npx.cmd`.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Windows default for the four extensions this launcher can start.
pub(crate) const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

const LAUNCHABLE: [&str; 4] = [".com", ".exe", ".bat", ".cmd"];

/// Launchable extensions in `PATHEXT` order.
///
/// An empty value uses [`.com`, `.exe`, `.bat`, `.cmd`]. A listed extension
/// keeps its position. Missing launchable extensions follow, in that same
/// default order.
pub(crate) fn pathext_extensions(pathext: &str) -> Vec<String> {
    let source = if pathext.trim().is_empty() {
        DEFAULT_PATHEXT
    } else {
        pathext
    };
    let mut listed = Vec::new();
    for ext in source.split(';') {
        let Some(ext) = normalize_extension(ext) else {
            continue;
        };
        if !listed.iter().any(|item: &String| item == &ext) {
            listed.push(ext);
        }
    }
    for ext in LAUNCHABLE {
        if !listed.iter().any(|item| item == ext) {
            listed.push(ext.to_owned());
        }
    }
    listed
}

/// File names tried for a bare `name`, in [`pathext_extensions`] order.
///
/// A name that already contains `.` is used as written (`npx.cmd` stays
/// `npx.cmd`). `npx` with the default order is `npx.com`, `npx.exe`,
/// `npx.bat`, `npx.cmd`.
fn bare_program_candidates(name: &str, pathext: &str) -> Vec<String> {
    if name.contains('.') {
        return vec![name.to_owned()];
    }
    pathext_extensions(pathext)
        .into_iter()
        .map(|ext| format!("{name}{ext}"))
        .collect()
}

/// First existing file for `command` on `path_var`.
///
/// Each directory is searched for every candidate before the next directory,
/// which is how `cmd.exe` walks `PATHEXT`. `None` means no candidate was a
/// file.
#[must_use]
pub fn first_program_on_path(command: &str, path_var: &OsStr, pathext: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let names = bare_program_candidates(command, pathext);
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in &names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn normalize_extension(ext: &str) -> Option<String> {
    let ext = ext.trim();
    if ext.is_empty() {
        return None;
    }
    let ext = if let Some(rest) = ext.strip_prefix('.') {
        format!(".{}", rest.to_ascii_lowercase())
    } else {
        format!(".{}", ext.to_ascii_lowercase())
    };
    LAUNCHABLE.contains(&ext.as_str()).then_some(ext)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{bare_program_candidates, first_program_on_path};

    static SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("mycode-pathext-{label}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"").expect("touch");
    }

    #[test]
    fn bare_npx_follows_com_exe_bat_cmd() {
        assert_eq!(
            bare_program_candidates("npx", ""),
            vec!["npx.com", "npx.exe", "npx.bat", "npx.cmd"]
        );
        assert_eq!(
            bare_program_candidates("npx", ".COM;.EXE;.BAT;.CMD;.VBS;.JS"),
            vec!["npx.com", "npx.exe", "npx.bat", "npx.cmd"]
        );
        assert_eq!(
            bare_program_candidates("uvx", "COM;EXE;BAT;CMD"),
            vec!["uvx.com", "uvx.exe", "uvx.bat", "uvx.cmd"]
        );
    }

    #[test]
    fn pathext_order_is_preserved_and_cmd_is_not_dropped() {
        assert_eq!(
            bare_program_candidates("npx", ".CMD;.EXE"),
            vec!["npx.cmd", "npx.exe", "npx.com", "npx.bat"]
        );
        assert_eq!(
            bare_program_candidates("npx.cmd", ".COM;.EXE;.BAT;.CMD"),
            vec!["npx.cmd"]
        );
    }

    #[test]
    fn bare_npx_resolves_to_npx_cmd_when_that_is_the_file() {
        let scratch = Scratch::new("cmd");
        touch(scratch.path(), "npx.cmd");
        let found = first_program_on_path("npx", scratch.path().as_os_str(), "").expect("npx.cmd");
        assert_eq!(found, scratch.path().join("npx.cmd"));
    }

    #[test]
    fn pathext_order_prefers_exe_over_cmd_and_an_earlier_directory() {
        let scratch = Scratch::new("order");
        let earlier = scratch.path().join("earlier");
        let later = scratch.path().join("later");
        fs::create_dir_all(&earlier).expect("earlier");
        fs::create_dir_all(&later).expect("later");
        touch(&earlier, "npx.cmd");
        touch(&later, "npx.com");
        touch(&later, "npx.exe");
        let path = std::env::join_paths([&earlier, &later]).expect("PATH");
        let found = first_program_on_path("npx", &path, ".COM;.EXE;.BAT;.CMD").expect("hit");
        assert_eq!(found, earlier.join("npx.cmd"));

        touch(&earlier, "npx.exe");
        let found = first_program_on_path("npx", &path, ".COM;.EXE;.BAT;.CMD").expect("exe");
        assert_eq!(found, earlier.join("npx.exe"));

        let cmd_first = Scratch::new("cmd-first");
        touch(cmd_first.path(), "npx.exe");
        touch(cmd_first.path(), "npx.cmd");
        let found = first_program_on_path("npx", cmd_first.path().as_os_str(), ".CMD;.EXE")
            .expect("cmd before exe");
        assert_eq!(found, cmd_first.path().join("npx.cmd"));
    }
}
