//! Windows self-update: a copy of the running executable applies the staged
//! binary, then relaunches it.
//!
//! The UI process spawns that helper with explicit arguments and exits. The
//! helper waits for the UI pid, copies the staged file onto the install
//! directory (so a download on another volume still lands), renames the old
//! binary to `*.mycode-previous`, and starts the new binary with no updater
//! arguments. A detached `cmd` script is not used: it could copy a backup and
//! then fail to replace a locked image.
//!
//! macOS and Linux keep the shell script in the parent module.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::previous_path;

/// Flag that selects the Windows apply helper.
pub const APPLY_UPDATE_ARG: &str = "--mycode-apply-update";

/// Arguments the helper receives. Paths are separate argv entries, so spaces
/// and `%` stay literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyUpdateArgs {
    /// Installed executable to replace.
    pub target: PathBuf,
    /// Staged, already verified executable.
    pub source: PathBuf,
    /// Expected SHA-256 of `source`, lowercase or mixed-case hex.
    pub sha256: String,
    /// Process id of the UI that must exit before the swap.
    pub wait_pid: u32,
    /// Start `target` after a successful swap.
    pub relaunch: bool,
}

/// Parses a helper invocation.
///
/// `Ok(None)` is a normal launch. `Ok(Some(_))` is the apply helper. `Err`
/// means the apply flag was present but the arguments were incomplete.
pub fn parse_apply_update_args<I, S>(args: I) -> Result<Option<ApplyUpdateArgs>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|value| value.as_ref().to_owned())
        .collect();
    if !args.iter().any(|arg| arg == APPLY_UPDATE_ARG) {
        return Ok(None);
    }
    let mut target = None;
    let mut source = None;
    let mut sha256 = None;
    let mut wait_pid = None;
    let mut relaunch = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--mycode-apply-update" => {}
            "--relaunch" => relaunch = true,
            "--target" | "--source" | "--sha256" | "--wait-pid" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| format!("updater: {arg} needs a value"))?;
                match arg {
                    "--target" => target = Some(PathBuf::from(value)),
                    "--source" => source = Some(PathBuf::from(value)),
                    "--sha256" => sha256 = Some(value.clone()),
                    "--wait-pid" => {
                        wait_pid =
                            Some(value.parse::<u32>().map_err(|_| {
                                "updater: --wait-pid is not a process id".to_owned()
                            })?);
                    }
                    _ => {}
                }
                index += 1;
            }
            other => return Err(format!("updater: unknown argument {other}")),
        }
        index += 1;
    }
    let target = target.ok_or_else(|| "updater: --target is required".to_owned())?;
    let source = source.ok_or_else(|| "updater: --source is required".to_owned())?;
    let sha256 = sha256.ok_or_else(|| "updater: --sha256 is required".to_owned())?;
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("updater: --sha256 must be 64 hex digits".to_owned());
    }
    let wait_pid = wait_pid.ok_or_else(|| "updater: --wait-pid is required".to_owned())?;
    Ok(Some(ApplyUpdateArgs {
        target,
        source,
        sha256,
        wait_pid,
        relaunch,
    }))
}

/// Production callers are Windows-only. Unit tests round-trip the flags on
/// every host.
#[cfg(any(windows, test))]
pub(crate) fn helper_arguments(
    target: &Path,
    source: &Path,
    sha256: &str,
    wait_pid: u32,
    relaunch: bool,
) -> Vec<String> {
    let mut args = vec![
        APPLY_UPDATE_ARG.to_owned(),
        "--target".to_owned(),
        target.to_string_lossy().into_owned(),
        "--source".to_owned(),
        source.to_string_lossy().into_owned(),
        "--sha256".to_owned(),
        sha256.to_owned(),
        "--wait-pid".to_owned(),
        wait_pid.to_string(),
    ];
    if relaunch {
        args.push("--relaunch".to_owned());
    }
    args
}

