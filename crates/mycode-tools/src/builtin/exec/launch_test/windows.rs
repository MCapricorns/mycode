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
use crate::builtin::exec::{
    PreparedInvocation, RunOutcome, run_prepared, snapshot_child_environment,
};
use crate::builtin::process::{WindowsStdioProcess, acquire_execution_lease};

const PROBE: &str = "builtin::exec::launch_test::windows::console_probe";
const DESCENDANT: &str = "builtin::exec::launch_test::windows::console_descendant";
const STATE_ENV: &str = "MYCODE_TEST_CONSOLE_STATE";
const STDIN_ENV: &str = "MYCODE_TEST_CONSOLE_STDIN";

fn probe_args(name: &str) -> Vec<String> {
    ["--exact", name, "--ignored", "--nocapture"]
        .map(str::to_owned)
        .to_vec()
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
                    crate::builtin::process::captured_text(&stdout)
                        .0
                        .contains("probe-stdout")
                );
                assert!(
                    crate::builtin::process::captured_text(&stderr)
                        .0
                        .contains("probe-stderr")
                );
            }
            _ => panic!("expected cancellation or timeout"),
        }
        assert_exited(&descendant);
    }
}

#[tokio::test]
async fn powershell_script_hides_later_console_allocation_and_keeps_native_output() {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let shell = crate::builtin::shell::DetectedShell {
        kind: crate::builtin::shell::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from(root)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    };
    let scratch = Scratch::new();
    let phase_log = scratch.0.join("powershell-phases.log");
    let script = r#"
$ErrorActionPreference = 'Stop'
[IO.File]::AppendAllText($phaseLog, "after-prelude ps=$($PSVersionTable.PSVersion) clr=$([Environment]::Version) pointer=$([IntPtr]::Size) home=$PSHOME`n")
Write-Output 'probe-script-stdout'
[IO.File]::AppendAllText($phaseLog, "after-first-pipeline`n")
# Define P/Invokes in memory: this launch probe must not depend on Add-Type's
# external C# compiler and its cold-start cost on Windows PowerShell 5.1.
$assembly = [AppDomain]::CurrentDomain.DefineDynamicAssembly(
    [Reflection.AssemblyName]::new('ConsoleProbe'),
    [Reflection.Emit.AssemblyBuilderAccess]::Run)
$module = $assembly.DefineDynamicModule('ConsoleProbe')
$type = $module.DefineType('ConsoleProbe', [Reflection.TypeAttributes]'Public, Abstract, Sealed')
foreach ($entry in @(
    @('GetStartupInfoW', 'kernel32.dll', [void], [type[]]@([IntPtr])),
    @('GetConsoleWindow', 'kernel32.dll', [IntPtr], [type[]]@()),
    @('FreeConsole', 'kernel32.dll', [bool], [type[]]@()),
    @('AllocConsole', 'kernel32.dll', [bool], [type[]]@()),
    @('IsWindowVisible', 'user32.dll', [bool], [type[]]@([IntPtr]))
)) {
    $method = $type.DefinePInvokeMethod($entry[0], $entry[1],
        [Reflection.MethodAttributes]'Public, Static, PinvokeImpl',
        [Reflection.CallingConventions]::Standard, $entry[2], $entry[3],
        [Runtime.InteropServices.CallingConvention]::Winapi,
        [Runtime.InteropServices.CharSet]::Unicode)
    $method.SetImplementationFlags([Reflection.MethodImplAttributes]::PreserveSig)
}
$probe = $type.CreateType()
# STARTUPINFOW is at most 104 bytes; its flags follow cb, alignment, three
# pointers and seven DWORDs. wShowWindow immediately follows the flags.
$startup = [Runtime.InteropServices.Marshal]::AllocHGlobal(104)
try {
    $probe::GetStartupInfoW($startup)
    $flagsOffset = 28 + 4 * [IntPtr]::Size
    $flags = [Runtime.InteropServices.Marshal]::ReadInt32($startup, $flagsOffset)
    $show = [Runtime.InteropServices.Marshal]::ReadInt16($startup, $flagsOffset + 4)
    if (($flags -band 1) -eq 0 -or $show -ne 0) { throw 'missing hidden startup' }
} finally {
    [Runtime.InteropServices.Marshal]::FreeHGlobal($startup)
}
Write-Output 'probe-pinvoke-ready'
[IO.File]::AppendAllText($phaseLog, "after-pinvoke`n")
Write-Error 'probe-script-stderr' -ErrorAction Continue
[IO.File]::AppendAllText($phaseLog, "before-native-command`n")
$native = & "$env:SystemRoot\System32\whoami.exe"
[IO.File]::AppendAllText($phaseLog, "after-native-command exit=$LASTEXITCODE`n")
if ($LASTEXITCODE -ne 0 -or -not $native) { throw 'native command failed or produced no output' }
Write-Output "probe-native-output=$native"
# Exercise a later allocation after native IO: FreeConsole invalidates PowerShell's cached console handles.
[IO.File]::AppendAllText($phaseLog, "before-free-console`n")
$probe::FreeConsole() | Out-Null
[IO.File]::AppendAllText($phaseLog, "before-alloc-console`n")
if (-not $probe::AllocConsole()) { throw 'console allocation failed' }
[IO.File]::AppendAllText($phaseLog, "after-alloc-console`n")
if ($probe::GetConsoleWindow() -eq [IntPtr]::Zero) { throw 'allocated console has no window' }
if ($probe::IsWindowVisible($probe::GetConsoleWindow())) { throw 'visible allocated console' }
Write-Output 'probe-native-hidden'
[IO.File]::AppendAllText($phaseLog, "completed`n")
"#;
    // Parameter defaults run before the production UTF-8 prelude. A file
    // journal distinguishes prelude and first-pipeline stalls from pipe IO.
    let script = format!(
        "param($phaseLog = '{}', $entry = [IO.File]::AppendAllText($phaseLog, \"entered-before-prelude`n\"))\n{script}",
        phase_log.to_str().unwrap().replace('\'', "''"),
    );
    let started = std::time::Instant::now();
    let result = ShellTool::forcing(shell.clone())
        .execute(
            ShellArgs {
                mode: ShellMode::Script,
                command: Some(script),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(30),
            },
            &ToolCtx::new(std::env::temp_dir()),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    let text = text_of(&result);
    let elapsed_ms = started.elapsed().as_millis();
    let phases =
        std::fs::read_to_string(&phase_log).unwrap_or_else(|err| format!("no journal: {err}"));
    let diagnostics = if result.is_error {
        powershell_startup_controls(&shell.program, &scratch).await
    } else {
        String::new()
    };
    assert!(
        !result.is_error,
        "{text}\nPowerShell wall_ms={elapsed_ms}; phases:\n{phases}\n{diagnostics}"
    );
    for marker in [
        "probe-script-stdout",
        "probe-script-stderr",
        "probe-native-output=",
        "probe-native-hidden",
    ] {
        assert!(text.contains(marker), "{text}");
    }
}

/// These controls only run after a failed probe; they never turn its failure
/// into a pass. Both bypass the encoding prelude and avoid cmdlet auto-import.
/// The stdio control observes startup up to the production default budget;
/// the subsequent pinned control distinguishes its launch policy on a warm host.
async fn powershell_startup_controls(program: &std::path::Path, scratch: &Scratch) -> String {
    let env = match snapshot_child_environment() {
        Ok(env) => env,
        Err(err) => return format!("startup controls: environment snapshot failed: {err}"),
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut report = format!(
        "startup controls: host={} parent_job={:?} program={}\n",
        std::env::consts::ARCH,
        crate::builtin::process::current_process_is_in_job(),
        program.display(),
    );
    for pinned in [false, true] {
        let started = std::time::Instant::now();
        let name = if pinned { "pinned" } else { "stdio" };
        let journal = scratch.0.join(format!("{name}-startup.log"));
        let script = format!(
            "[IO.File]::WriteAllText('{}', \"entered ps=$($PSVersionTable.PSVersion) clr=$([Environment]::Version) pointer=$([IntPtr]::Size)`n\"); [Console]::Out.WriteLine('probe-startup-control'); [Console]::Out.Flush()",
            journal.to_str().unwrap().replace('\'', "''"),
        );
        let encoded = match crate::builtin::shell::encode_powershell_command(&script, program) {
            Ok(encoded) => encoded,
            Err(err) => {
                report.push_str(&format!("{name}: argument encoding failed: {err}\n"));
                continue;
            }
        };
        let mut args = [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(encoded);
        let outcome = if pinned {
            let lease = acquire_execution_lease().await;
            report.push_str(&format!(
                "pinned lease_wait_ms={}\n",
                started.elapsed().as_millis()
            ));
            match PreparedInvocation::from_snapshot(
                &scratch.0,
                program.to_str().unwrap(),
                &args,
                &env,
                &cancel,
            ) {
                Ok(invocation) => {
                    let deadline = tokio::time::sleep(Duration::from_secs(10));
                    tokio::pin!(deadline);
                    match run_prepared(
                        invocation.with_hidden_window(),
                        lease,
                        &cancel,
                        &mut deadline,
                    )
                    .await
                    {
                        Ok(RunOutcome::Done {
                            status,
                            stdout,
                            stderr,
                            ..
                        }) => startup_control_output(format!("exit={status}"), &stdout, &stderr),
                        Ok(RunOutcome::Timeout {
                            started,
                            teardown,
                            stdout,
                            stderr,
                            ..
                        }) => startup_control_output(
                            format!("timed out started={started} teardown={teardown:?}"),
                            &stdout,
                            &stderr,
                        ),
                        Ok(RunOutcome::Cancelled { teardown }) => {
                            format!("cancelled: {teardown:?}")
                        }
                        Ok(RunOutcome::CollectFailed { error, teardown }) => {
                            format!("collection failed: {error}; teardown={teardown:?}")
                        }
                        Err(err) => format!("launch failed: {err}"),
                    }
                }
                Err(err) => format!("preparation failed: {err}"),
            }
        } else {
            stdio_startup_control(program, &args, &env, scratch).await
        };
        let phases =
            std::fs::read_to_string(journal).unwrap_or_else(|err| format!("no journal: {err}"));
        report.push_str(&format!(
            "{name} wall_ms={}: {outcome}\njournal: {phases}\n",
            started.elapsed().as_millis(),
        ));
    }
    report
}

fn startup_control_output(
    status: String,
    stdout: &crate::builtin::process::CapturedStream,
    stderr: &crate::builtin::process::CapturedStream,
) -> String {
    format!(
        "{status}; stdout={:?}; stderr={:?}",
        crate::builtin::process::captured_text(stdout).0,
        crate::builtin::process::captured_text(stderr).0,
    )
}

async fn stdio_startup_control(
    program: &std::path::Path,
    args: &[String],
    env: &[(OsString, OsString)],
    scratch: &Scratch,
) -> String {
    use crate::builtin::process::{CapturedStream, collect_until_exit};

    let mut child =
        match WindowsStdioProcess::spawn(program, args, Some(&scratch.0), env, false, true) {
            Ok(child) => child,
            Err(err) => return format!("launch failed: {err}"),
        };
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let mut stdout = CapturedStream::new();
    let mut stderr = CapturedStream::new();
    let status = tokio::time::timeout(
        Duration::from_secs(crate::builtin::shell::DEFAULT_TIMEOUT_SECS),
        collect_until_exit(
            &mut stdout_pipe,
            &mut stderr_pipe,
            &mut stdout,
            &mut stderr,
            child.wait(),
        ),
    )
    .await;
    let termination = child.terminate_job();
    let reaped = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    startup_control_output(
        format!("status={status:?} termination={termination:?} reap={reaped:?}"),
        &stdout,
        &stderr,
    )
}
