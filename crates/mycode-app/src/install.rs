//! Self-install into `<home>/bin`, PATH registration, and download cleanup.
//!
//! The desktop installs itself on Windows only (macOS ships a dmg the user
//! drags to `/Applications`). Launched from anywhere but `<home>/bin`, it
//! copies itself there, spawns the installed copy with the same arguments,
//! and exits. The installed copy deletes the old exe it replaced (the
//! launcher holds the file until it exits, so deletion retries for up to 30
//! seconds on a background thread) and drops a desktop shortcut — written
//! once and recorded under `<home>`, so upgrades repair a shortcut the
//! install never dropped while a user-deleted one stays gone. Sweeping
//! `~/Downloads` and `~/Desktop` removes loose MYCode binaries and release
//! archives.
//!
//! Debug builds and `MYCODE_NO_INSTALL=1` skip the whole dance so
//! `cargo run` stays in `target/`.

use std::env;
use std::fs;
#[cfg(windows)]
use std::io;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
use std::time::{Duration, Instant};

use mycode_agent::HomeLayout;

/// `MYCODE_NO_INSTALL=1` runs a binary straight from wherever it launched.
const NO_INSTALL_ENV: &str = "MYCODE_NO_INSTALL";
/// Set on the spawned installed copy so it knows which download it
/// replaced; that source is deleted once the launcher exits.
#[cfg(windows)]
const INSTALLED_FROM_ENV: &str = "MYCODE_INSTALLED_FROM";

/// `mycode-desktop` plus the platform executable suffix.
#[must_use]
pub fn desktop_exe_name() -> String {
    format!("mycode-desktop{}", env::consts::EXE_SUFFIX)
}

/// `<home>/bin` — the user-owned install location placed on PATH.
#[must_use]
pub fn install_dir(home: &HomeLayout) -> PathBuf {
    home.root().join("bin")
}

/// Whether this process opted out of installing (debug or explicit env).
#[must_use]
pub fn install_skipped() -> bool {
    cfg!(debug_assertions) || env::var_os(NO_INSTALL_ENV).is_some()
}

#[cfg(windows)]
fn paths_equal(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
fn paths_equal(a: &Path, b: &Path) -> bool {
    a == b
}

/// Copies `exe` onto `target` via a sibling temp name, so a target held by
/// another running copy fails only the rename, never the read.
#[cfg(windows)]
fn install_file(exe: &Path, target: &Path) -> io::Result<bool> {
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let stage = target.with_extension("installing");
    fs::copy(exe, &stage)?;
    match fs::rename(&stage, target) {
        Ok(()) => {}
        Err(error) => {
            let _ = fs::remove_file(&stage);
            // A running installed copy locks its own exe on Windows; an
            // up-to-date copy in place is not a failure for this run.
            if !target.exists() {
                return Err(error);
            }
            return Ok(false);
        }
    }
    Ok(true)
}

/// What the desktop main should do after `ensure_desktop_installed`.
pub enum DesktopInstall {
    /// Keep running. `cleanup` is the download this installed copy
    /// replaced; pass it to [`cleanup_installed_source`] on a thread.
    Run { cleanup: Option<PathBuf> },
    /// The installed copy was spawned; exit this process at once.
    Exit,
}

/// Whether the canonical `exe` is `target`. `target` is canonicalized
/// too: on Windows a canonical path carries the `\\?\` prefix, and
/// comparing it against the lexical home path never matches.
#[cfg(any(windows, test))]
fn is_installed_exe(exe: &Path, target: &Path) -> bool {
    paths_equal(exe, &canonical(target))
}

#[cfg(any(windows, test))]
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `path` names a MYCode desktop executable the installer may
/// delete after relocating: a plain file called `mycode-desktop*.exe`.
/// The marker env is inherited by child processes, so it alone is never
/// proof that a path is ours.
fn is_replaced_desktop_exe(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    path.is_file() && name.starts_with("mycode-desktop") && name.ends_with(".exe")
}

/// Installs the desktop binary into `<home>/bin`, spawns the installed
/// copy, and hands off. PATH registration still happens so a terminal can
/// run the GUI by name. Windows only: the macOS app ships as a dmg the
/// user drags to `/Applications`, and updates in place from there.
#[cfg(not(windows))]
pub fn ensure_desktop_installed(_home: &HomeLayout) -> DesktopInstall {
    DesktopInstall::Run { cleanup: None }
}

/// See the non-Windows twin; this is the real install.
#[cfg(windows)]
pub fn ensure_desktop_installed(home: &HomeLayout) -> DesktopInstall {
    if install_skipped() {
        return DesktopInstall::Run { cleanup: None };
    }
    let dir = install_dir(home);
    let exe = match env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => exe,
        Err(_) => return DesktopInstall::Run { cleanup: None },
    };
    let target = dir.join(desktop_exe_name());
    if is_installed_exe(&exe, &target) {
        sweep_downloaded_binaries();
        // Child processes inherit the marker; once the source is gone a
        // later restart must not spawn the cleanup thread for it again.
        let cleanup = env::var_os(INSTALLED_FROM_ENV)
            .map(PathBuf::from)
            .filter(|source| source.exists());
        // The shortcut is written once, then recorded. A missing record —
        // an install predating the shortcut, or an upgrade that never ran
        // the download hand-off — still gets one; a record that outlives
        // the .lnk means the user deleted it on purpose, so it is not
        // recreated.
        if cleanup.is_some() || !shortcut_record_path(home).exists() {
            if create_desktop_shortcut(&target) {
                let _ = fs::write(
                    shortcut_record_path(home),
                    target.as_os_str().as_encoded_bytes(),
                );
            }
            let _ = ensure_path_entry(&dir);
        }
        return DesktopInstall::Run { cleanup };
    }
    // A spawned copy that still does not see itself installed must not
    // spawn again, or every launch would hand off forever.
    if env::var_os(INSTALLED_FROM_ENV).is_some() {
        return DesktopInstall::Run { cleanup: None };
    }
    if install_file(&exe, &target).is_err() {
        // Stay on the download rather than failing to launch at all.
        return DesktopInstall::Run { cleanup: None };
    }
    let _ = ensure_path_entry(&dir);
    // The installed copy learns which download it replaced, so it can
    // delete it and make the shortcut once this process has exited.
    let spawned = Command::new(&target)
        .args(env::args_os().skip(1))
        .env(INSTALLED_FROM_ENV, &exe)
        .spawn()
        .is_ok();
    if spawned {
        DesktopInstall::Exit
    } else {
        DesktopInstall::Run { cleanup: None }
    }
}