/// Waits for the UI process, swaps the binary, and optionally relaunches it.
///
/// # Errors
///
/// Returns a failure message when the staged hash is wrong or the swap cannot
/// finish. A busy target is retried. A hash mismatch is not.
pub fn run_apply_update_helper(args: &ApplyUpdateArgs) -> Result<(), String> {
    wait_for_process_exit(args.wait_pid, Duration::from_secs(60));
    let mut last = String::new();
    for _ in 0..30 {
        match replace_verified_executable(&args.source, &args.target, &args.sha256) {
            Ok(()) => {
                if args.relaunch {
                    relaunch_target(&args.target)?;
                }
                return Ok(());
            }
            Err(error) if error.retryable => {
                last = error.message;
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(error) => return Err(error.message),
        }
    }
    Err(if last.is_empty() {
        "could not replace the current binary".to_owned()
    } else {
        last
    })
}

#[cfg(windows)]
pub(crate) fn spawn_windows_helper(
    stage_dir: &Path,
    current: &Path,
    source: &Path,
    sha256: &str,
) -> Result<(), String> {
    let helper = stage_dir.join("mycode-update-helper.exe");
    std::fs::copy(current, &helper).map_err(|error| format!("updater helper: {error}"))?;
    let args = helper_arguments(current, source, sha256, std::process::id(), true);
    spawn_detached(&helper, &args)
}

#[cfg(windows)]
fn spawn_detached(program: &Path, args: &[String]) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;
    use std::process::Command;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let mut command = Command::new(program);
    command.args(args);
    command.creation_flags(
        DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
    );
    match command.spawn() {
        Ok(_) => Ok(()),
        Err(error) if error.raw_os_error() == Some(5) => {
            let mut retry = Command::new(program);
            retry.args(args);
            retry.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
            retry
                .spawn()
                .map(|_| ())
                .map_err(|error| format!("updater spawn: {error}"))
        }
        Err(error) => Err(format!("updater spawn: {error}")),
    }
}

#[derive(Debug)]
struct SwapFailure {
    message: String,
    retryable: bool,
}

fn replace_verified_executable(
    source: &Path,
    target: &Path,
    expected: &str,
) -> Result<(), SwapFailure> {
    let source_hash = super::file_sha256(source).map_err(fatal)?;
    if !source_hash.eq_ignore_ascii_case(expected) {
        return Err(fatal("staged binary sha256 does not match".to_owned()));
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| fatal("target has no directory".to_owned()))?;
    let file_name = target
        .file_name()
        .ok_or_else(|| fatal("target has no file name".to_owned()))?;
    let incoming = parent.join(format!(".{}.mycode-incoming", file_name.to_string_lossy()));
    let backup = previous_path(target);
    std::fs::copy(source, &incoming).map_err(|error| {
        io_failure(
            &error,
            "could not copy the staged binary into the install directory",
        )
    })?;
    let copied_hash = super::file_sha256(&incoming).map_err(fatal)?;
    if !copied_hash.eq_ignore_ascii_case(expected) {
        let _ = std::fs::remove_file(&incoming);
        return Err(fatal("copied binary sha256 does not match".to_owned()));
    }
    if target.exists() {
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
        if let Err(error) = std::fs::rename(target, &backup) {
            let _ = std::fs::remove_file(&incoming);
            return Err(io_failure(&error, "could not back up the current binary"));
        }
    }
    if let Err(error) = std::fs::rename(&incoming, target) {
        let restored = restore_backup(&backup, target);
        return Err(SwapFailure {
            message: if restored {
                format!("could not replace the current binary: {error}")
            } else {
                format!("could not replace the current binary and backup restore failed: {error}")
            },
            retryable: restored && is_retryable(&error),
        });
    }
    let installed = super::file_sha256(target).map_err(fatal)?;
    if !installed.eq_ignore_ascii_case(expected) {
        let restored = restore_backup(&backup, target);
        return Err(fatal(if restored {
            "replaced binary sha256 does not match; restored backup".to_owned()
        } else {
            "replaced binary sha256 does not match; backup restore failed".to_owned()
        }));
    }
    Ok(())
}

fn restore_backup(backup: &Path, target: &Path) -> bool {
    if !backup.exists() {
        return false;
    }
    if target.exists() {
        let _ = std::fs::remove_file(target);
    }
    std::fs::rename(backup, target).is_ok()
}

fn fatal(message: String) -> SwapFailure {
    SwapFailure {
        message,
        retryable: false,
    }
}

fn io_failure(error: &std::io::Error, action: &str) -> SwapFailure {
    SwapFailure {
        message: format!("{action}: {error}"),
        retryable: is_retryable(error),
    }
}

