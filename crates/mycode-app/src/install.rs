//! Self-install into `<home>/bin`, PATH registration, and download cleanup.
//!
//! Both shipped binaries install the same way: the running executable is
//! copied into `<home>/bin` and that directory is put on the user PATH —
//! the HKCU `Environment` registry value on Windows, shell profile files on
//! macOS — so a refreshed terminal resolves `mycode-cli` without a package
//! manager.
//!
//! The desktop additionally relocates: after the copy it spawns the
//! installed binary and the download exits, so the GUI the user sees always
//! runs from the install dir. The installed copy deletes the download it
//! replaced (the launcher holds the file until it exits, so deletion
//! retries briefly on a background thread) and drops a desktop shortcut.
//! Sweeping `~/Downloads` and `~/Desktop` removes any MYCode binary or
//! release archive left behind, whichever binary launched first.
//!
//! Debug builds and `MYCODE_NO_INSTALL=1` skip the whole dance so
//! `cargo run` stays in `target/`.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use mycode_config::HomeLayout;

/// `MYCODE_NO_INSTALL=1` runs a binary straight from wherever it launched.
const NO_INSTALL_ENV: &str = "MYCODE_NO_INSTALL";
/// Set on the spawned installed copy so it knows which download it
/// replaced; that source is deleted once the launcher exits.
const INSTALLED_FROM_ENV: &str = "MYCODE_INSTALLED_FROM";

/// `mycode-cli` plus the platform executable suffix.
#[must_use]
pub fn cli_exe_name() -> String {
    format!("mycode-cli{}", env::consts::EXE_SUFFIX)
}

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

/// One `ensure_*_installed` outcome, for the notice the CLI shows.
#[derive(Debug, Default)]
pub struct InstallReport {
    /// The installed location inside `<home>/bin`.
    pub target: PathBuf,
    /// The running binary was copied into `target` during this call.
    pub copied: bool,
    /// This process already runs `target`.
    pub running_installed: bool,
    /// A PATH entry for the install dir was added during this call.
    pub path_added: bool,
    /// Human-readable failure detail when the copy or PATH write failed.
    /// The binary keeps running regardless.
    pub warning: Option<String>,
}

/// Whether the running executable already lives at `target`.
fn running_at(target: &Path) -> Option<PathBuf> {
    let exe = env::current_exe().ok()?.canonicalize().ok()?;
    let target_canonical = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    if paths_equal(&exe, &target_canonical) {
        Some(exe)
    } else {
        None
    }
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
    }
    Ok(true)
}

/// Installs the terminal binary into `<home>/bin` and registers that dir on
/// the user PATH. The current process keeps running from its download;
/// PATH makes the next terminal resolve `mycode-cli` to the installed copy.
pub fn ensure_cli_installed(home: &HomeLayout) -> InstallReport {
    let target = install_dir(home).join(cli_exe_name());
    let mut report = InstallReport {
        target: target.clone(),
        ..InstallReport::default()
    };
    if install_skipped() {
        report.running_installed = true;
        return report;
    }
    if running_at(&target).is_some() {
        report.running_installed = true;
        sweep_downloaded_binaries();
        return report;
    }
    let exe = match env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => exe,
        Err(error) => {
            report.warning = Some(format!("current exe unavailable: {error}"));
            return report;
        }
    };
    match install_file(&exe, &target) {
        Ok(copied) => report.copied = copied,
        Err(error) => report.warning = Some(format!("copy to {}: {error}", target.display())),
    }
    match ensure_path_entry(&install_dir(home)) {
        Ok(added) => report.path_added = added,
        Err(error) => {
            report.warning = Some(match report.warning.take() {
                Some(previous) => format!("{previous}; PATH: {error}"),
                None => format!("PATH: {error}"),
            });
        }
    }
    report
}

/// What the desktop main should do after `ensure_desktop_installed`.
pub enum DesktopInstall {
    /// Keep running. `cleanup` is the download this installed copy
    /// replaced; pass it to [`cleanup_installed_source`] on a thread.
    Run { cleanup: Option<PathBuf> },
    /// The installed copy was spawned; exit this process at once.
    Exit,
}

/// The installable unit: the `.app` bundle on macOS when the exe sits
/// inside one, otherwise the executable file itself.
#[cfg(target_os = "macos")]
fn install_unit(exe: &Path, dir: &Path) -> (PathBuf, PathBuf) {
    for ancestor in exe.ancestors() {
        if ancestor.extension().is_some_and(|ext| ext == "app") {
            return (ancestor.to_path_buf(), dir.join("MYCode.app"));
        }
    }
    (exe.to_path_buf(), dir.join(desktop_exe_name()))
}

