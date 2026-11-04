//! Child-side Win32 probes: no console window, real pipes, and Job teardown.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::time::Duration;

use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
use windows_sys::Win32::System::Threading::{
    GetStartupInfoW, OpenProcess, PROCESS_SYNCHRONIZE, STARTF_USESHOWWINDOW, STARTUPINFOW,
    WaitForSingleObject,
};

use super::{ShellArgs, ShellMode, ShellTool, Tool, ToolCtx, ToolStream, text_of};
use crate::tools::builtin::exec::{
    PreparedInvocation, RunOutcome, run_prepared, snapshot_child_environment,
};
use crate::tools::builtin::process::{WindowsStdioProcess, acquire_execution_lease};

const PROBE: &str = "console_probe";
const DESCENDANT: &str = "console_descendant";
const STATE_ENV: &str = "MYCODE_TEST_CONSOLE_STATE";
const STDIN_ENV: &str = "MYCODE_TEST_CONSOLE_STDIN";

fn probe_args(name: &str) -> Vec<String> {
    let (_, module) = module_path!().split_once("::").unwrap();
    vec![
        "--exact".to_owned(),
        format!("{module}::{name}"),
        "--ignored".to_owned(),
        "--nocapture".to_owned(),
    ]
}

/// Runs only as a child of the tests below; libtest itself is a console image.
#[test]
#[ignore = "child-side console probe"]
fn console_probe() {
    // SAFETY: these APIs only query this process; the pid buffer is writable.
    let mut pids = [0_u32; 1];
    assert!(unsafe { GetConsoleWindow() }.is_null());
    // CREATE_NO_WINDOW can retain an internal console with no HWND. It must
    // not attach the child to the parent's console.
    let count = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 1) };
    assert!(count <= 1, "child shares a console with {count} processes");
    if count == 1 {
        assert_eq!(pids[0], std::process::id());
    }
    let mut startup = STARTUPINFOW::default();
    // SAFETY: startup is valid writable storage of the documented type.
    unsafe { GetStartupInfoW(&raw mut startup) };
    println!(
        "probe-hidden={}",
        startup.dwFlags & STARTF_USESHOWWINDOW != 0 && startup.wShowWindow == 0
    );
    println!("probe-stdout");
    eprintln!("probe-stderr");
    if std::env::var_os(STDIN_ENV).is_some() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        println!("probe-stdin={}", line.trim());
    }
    std::io::stdout().flush().unwrap();
    if let Some(state) = std::env::var_os(STATE_ENV) {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(probe_args(DESCENDANT))
            .env_remove(STATE_ENV)
            .env_remove(STDIN_ENV)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        std::fs::write(state, child.id().to_string()).unwrap();
        // The parent must end this descendant through the enclosing Job.
        child.wait().unwrap();
    }
}

#[test]
#[ignore = "child-side cancellation probe"]
fn console_descendant() {
    assert!(unsafe { GetConsoleWindow() }.is_null());
    std::thread::sleep(Duration::from_secs(120));
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mycode-console-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn state(&self) -> std::path::PathBuf {
        self.0.join("descendant.pid")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn descendant_handle(scratch: &Scratch) -> OwnedHandle {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(scratch.state()).await
                && let Ok(pid) = text.parse::<u32>()
            {
                // SAFETY: this is the reported child pid. The handle only waits
                // for that process and remains owned across cancellation.
                let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                assert!(
                    !handle.is_null(),
                    "open descendant: {}",
                    std::io::Error::last_os_error()
                );
                return unsafe { OwnedHandle::from_raw_handle(handle) };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("console probe did not report its descendant")
}

fn assert_exited(handle: &OwnedHandle) {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    // SAFETY: this owned process handle supports synchronization.
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
}

#[tokio::test]
async fn program_mode_has_no_console_window_and_preserves_default_gui_show_behavior() {
    let result = ShellTool::with_default_timeout(30)
        .execute(
            ShellArgs {
                mode: ShellMode::Program,
                command: None,
                program: Some(
                    std::env::current_exe()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                ),
                args: probe_args(PROBE),
                timeout_secs: Some(30),
            },
            &ToolCtx::new(std::env::temp_dir()),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!result.is_error, "{text}");
    for marker in ["probe-hidden=false", "probe-stdout", "probe-stderr"] {
        assert!(text.contains(marker), "{text}");
    }
}

#[tokio::test]
async fn stdio_host_has_no_console_window_with_pipes_and_tree_cancellation() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let scratch = Scratch::new();
    let mut env = snapshot_child_environment().unwrap();
    env.push((OsString::from(STATE_ENV), scratch.state().into_os_string()));
    env.push((OsString::from(STDIN_ENV), OsString::from("1")));
    let mut child = WindowsStdioProcess::spawn(
        &std::env::current_exe().unwrap(),
        &probe_args(PROBE),
        None,
        &env,
        true,
        true,
    )
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    stdin.write_all(b"round-trip\n").await.unwrap();
    drop(stdin);
    let descendant = descendant_handle(&scratch).await;
    child.terminate_job().unwrap();
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_exited(&descendant);
    let mut out = String::new();
    let mut err = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        stdout.read_to_string(&mut out).await.unwrap();
        stderr.read_to_string(&mut err).await.unwrap();
    })
    .await
    .unwrap();
    for marker in [
        "probe-hidden=true",
        "probe-stdout",
        "probe-stdin=round-trip",
    ] {
        assert!(out.contains(marker), "{out}");
    }
    assert!(err.contains("probe-stderr"), "{err}");
}

#[tokio::test]
async fn windowless_program_cancellation_and_timeout_reap_the_tree() {
    for cancel_run in [true, false] {
        let scratch = Scratch::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut env = snapshot_child_environment().unwrap();
        env.push((OsString::from(STATE_ENV), scratch.state().into_os_string()));
        let invocation = PreparedInvocation::from_snapshot(
            &std::env::temp_dir(),
            std::env::current_exe().unwrap().to_str().unwrap(),
            &probe_args(PROBE),
            &env,
            &cancel,
        )
        .unwrap();
        let lease = acquire_execution_lease().await;
        let deadline = tokio::time::sleep(Duration::from_secs(if cancel_run { 15 } else { 5 }));
        tokio::pin!(deadline);
        let (outcome, descendant) = tokio::join!(
            run_prepared(invocation, lease, &cancel, &mut deadline),
            async {
                let handle = descendant_handle(&scratch).await;
                if cancel_run {
                    cancel.cancel();
                }
                handle
            },
        );
        match outcome.unwrap() {
            RunOutcome::Cancelled { teardown } if cancel_run => teardown.unwrap(),
            RunOutcome::Timeout {
                stdout,
                stderr,
                teardown,
                started,
                ..
            } if !cancel_run => {
                teardown.unwrap();
                assert!(started);
                assert!(
                    crate::tools::builtin::process::captured_text(&stdout)
                        .0
                        .contains("probe-stdout")
                );
                assert!(
                    crate::tools::builtin::process::captured_text(&stderr)
                        .0
                        .contains("probe-stderr")
                );
            }
            _ => panic!("expected cancellation or timeout"),
        }
        assert_exited(&descendant);
    }
}