fn is_retryable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::ResourceBusy
    ) || matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

fn wait_for_process_exit(pid: u32, timeout: Duration) {
    if pid == 0 || pid == std::process::id() {
        return;
    }
    let start = std::time::Instant::now();
    while process_is_running(pid) {
        if start.elapsed() > timeout {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // Safety: `kill(pid, 0)` only probes the process and does not signal it.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        let _ = CloseHandle(handle);
        // `GetExitCodeProcess` writes a `u32`. windows-sys 0.61 types
        // `STILL_ACTIVE` as `i32` (NTSTATUS, value 259).
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(any(unix, windows)))]
fn process_is_running(_pid: u32) -> bool {
    false
}

fn relaunch_target(target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    return spawn_detached(target, &[]).map_err(|error| format!("updater relaunch: {error}"));

    #[cfg(not(windows))]
    {
        std::process::Command::new(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("updater relaunch: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{helper_arguments, parse_apply_update_args, replace_verified_executable};
    use std::path::Path;

    #[test]
    fn absent_flag_is_a_normal_launch() {
        let parsed = parse_apply_update_args(["--other"]).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn apply_args_round_trip_paths_with_spaces_and_percent() {
        let target = Path::new(r"C:\Program Files\%SystemRoot%\app.exe");
        let source = Path::new("/tmp/stage/next build.exe");
        let sha = "ab".repeat(32);
        let args = helper_arguments(target, source, &sha, 42, true);
        let parsed = parse_apply_update_args(&args).unwrap().unwrap();
        assert_eq!(parsed.target, target);
        assert_eq!(parsed.source, source);
        assert_eq!(parsed.sha256, sha);
        assert_eq!(parsed.wait_pid, 42);
        assert!(parsed.relaunch);
        assert!(args.iter().any(|arg| arg.contains("%SystemRoot%")));
        assert!(args.iter().any(|arg| arg.contains("Program Files")));
        let quiet = helper_arguments(target, source, &sha, 42, false);
        assert!(!quiet.iter().any(|arg| arg == "--relaunch"));
    }

    #[test]
    fn incomplete_apply_args_fail() {
        let error =
            parse_apply_update_args(["--mycode-apply-update", "--target", "app.exe"]).unwrap_err();
        assert!(error.contains("--source"), "{error}");
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mycode-apply-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replace_keeps_a_backup_and_rejects_a_bad_hash() {
        let dir = scratch("swap");
        let nested = dir.join("Program Files");
        std::fs::create_dir_all(&nested).unwrap();
        let target = nested.join("app.exe");
        let source = dir.join("next.exe");
        std::fs::write(&target, b"old-bytes").unwrap();
        std::fs::write(&source, b"new-bytes").unwrap();
        let hash = super::super::sha256_hex(b"new-bytes");
        replace_verified_executable(&source, &target, &hash).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-bytes");
        let backup = super::super::previous_path(&target);
        assert_eq!(std::fs::read(&backup).unwrap(), b"old-bytes");
        std::fs::write(&source, b"other").unwrap();
        let error = replace_verified_executable(&source, &target, &hash).unwrap_err();
        assert!(!error.retryable);
        assert!(error.message.contains("sha256"), "{}", error.message);
        assert_eq!(std::fs::read(&target).unwrap(), b"new-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn helper_waits_until_the_pid_exits_then_swaps() {
        let dir = scratch("wait");
        let target = dir.join("app");
        let source = dir.join("next");
        std::fs::write(&target, b"old-bytes").unwrap();
        std::fs::write(&source, b"new-bytes").unwrap();
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("read line")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sh");
        let stdin = child.stdin.take();
        let args = super::ApplyUpdateArgs {
            target: target.clone(),
            source,
            sha256: super::super::sha256_hex(b"new-bytes"),
            wait_pid: child.id(),
            relaunch: false,
        };
        let worker = std::thread::spawn(move || super::run_apply_update_helper(&args));
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old-bytes",
            "swap ran while the waited pid was still alive"
        );
        drop(stdin);
        let _ = child.wait();
        worker.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-bytes");
        assert_eq!(
            std::fs::read(super::super::previous_path(&target)).unwrap(),
            b"old-bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