#[cfg(not(target_os = "macos"))]
fn install_unit(exe: &Path, dir: &Path) -> (PathBuf, PathBuf) {
    (exe.to_path_buf(), dir.join(desktop_exe_name()))
}

/// Whether `exe` is the installed unit itself (a file) or lives inside it
/// (a `.app` bundle).
fn exe_inside_unit(exe: &Path, unit: &Path) -> bool {
    if unit.extension().is_some_and(|ext| ext == "app") {
        exe.starts_with(unit)
    } else {
        paths_equal(exe, unit)
    }
}

fn copy_dir(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let from = entry.path();
        let to = target.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Copies `unit` (file or `.app` dir) onto `target` in place.
fn copy_unit(unit: &Path, target: &Path) -> io::Result<()> {
    if unit.is_dir() {
        let stage = target.with_extension("installing");
        if stage.exists() {
            fs::remove_dir_all(&stage)?;
        }
        copy_dir(unit, &stage)?;
        if target.exists() {
            fs::remove_dir_all(target)?;
        }
        fs::rename(&stage, target)
    } else {
        install_file(unit, target).map(|_| ())
    }
}

/// Installs the desktop binary into `<home>/bin`, spawns the installed
/// copy, and hands off. PATH registration still happens so a terminal can
/// run the GUI by name.
pub fn ensure_desktop_installed(home: &HomeLayout) -> DesktopInstall {
    if install_skipped() {
        return DesktopInstall::Run { cleanup: None };
    }
    let dir = install_dir(home);
    let exe = match env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => exe,
        Err(_) => return DesktopInstall::Run { cleanup: None },
    };
    let (unit, target) = install_unit(&exe, &dir);
    if exe_inside_unit(&exe, &target) {
        sweep_downloaded_binaries();
        let cleanup = env::var_os(INSTALLED_FROM_ENV).map(PathBuf::from);
        if cleanup.is_some() {
            create_desktop_shortcut(&target);
            let _ = ensure_path_entry(&dir);
        }
        return DesktopInstall::Run { cleanup };
    }
    if copy_unit(&unit, &target).is_err() {
        // Stay on the download rather than failing to launch at all.
        return DesktopInstall::Run { cleanup: None };
    }
    let _ = ensure_path_entry(&dir);
    let spawned = spawn_installed(&exe, &unit, &target);
    if spawned {
        DesktopInstall::Exit
    } else {
        DesktopInstall::Run { cleanup: None }
    }
}

/// Launches the installed unit detached, marking it with the download it
/// replaces so the installed copy cleans up and makes the shortcut.
fn spawn_installed(exe: &Path, unit: &Path, target: &Path) -> bool {
    // On macOS an installed `.app` runs via its inner binary; that keeps
    // env propagation trivial and avoids `open`'s indirection.
    let program = if target.extension().is_some_and(|ext| ext == "app") {
        exe.file_name()
            .map(|name| target.join("Contents/MacOS").join(name))
            .unwrap_or_else(|| target.to_path_buf())
    } else {
        target.to_path_buf()
    };
    Command::new(program)
        .env(INSTALLED_FROM_ENV, unit)
        .spawn()
        .is_ok()
}