/// Deletes the old executable an installed copy replaced. The launcher
/// holds its exe until it exits, so this retries for up to 30 seconds — spawn
/// it on a thread, never block the UI on it.
pub fn cleanup_installed_source(source: PathBuf) {
    if install_skipped() || !is_replaced_desktop_exe(&source) {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs::remove_file(&source) {
            Ok(()) => return,
            Err(_) if !source.exists() => return,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => return,
        }
    }
}

fn user_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty() && home.is_absolute())
}

/// Deletes MYCode binaries and release archives sitting loose in
/// `~/Downloads` or `~/Desktop`. Runs only from an installed copy; silent
/// on errors — leftovers are cosmetic.
pub fn sweep_downloaded_binaries() {
    if install_skipped() {
        return;
    }
    let Some(home) = user_home() else { return };
    let exe = env::current_exe().and_then(|exe| exe.canonicalize()).ok();
    for dir in [home.join("Downloads"), home.join("Desktop")] {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !is_download_artifact(name) {
                continue;
            }
            let path = entry.path();
            if exe.as_ref().is_some_and(|exe| {
                path.canonicalize()
                    .is_ok_and(|canonical| paths_equal(&canonical, exe))
            }) {
                continue;
            }
            // Files only: a folder named `mycode` is far more likely a
            // checkout than a download.
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Whether a loose filename in Downloads/Desktop is one of our release
/// artifacts: the shipped binary, a `*-v*` archive, or its `.sha256`
/// sidecar. The binary name matches exactly, and only on Windows, where the
/// desktop installs itself; on macOS the user may run it from a download or
/// keep the dmg, so those files stay.
fn is_download_artifact(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !cfg!(windows) {
        return false;
    }
    if name == "mycode-desktop.exe" {
        return true;
    }
    lower.starts_with("mycode-desktop-v")
        && (lower.ends_with(".zip") || lower.ends_with(".dmg") || lower.ends_with(".sha256"))
}

/// Ensures `dir` appears in the user's PATH, appending it when absent.
/// Returns whether the PATH was changed this call.
#[cfg(windows)]
fn ensure_path_entry(dir: &Path) -> io::Result<bool> {
    if path_has_dir(dir)? {
        return Ok(false);
    }
    add_path_dir(dir).map(|()| true)
}

#[cfg(windows)]
fn path_has_dir(dir: &Path) -> io::Result<bool> {
    let normalized = dir.to_string_lossy().replace('/', "\\");
    let normalized = normalized.trim_end_matches('\\').to_owned();
    Ok(read_user_path()?.is_some_and(|value| {
        value.split(';').any(|entry| {
            entry
                .trim()
                .trim_matches('"')
                .replace('/', "\\")
                .trim_end_matches('\\')
                .eq_ignore_ascii_case(&normalized)
        })
    }))
}

#[cfg(windows)]
fn read_user_path() -> io::Result<Option<String>> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Environment", KEY_READ)?;
    match key.get_raw_value("Path") {
        Ok(raw) => Ok(Some(raw.to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn add_path_dir(dir: &Path) -> io::Result<()> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
    let dir = dir.to_string_lossy().replace('/', "\\");
    let (merged, raw_type) = match key.get_raw_value("Path") {
        Ok(raw) => {
            let existing = raw.to_string();
            if existing.trim().is_empty() {
                (dir, raw.vtype)
            } else {
                (
                    format!("{};{dir}", existing.trim_end_matches(';')),
                    raw.vtype,
                )
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (dir, winreg::enums::RegType::REG_EXPAND_SZ)
        }
        Err(error) => return Err(error),
    };
    let raw = winreg::RegValue {
        bytes: merged
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect(),
        vtype: raw_type,
    };
    key.set_raw_value("Path", &raw)?;
    broadcast_environment_change();
    Ok(())
}

/// Tells Explorer and friends the environment changed so windows opened
/// afterwards inherit the new PATH without a sign-out.
#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let message: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            message.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result as *mut usize,
        );
    }
}

/// `<home>/mycode-desktop.shortcut` records that the desktop shortcut was
/// written once. Missing means this install predates the shortcut or its
/// creation failed, so a later launch still writes one; present means a
/// missing .lnk was removed on purpose and is not recreated.
#[cfg(windows)]
fn shortcut_record_path(home: &HomeLayout) -> PathBuf {
    home.root().join("mycode-desktop.shortcut")
}

/// Writes a desktop shortcut for the installed desktop binary.
/// Best-effort: a failed shortcut must not block the install. Returns
/// whether the shortcut script ran to completion; only then is the
/// shortcut recorded, so a failure is retried on the next launch.
#[cfg(windows)]
fn create_desktop_shortcut(target: &Path) -> bool {
    let exe = target.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$desktop=[Environment]::GetFolderPath('Desktop');\
         $ws=New-Object -ComObject WScript.Shell;\
         $sc=$ws.CreateShortcut((Join-Path $desktop 'MYCode.lnk'));\
         $sc.TargetPath='{exe}';$sc.WorkingDirectory=[IO.Path]::GetDirectoryName('{exe}');\
         $sc.IconLocation='{exe},0';$sc.Save()"
    );
    use std::os::windows::process::CommandExt as _;
    // The desktop is a windowed app; without this a console flashes up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .and_then(|mut child| child.wait())
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names_match_release_layout() {
        // The desktop installs itself only on Windows; elsewhere the user
        // may run it from a download or keep the dmg.
        for name in [
            "mycode-desktop.exe",
            "mycode-desktop-v0.12.0-x86_64-pc-windows-msvc.zip",
            "mycode-desktop-v0.12.0-aarch64-apple-darwin.zip.sha256",
        ] {
            assert_eq!(is_download_artifact(name), cfg!(windows), "{name}");
        }
        for name in [
            "mycode-notes.txt",
            "mycode-desktop.zip",
            "MYCODE-DESKTOP.EXE.bak",
            "MYCode.app",
            "other.zip",
        ] {
            assert!(!is_download_artifact(name), "{name}");
        }
    }

    #[test]
    fn only_a_desktop_exe_is_deleted_after_relocation() {
        let dir = env::temp_dir().join(format!("mycode-replaced-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let old = dir.join("mycode-desktop-old.EXE");
        let other = dir.join("notes.exe");
        fs::write(&old, b"bin").expect("old");
        fs::write(&other, b"bin").expect("other");
        assert!(is_replaced_desktop_exe(&old));
        // An inherited marker pointing at anything else is ignored.
        assert!(!is_replaced_desktop_exe(&other));
        assert!(!is_replaced_desktop_exe(&dir));
        assert!(!is_replaced_desktop_exe(&dir.join("mycode-desktop.exe")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_canonical_exe_matches_its_lexical_install_path() {
        // `current_exe().canonicalize()` is compared against the lexical
        // `<home>/bin` target; on Windows only the canonical form carries
        // `\\?\`, so a plain comparison never saw the installed copy.
        let dir = env::temp_dir().join(format!("mycode-install-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let target = dir.join(desktop_exe_name());
        fs::write(&target, b"bin").expect("file");
        let exe = target.canonicalize().expect("canonical");
        assert!(is_installed_exe(&exe, &target));
        assert!(!is_installed_exe(&exe, &dir.join("other")));
        let _ = fs::remove_dir_all(&dir);
    }
}
