//! Proves script mode returns when the shell exits, and that cancel or
//! timeout kills descendants that left the process group.
#![cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::builtin::shell::{DetectedShell, ShellArgs, ShellKind, ShellMode, ShellTool};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError};

fn shell() -> ShellTool {
    ShellTool::forcing(DetectedShell {
        kind: ShellKind::Bash,
        program: PathBuf::from("/bin/bash"),
    })
}

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mycode-shell-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn process_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return true;
    };
    let state = rest.split_whitespace().next().unwrap_or("");
    state != "Z"
}

fn read_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

fn kill_pids(pids: &[u32]) {
    for pid in pids {
        // SAFETY: the pid came from a child this test started. SIGKILL only
        // requests termination. ESRCH means it is already gone.
        unsafe {
            libc::kill(*pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn background_child_does_not_hold_the_tool_open() {
    let dir = scratch_dir("bg");
    let pid_path = dir.join("pid");
    let command = format!("sleep 30 & echo $! > '{}'; echo bg", pid_path.display());
    let started = Instant::now();
    let result = shell()
        .execute(
            ShellArgs {
                mode: ShellMode::Script,
                command: Some(command),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(8),
            },
            &ToolCtx::new(&dir),
            &mut ToolStream::closed(),
        )
        .await
        .expect("background command should finish with the shell");
    let elapsed = started.elapsed();
    let pid = read_pid(&pid_path);
    if let Some(pid) = pid {
        kill_pids(&[pid]);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        elapsed < Duration::from_secs(4),
        "tool waited {elapsed:?} for a background sleep"
    );
    assert!(!result.is_error, "{:?}", result.content);
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_core::message::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(text.contains("bg"), "missing shell output: {text:?}");
    assert!(pid.is_some(), "background pid was not recorded");
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_kills_setsid_and_reparented_children() {
    let dir = scratch_dir("cancel");
    let command = format!(
        "sleep 300 & echo $! > '{dir}/sleep300'; \
         (sleep 302 & echo $! > '{dir}/sleep302'); \
         setsid sleep 303 </dev/null >/dev/null 2>&1 & echo $! > '{dir}/sleep303'; \
         wait",
        dir = dir.display()
    );
    let cancel = CancellationToken::new();
    let ctx = ToolCtx::new(&dir).with_cancel(cancel.clone());
    let tool = shell();
    let run = tokio::spawn(async move {
        tool.execute(
            ShellArgs {
                mode: ShellMode::Script,
                command: Some(command),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(30),
            },
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    let ready = Instant::now();
    let pids = loop {
        let pids = ["sleep300", "sleep302", "sleep303"].map(|name| read_pid(&dir.join(name)));
        if pids.iter().all(Option::is_some) {
            break pids.map(Option::unwrap);
        }
        assert!(
            ready.elapsed() < Duration::from_secs(8),
            "shell did not record child pids"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let error = run
        .await
        .expect("cancel task")
        .expect_err("cancel is a tool error");
    let message = match error {
        ToolError::Execution(message) => message,
        other => panic!("expected an execution error, got {other:?}"),
    };
    assert!(
        message.contains("cancelled"),
        "cancel error should name the cancellation: {message}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let survivors: Vec<_> = pids.into_iter().filter(|pid| process_alive(*pid)).collect();
    kill_pids(&pids);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        survivors.is_empty(),
        "descendants still alive after cancel: {survivors:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_kills_setsid_and_reparented_children() {
    let dir = scratch_dir("timeout");
    let command = format!(
        "sleep 300 & echo $! > '{dir}/sleep300'; \
         (sleep 302 & echo $! > '{dir}/sleep302'); \
         setsid sleep 303 </dev/null >/dev/null 2>&1 & echo $! > '{dir}/sleep303'; \
         wait",
        dir = dir.display()
    );
    let started = Instant::now();
    let result = shell()
        .execute(
            ShellArgs {
                mode: ShellMode::Script,
                command: Some(command),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(2),
            },
            &ToolCtx::new(&dir),
            &mut ToolStream::closed(),
        )
        .await
        .expect("timeout is a tool result");
    let elapsed = started.elapsed();
    let pids = ["sleep300", "sleep302", "sleep303"]
        .map(|name| read_pid(&dir.join(name)))
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let survivors: Vec<_> = pids
        .iter()
        .copied()
        .filter(|pid| process_alive(*pid))
        .collect();
    kill_pids(&pids);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(elapsed < Duration::from_secs(8), "timeout took {elapsed:?}");
    assert!(result.is_error, "a timed-out command is an error result");
    let details = result.details.expect("timeout details");
    assert_eq!(details["timed_out"], true, "{details}");
    assert_eq!(pids.len(), 3, "expected three child pids, got {pids:?}");
    assert!(
        survivors.is_empty(),
        "descendants still alive after timeout: {survivors:?}"
    );
}