/// Deletes the download an installed binary replaced. The launcher holds
/// its exe until it exits, so this retries for a few seconds — spawn it on
/// a thread, never block the UI on it.
pub fn cleanup_installed_source(source: PathBuf) {
    if install_skipped() {
        return;
    }
    // Only ever delete under the user home; the marker env is not proof.
    let Some(root) = user_home() else { return };
    if !source.starts_with(&root) {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let removed = if source.is_dir() {
            fs::remove_dir_all(&source)
        } else {
            fs::remove_file(&source)
        };
        match removed {
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
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let _ = fs::remove_dir_all(&path);
            } else {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Whether a loose filename in Downloads/Desktop is one of our release
/// artifacts: the shipped binaries, the `.app`, or `*-v*` archives.
fn is_download_artifact(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let bare = [
        "mycode",
        "mycode.exe",
        "mycode-cli",
        "mycode-cli.exe",
        "mycode-desktop",
        "mycode-desktop.exe",
    ];
    if bare.contains(&lower.as_str()) || name == "MYCode.app" {
        return true;
    }
    let archive = |stem: &str| {
        lower.starts_with(stem)
            && (lower.ends_with(".zip") || lower.ends_with(".dmg") || lower.ends_with(".sha256"))
    };
    archive("mycode-v") || archive("mycode-cli-v") || archive("mycode-desktop-v")
}

/// Ensures `dir` appears in the user's PATH, appending it when absent.
/// Returns whether the PATH was changed this call.
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

#[cfg(not(windows))]
fn path_has_dir(dir: &Path) -> io::Result<bool> {
    // Already exported into this process's PATH by a profile — no write.
    let needle = dir.to_string_lossy();
    if env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|entry| entry.to_string_lossy() == needle))
    {
        return Ok(true);
    }
    for file in profile_files() {
        if file
            .as_ref()
            .and_then(|file| fs::read_to_string(file).ok())
            .is_some_and(|text| profile_mentions(&text, dir))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(windows))]
fn profile_mentions(text: &str, dir: &Path) -> bool {
    let dir = dir.to_string_lossy();
    text.lines().any(|line| line.contains(dir.as_ref()))
}

/// The shell profile files we add the PATH export to. Writing
/// `.bash_profile` unconditionally would shadow a user's `.profile`, so the
/// set is: every existing startup file among the usual suspects, plus
/// `.zprofile` when nothing exists yet (macOS's default login shell).
#[cfg(not(windows))]
fn profile_files() -> Vec<PathBuf> {
    let Some(home) = user_home() else {
        return Vec::new();
    };
    let names = [
        ".zprofile",
        ".zshrc",
        ".bash_profile",
        ".bashrc",
        ".profile",
    ];
    let mut existing: Vec<PathBuf> = names
        .iter()
        .map(|name| home.join(name))
        .filter(|file| file.exists())
        .collect();
    if existing.is_empty() {
        existing.push(home.join(".zprofile"));
        existing.push(home.join(".profile"));
    }
    existing
}

#[cfg(not(windows))]
fn add_path_dir(dir: &Path) -> io::Result<()> {
    let line = format!(
        "export PATH={}:\"$PATH\"",
        shell_quote(&dir.to_string_lossy())
    );
    for file in profile_files() {
        let existing = fs::read_to_string(&file).unwrap_or_default();
        if profile_mentions(&existing, dir) {
            continue;
        }
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("\n# Added by MYCode — the installed binaries live here.\n");
        content.push_str(&line);
        content.push('\n');
        fs::write(&file, content)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Writes a desktop shortcut for the installed unit. Best-effort: a failed
/// shortcut must not block the install.
#[cfg(windows)]
fn create_desktop_shortcut(target: &Path) {
    if target.extension().is_some_and(|ext| ext == "app") {
        return;
    }
    let exe = target.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$desktop=[Environment]::GetFolderPath('Desktop');\
         $ws=New-Object -ComObject WScript.Shell;\
         $sc=$ws.CreateShortcut((Join-Path $desktop 'MYCode.lnk'));\
         $sc.TargetPath='{exe}';$sc.WorkingDirectory=[IO.Path]::GetDirectoryName('{exe}');\
         $sc.IconLocation='{exe},0';$sc.Save()"
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .spawn()
        .and_then(|mut child| child.wait());
}

/// Finder alias `MYCode` on the desktop pointing at the installed `.app`.
/// No shortcut is made for a bare downloaded binary.
#[cfg(target_os = "macos")]
fn create_desktop_shortcut(target: &Path) {
    if !target.extension().is_some_and(|ext| ext == "app") {
        return;
    }
    let Some(home) = user_home() else { return };
    let desktop = home.join("Desktop");
    for stale in [desktop.join("MYCode"), desktop.join("MYCode alias")] {
        if stale.exists() {
            let _ = fs::remove_file(&stale);
        }
    }
    let app = target
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let desk = desktop
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let _ = Command::new("osascript")
        .args([
            "-e",
            &format!(
                "tell application \"Finder\" to make new alias file at POSIX file \"{desk}\" to POSIX file \"{app}\""
            ),
            "-e",
            "tell application \"Finder\" to set name of result to \"MYCode\"",
        ])
        .spawn()
        .and_then(|mut child| child.wait());
}

#[cfg(not(any(windows, target_os = "macos")))]
fn create_desktop_shortcut(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names_match_release_layout() {
        for name in [
            "mycode",
            "mycode.exe",
            "mycode-cli",
            "mycode-cli.exe",
            "mycode-desktop",
            "mycode-desktop.exe",
            "MYCode.app",
            "mycode-v0.11.32-x86_64-pc-windows-msvc.zip",
            "mycode-cli-v0.12.0-aarch64-apple-darwin.zip",
            "mycode-desktop-v0.12.0-aarch64-apple-darwin.dmg",
            "mycode-cli-v0.12.0-x86_64-pc-windows-msvc.zip.sha256",
        ] {
            assert!(is_download_artifact(name), "{name}");
        }
        for name in [
            "mycode-notes.txt",
            "mycode-cli.zip",
            "MYCODE-CLI.EXE.bak",
            "some-app.app",
            "other.zip",
        ] {
            assert!(!is_download_artifact(name), "{name}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn shell_quoting_round_trips_special_chars() {
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("/tmp/o'my"), "'/tmp/o'\\''my'");
    }
}
