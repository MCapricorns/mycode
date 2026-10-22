//! One desktop per home.
//!
//! The session store keeps one writer per process. Two desktops on one home
//! could open the same chat, and each would cut the JSONL bytes the other
//! committed off as a torn tail on its next append. The desktop is the only
//! writer of stored sessions, so it alone holds an exclusive lock on
//! `<home>/mycode-desktop.instance` for its whole life. The OS releases the
//! lock however the process ends, so a crash never leaves the home locked.
//!
//! A second launch raises the running window (Windows) and exits. A launch
//! that races a desktop on its way out — the installed copy a download hands
//! off to, or the relaunch an update swap starts — waits for it to leave.

use std::fs::{File, OpenOptions, TryLockError};
use std::time::{Duration, Instant};

use mycode_config::HomeLayout;

/// Lock file in the home root, beside `mycode-desktop.shortcut`.
const LOCK_FILE: &str = "mycode-desktop.instance";
/// How long a launch waits for a desktop that is exiting. Its shutdown
/// joins the core thread, which first ends running turns and MCP servers.
const EXIT_GRACE: Duration = Duration::from_secs(10);
/// Pause between lock attempts while waiting.
const RETRY_EVERY: Duration = Duration::from_millis(100);

/// Exclusive ownership of one home, released on drop or process exit.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// What [`claim`] decided for this launch.
#[derive(Debug)]
pub enum Claim {
    /// Start the desktop. `None` when the lock file could not be opened or
    /// locked for any reason other than another owner: refusing to open
    /// would be worse than running unguarded.
    Run(Option<InstanceLock>),
    /// Another desktop owns this home; its window was raised if it had one.
    Running,
}

/// Claims `home` for this desktop process.
///
/// Call it before the launch steps: they clear update files that a running
/// desktop may still be using.
#[must_use]
pub fn claim(home: &HomeLayout) -> Claim {
    claim_within(home, EXIT_GRACE, raise_running_window)
}

fn claim_within(home: &HomeLayout, grace: Duration, raise: impl Fn() -> bool) -> Claim {
    let Some(file) = open_lock_file(home) else {
        return Claim::Run(None);
    };
    let deadline = Instant::now() + grace;
    loop {
        match file.try_lock() {
            Ok(()) => return Claim::Run(Some(InstanceLock { _file: file })),
            Err(TryLockError::Error(_)) => return Claim::Run(None),
            Err(TryLockError::WouldBlock) => {}
        }
        // A live owner has a window to raise. One still starting opens it
        // during the wait; one leaving has already closed it.
        if raise() || Instant::now() >= deadline {
            return Claim::Running;
        }
        std::thread::sleep(RETRY_EVERY);
    }
}

fn open_lock_file(home: &HomeLayout) -> Option<File> {
    let path = home.owned_join(LOCK_FILE).ok()?;
    // A first launch finds no home yet. The config layer tightens the
    // root's permissions when it first opens it, as it does for the root
    // the session index creates.
    std::fs::create_dir_all(home.root()).ok()?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).ok()
}

/// Restores and focuses the main window of another running desktop.
/// `false` when none is open: that desktop is still starting or leaving.
#[cfg(windows)]
fn raise_running_window() -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    use windows_sys::core::BOOL;

    struct Search {
        own_pid: u32,
        found: HWND,
    }

    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Search` handed to EnumWindows below, which
        // outlives the enumeration and is not touched elsewhere meanwhile.
        let search = unsafe { &mut *(data as *mut Search) };
        // SAFETY: `window` is a top-level handle EnumWindows just produced;
        // these calls take no pointers.
        let main_window =
            unsafe { IsWindowVisible(window) != 0 && GetWindow(window, GW_OWNER).is_null() };
        if !main_window {
            return 1;
        }
        let mut pid = 0_u32;
        // SAFETY: `pid` is a live u32 the call writes.
        unsafe { GetWindowThreadProcessId(window, &raw mut pid) };
        if pid == 0 || pid == search.own_pid || !is_desktop_process(pid) {
            return 1;
        }
        search.found = window;
        0
    }

    let mut search = Search {
        own_pid: std::process::id(),
        found: std::ptr::null_mut(),
    };
    // SAFETY: `visit` has the WNDENUMPROC signature and only dereferences
    // `search`, which lives across this synchronous call.
    unsafe { EnumWindows(Some(visit), (&raw mut search) as LPARAM) };
    if search.found.is_null() {
        return false;
    }
    // SAFETY: window-state calls on a handle EnumWindows produced; a window
    // that closed meanwhile makes them fail without effect.
    unsafe {
        if IsIconic(search.found) != 0 {
            ShowWindow(search.found, SW_RESTORE);
        }
        SetForegroundWindow(search.found);
    }
    true
}

/// Whether `pid` runs a MYCode desktop binary (`mycode-desktop*.exe`): the
/// installed copy, or a download started before it was installed.
#[cfg(windows)]
fn is_desktop_process(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    // SAFETY: no pointers; a null handle means the process is gone or
    // denies the query.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let mut buffer = [0_u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` u16s; the call writes at most that many
    // and stores the written length in `len`.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            buffer.as_mut_ptr(),
            &raw mut len,
        )
    };
    // SAFETY: `process` is live and closed exactly once.
    unsafe { CloseHandle(process) };
    if queried == 0 {
        return false;
    }
    let path = String::from_utf16_lossy(&buffer[..(len as usize).min(buffer.len())]);
    let name = path
        .rsplit('\\')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.starts_with("mycode-desktop") && name.ends_with(".exe")
}

/// macOS hands a Finder or Dock launch to the running app itself; a binary
/// started from a terminal has no window of its own to raise.
#[cfg(not(windows))]
fn raise_running_window() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mycode_config::HomeLayout;

    use super::{Claim, claim_within};

    fn scratch(label: &str) -> (std::path::PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-instance-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let home = HomeLayout::from_root(&root).expect("home");
        (root, home)
    }

    #[test]
    fn a_second_desktop_reports_the_running_one() {
        let (root, home) = scratch("second");
        let Claim::Run(Some(first)) = claim_within(&home, Duration::ZERO, || false) else {
            panic!("the first claim must own a fresh home");
        };
        assert!(matches!(
            claim_within(&home, Duration::from_millis(250), || false),
            Claim::Running
        ));
        // A raised window ends the wait at once.
        let started = std::time::Instant::now();
        assert!(matches!(
            claim_within(&home, Duration::from_secs(30), || true),
            Claim::Running
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(first);
        assert!(matches!(
            claim_within(&home, Duration::ZERO, || false),
            Claim::Run(Some(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_launch_takes_over_from_a_desktop_that_exits_within_the_grace() {
        let (root, home) = scratch("handoff");
        let Claim::Run(Some(leaving)) = claim_within(&home, Duration::ZERO, || false) else {
            panic!("the first claim must own a fresh home");
        };
        let exit = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(leaving);
        });
        assert!(matches!(
            claim_within(&home, Duration::from_secs(10), || false),
            Claim::Run(Some(_))
        ));
        exit.join().expect("exit thread");
        let _ = std::fs::remove_dir_all(root);
    }
}
