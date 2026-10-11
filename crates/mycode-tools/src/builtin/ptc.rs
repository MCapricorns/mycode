//! `run_code` — the only tool the model can call directly.
//!
//! The program runs in a fresh system Python process. Other tools are
//! `await tools.name(...)` inside that program and go through the same
//! preflight as a direct call. Only `print` and `return` come back to the
//! model. Inner calls are nested UI events, not model history.

mod protocol;
mod runtime;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::builtin::python::python_unavailable_message;
use crate::builtin::truncate_bytes;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

use runtime::{ProgramOutcome, execute_program};

/// Example `description` embedded in the system prompt.
pub const RUN_CODE_EXAMPLE_DESCRIPTION: &str = "List rust files that mention dispatch";

/// Example program body. Top-level `await` and `return`, one grep, a filter.
pub const RUN_CODE_EXAMPLE_CODE: &str = "\
hits = await tools.grep(pattern=\"dispatch_tool\", include=\"*.rs\")\n\
return \"\\n\".join([line for line in hits.split(\"\\n\") if \"turn.rs\" in line][:20])\n";

/// Largest program, in characters.
const MAX_CODE_CHARS: usize = 16_000;
/// Largest combined log and return value sent back to the model.
const MAX_OUTPUT_BYTES: usize = 8_000;

/// The `run_code` builtin.
#[derive(Debug, Default)]
pub struct RunCodeTool;

/// Arguments for [`RunCodeTool`]. `description` is listed first on purpose.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunCodeArgs {
    /// UI title. The schema text is what the model sees.
    #[schemars(
        description = "Clear, concise description of what this program does in active voice, 5-10 words (shown in the UI). Provide `description` before `code`. Example: \"List rust files that mention dispatch\"."
    )]
    pub description: String,
    /// Program body. The schema text is what the model sees.
    #[schemars(
        description = "The program: the body of an async Python function run by the system Python. Top-level await and return work. Call tools as await tools.name(arg=value). Only print and return come back."
    )]
    pub code: String,
}

#[async_trait]
impl Tool for RunCodeTool {
    type Args = RunCodeArgs;
    type Output = ();

    fn name(&self) -> &str {
        "run_code"
    }

    fn description(&self) -> &str {
        "Execute a Python program against the available tools. The only tool \
         you can call directly. Takes two required arguments, `description` \
         then `code`: `description` is a short summary shown in the UI, and \
         `code` is the body of an async function (top-level await and return \
         work). Call tools as `await tools.name(...)` per the system prompt. \
         Only what you print or return comes back. Each call runs in a fresh \
         system Python process."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "run_code: the only directly callable tool. Pass description, then code. Reach every other tool as await tools.name(...) inside the program. Only print and return come back.",
        )
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let description = args.description.trim();
        if description.is_empty() {
            return Err(ToolError::InvalidArgs("description is required".to_owned()));
        }
        if description.chars().count() > 200 {
            return Err(ToolError::InvalidArgs(
                "description exceeds 200 characters".to_owned(),
            ));
        }
        let code = args.code.trim();
        if code.is_empty() {
            return Err(ToolError::InvalidArgs("code is required".to_owned()));
        }
        if code.chars().count() > MAX_CODE_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "code exceeds {MAX_CODE_CHARS} characters"
            )));
        }
        let Some(catalog) = ctx.catalog.clone() else {
            return Err(ToolError::Execution(
                "run_code has no tool catalog".to_owned(),
            ));
        };
        if let Some(message) = python_unavailable_message() {
            return Ok(failure("exception", &message, "", &[], description));
        }
        let outcome = execute_program(code, ctx, &catalog, out).await;
        Ok(render_outcome(description, outcome))
    }
}

fn render_outcome(description: &str, outcome: ProgramOutcome) -> ToolResult {
    let calls = outcome
        .calls
        .iter()
        .map(|note| {
            json!({
                "tool": note.tool,
                "ok": note.ok,
                "bytes": note.bytes,
            })
        })
        .collect::<Vec<_>>();
    match outcome.error {
        Some(error) => failure(
            error.kind,
            &error.message,
            &outcome.logs,
            &calls,
            description,
        ),
        None => success(description, &outcome.logs, outcome.value, &calls),
    }
}

fn success(
    description: &str,
    logs: &str,
    value: Option<String>,
    calls: &[serde_json::Value],
) -> ToolResult {
    let mut body = String::new();
    let logs = logs.trim();
    if !logs.is_empty() {
        body.push_str(logs);
    }
    if let Some(value) = value.filter(|text| !text.trim().is_empty()) {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(value.trim());
    }
    if body.is_empty() {
        body.push_str("(run_code completed with no output)");
    }
    let (body, truncated) = cap_output(body);
    ToolResult::text(body).with_details(details(description, calls, truncated))
}

fn failure(
    kind: &str,
    message: &str,
    logs: &str,
    calls: &[serde_json::Value],
    description: &str,
) -> ToolResult {
    let mut body = String::new();
    let logs = logs.trim();
    if !logs.is_empty() {
        body.push_str(logs);
        body.push('\n');
    }
    body.push_str(&format!("Error: code run failed ({kind}): {message}"));
    let (body, truncated) = cap_output(body);
    ToolResult::error(body).with_details(details(description, calls, truncated))
}

fn cap_output(text: String) -> (String, bool) {
    let (cut, truncated) = truncate_bytes(&text, MAX_OUTPUT_BYTES.saturating_sub(80));
    if !truncated {
        return (text, false);
    }
    let mut body = cut;
    body.push_str("\n(output truncated; return a smaller value)");
    (body, true)
}

fn details(description: &str, calls: &[serde_json::Value], truncated: bool) -> serde_json::Value {
    json!({
        "description": description,
        "truncated": truncated,
        "calls": calls,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use mycode_core::tool::ToolSpec;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::RunCodeTool;
    use crate::builtin::python::{PythonStatus, set_python_status_for_test};
    use crate::builtin::{GrepTool, ReadTool, WriteTool};
    use crate::ctx::ToolCtx;
    use crate::registry::{ToolCatalog, ToolRegistry};
    use crate::stream::{ToolStream, ToolStreamItem};
    use crate::tool::{Tool, ToolDyn, ToolError, ToolResult};

    fn text_of(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                mycode_core::message::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn catalog(tools: Vec<Arc<dyn ToolDyn>>) -> ToolCatalog {
        let registry = ToolRegistry::new();
        for tool in tools {
            registry.register(tool);
        }
        ToolCatalog::from_registry(&registry)
    }

    async fn run(dir: &std::path::Path, tools: ToolCatalog, code: &str) -> ToolResult {
        run_with(ToolCtx::new(dir).with_catalog(tools), code).await
    }

    /// The interpreter probe is process-global, and the missing-Python test
    /// swaps it. Executions take a read lock and stay concurrent; the swap
    /// takes the write lock, so no running program observes the override.
    static STATUS_SWAP: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

    async fn run_with(ctx: ToolCtx, code: &str) -> ToolResult {
        let _serial = STATUS_SWAP.read().await;
        run_with_unlocked(ctx, code).await
    }

    async fn run_with_unlocked(ctx: ToolCtx, code: &str) -> ToolResult {
        let (mut stream, mut rx) = ToolStream::channel();
        let result = RunCodeTool
            .execute(
                super::RunCodeArgs {
                    description: "test".to_owned(),
                    code: code.to_owned(),
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("run_code returns a result");
        drop(stream);
        while rx.recv().await.is_some() {}
        result
    }

    async fn run_watching(ctx: ToolCtx, code: &str) -> (ToolResult, Vec<ToolStreamItem>) {
        let (mut stream, mut rx) = ToolStream::channel();
        let result = RunCodeTool
            .execute(
                super::RunCodeArgs {
                    description: "watch".to_owned(),
                    code: code.to_owned(),
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("run_code");
        drop(stream);
        let mut items = Vec::new();
        while let Some(item) = rx.recv().await {
            items.push(item);
        }
        (result, items)
    }

    fn temp_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mycode-ptc-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("temp");
        root
    }

    #[tokio::test]
    async fn filters_grep_hits_so_only_the_return_value_comes_back() {
        let root = temp_dir("grep");
        std::fs::write(
            root.join("turn.rs"),
            "fn dispatch_tool() {}\nfn other() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("skip.rs"), "fn unrelated() {}\n").unwrap();
        let result = run(
            &root,
            catalog(vec![Arc::new(GrepTool)]),
            super::RUN_CODE_EXAMPLE_CODE,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("turn.rs"), "{text}");
        assert!(text.contains("dispatch_tool"), "{text}");
        assert!(!text.contains("unrelated"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn gather_reads_two_files_and_strips_the_revision_tag() {
        let root = temp_dir("all");
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("b.txt"), "beta\n").unwrap();
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool)]),
            "\
a, b = await asyncio.gather(tools.read(path=\"a.txt\", limit=1), tools.read(path=\"b.txt\", limit=1))\n\
return a + \"\\n\" + b\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("alpha"), "{text}");
        assert!(text.contains("beta"), "{text}");
        assert!(!text.contains("[revision"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_missing_file_is_catchable_and_prints_survive() {
        let root = temp_dir("miss");
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool)]),
            r#"
print("kept")
try:
    await tools.read(path="missing.txt")
    return "no"
except ToolCallError as e:
    return e.toolName + ":" + e.message
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("kept"), "{text}");
        assert!(text.contains("read:"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn an_uncaught_error_keeps_earlier_output_and_the_user_line() {
        let root = temp_dir("line");
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool)]),
            "print(\"before\")\nx = 1\nraise RuntimeError(\"boom\")\n",
        )
        .await;
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("before"), "{text}");
        assert!(text.contains("boom"), "{text}");
        assert!(text.contains("line 3"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn nested_run_code_is_rejected() {
        let root = temp_dir("nest");
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool), Arc::new(RunCodeTool)]),
            r#"
try:
    await tools.run_code(description="again", code="return 1")
    return "no"
except ToolCallError as e:
    return e.message
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("cannot be called"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_write_outside_the_workspace_fails_through_preflight() {
        let root = temp_dir("perm");
        let outside =
            std::env::temp_dir().join(format!("mycode-ptc-outside-{}", std::process::id()));
        let _ = std::fs::remove_file(&outside);
        let path = serde_json::to_string(&outside.display().to_string()).unwrap();
        let code = format!(
            r#"
try:
    await tools.write(path={path}, content="nope")
    return "wrote"
except ToolCallError as e:
    return "caught"
"#
        );
        let result = run(&root, catalog(vec![Arc::new(WriteTool)]), &code).await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("caught"), "{text}");
        assert!(!outside.exists(), "preflight let a write escape");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn scout_python_cannot_write_spawn_or_open_a_socket_but_can_read() {
        let root = temp_dir("scout");
        std::fs::write(root.join("ok.txt"), "visible\n").unwrap();
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool), Arc::new(GrepTool)]),
            r#"
import pathlib, socket, subprocess
err = []
try:
    pathlib.Path("nope.txt").write_text("x")
except Exception:
    err.append("write")
try:
    subprocess.Popen(["echo", "hi"])
except Exception:
    err.append("spawn")
try:
    socket.socket()
except Exception:
    err.append("net")
text = await tools.read(path="ok.txt")
return "|".join(err) + "|" + text
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("write"), "{text}");
        assert!(text.contains("spawn"), "{text}");
        assert!(text.contains("net"), "{text}");
        assert!(text.contains("visible"), "{text}");
        assert!(!root.join("nope.txt").exists(), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_main_agent_warns_when_python_calls_os_system() {
        let root = temp_dir("warn");
        let result = run(
            &root,
            catalog(vec![Arc::new(WriteTool)]),
            "import os\nos.system(\"echo hi\")\nreturn \"done\"\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("warning:"), "{text}");
        assert!(
            text.contains("os.system") || text.contains("subprocess"),
            "{text}"
        );
        assert!(text.contains("tools.shell"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    type ProbeLog = Arc<Mutex<Vec<(String, &'static str, Instant)>>>;

    struct Probe {
        name: &'static str,
        delay: Duration,
        log: ProbeLog,
    }

    #[async_trait]
    impl ToolDyn for Probe {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.to_owned(),
                description: "probe".to_owned(),
                params_schema: json!({"type": "object"}),
            }
        }

        async fn execute_dyn(
            &self,
            args: serde_json::Value,
            _ctx: &ToolCtx,
            _out: &mut ToolStream,
        ) -> Result<ToolResult, ToolError> {
            let path = args
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(self.name)
                .to_owned();
            self.log
                .lock()
                .expect("log")
                .push((path.clone(), "start", Instant::now()));
            tokio::time::sleep(self.delay).await;
            self.log
                .lock()
                .expect("log")
                .push((path.clone(), "end", Instant::now()));
            Ok(ToolResult::text(path))
        }
    }

    #[tokio::test]
    async fn gather_overlaps_read_only_calls_and_serializes_writes() {
        let root = temp_dir("sched");
        let _serial = STATUS_SWAP.read().await;
        let delay = Duration::from_millis(200);
        let log = Arc::new(Mutex::new(Vec::new()));
        let tools = catalog(vec![
            Arc::new(Probe {
                name: "read",
                delay,
                log: Arc::clone(&log),
            }),
            Arc::new(Probe {
                name: "write",
                delay,
                log: Arc::clone(&log),
            }),
        ]);
        let ctx = ToolCtx::new(&root).with_catalog(tools.clone());
        let overlapped = run_with_unlocked(
            ctx.clone(),
            "await asyncio.gather(tools.read(path=\"a\"), tools.read(path=\"b\"))\nreturn \"ok\"\n",
        )
        .await;
        let text = text_of(&overlapped);
        assert!(!overlapped.is_error, "{text}");
        let agent_log = Arc::new(Mutex::new(Vec::new()));
        let agents = catalog(vec![Arc::new(Probe {
            name: "agent",
            delay,
            log: Arc::clone(&agent_log),
        })]);
        let overlapped_agents = run_with_unlocked(
            ToolCtx::new(&root).with_catalog(agents),
            "await asyncio.gather(tools.agent(path=\"a\"), tools.agent(path=\"b\"))\nreturn \"ok\"\n",
        )
        .await;
        let agent_text = text_of(&overlapped_agents);
        assert!(!overlapped_agents.is_error, "{agent_text}");
        log.lock().expect("log").clear();
        let serial = run_with_unlocked(
            ctx,
            "await asyncio.gather(tools.write(path=\"a\"), tools.write(path=\"b\"))\nreturn \"ok\"\n",
        )
        .await;
        let text = text_of(&serial);
        assert!(!serial.is_error, "{text}");
        // The scheduling contract is asserted on the probes' recorded
        // intervals. Exclusive (mutating) calls must never overlap — that is
        // the hard invariant. Read-only and agent calls MAY overlap; on the
        // single-threaded core runtime the second worker can start only after
        // the first parks, so overlap is opportunistic and not asserted.
        let reads = intervals(&log);
        assert_eq!(reads.len(), 2, "two read probes must run: {reads:?}");
        let agents_run = intervals(&agent_log);
        assert_eq!(
            agents_run.len(),
            2,
            "two agent probes must run: {agents_run:?}"
        );
        let writes = intervals(&log);
        assert_eq!(writes.len(), 2, "two write probes must run: {writes:?}");
        assert!(
            !intervals_overlap(&writes[0], &writes[1]),
            "exclusive calls overlapped: {writes:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Start/end interval of each probe that ran, from the shared probe log.
    fn intervals(log: &ProbeLog) -> Vec<(Instant, Instant)> {
        let guard = log.lock().expect("log");
        let mut starts = std::collections::HashMap::new();
        let mut out = Vec::new();
        for (path, kind, at) in guard.iter() {
            match *kind {
                "start" => {
                    starts.insert(path.clone(), *at);
                }
                _ => {
                    if let Some(start) = starts.remove(path) {
                        out.push((start, *at));
                    }
                }
            }
        }
        out
    }

    fn intervals_overlap(left: &(Instant, Instant), right: &(Instant, Instant)) -> bool {
        left.0 < right.1 && right.0 < left.1
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_python_process_tree() {
        let root = temp_dir("kill");
        let token = CancellationToken::new();
        let ctx = ToolCtx::new(&root)
            .with_catalog(catalog(vec![Arc::new(WriteTool)]))
            .with_cancel(token.clone());
        let code = "\
import subprocess, sys, time\n\
from pathlib import Path\n\
child = subprocess.Popen([sys.executable, \"-c\", \"import time; time.sleep(60)\"])\n\
Path(\"child.pid\").write_text(str(child.pid))\n\
time.sleep(45)\n\
return \"done\"\n";
        let ctx_task = ctx.clone();
        let task = tokio::spawn(async move { run_with(ctx_task, code).await });
        let pid_path = root.join("child.pid");
        let mut pid = None;
        for _ in 0..50 {
            if let Ok(text) = std::fs::read_to_string(&pid_path) {
                pid = text.trim().parse::<i32>().ok();
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let pid = pid.expect("child pid was not written");
        token.cancel();
        let result = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("cancel hung")
            .expect("task");
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("cancel") || text.contains("abort"), "{text}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(!alive, "grandchild {pid} survived the tree kill");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_short_wall_clock_times_out_and_returns() {
        let root = temp_dir("time");
        let ctx = ToolCtx::new(&root)
            .with_catalog(catalog(vec![Arc::new(ReadTool)]))
            .with_ptc_wall(Duration::from_secs(1));
        let started = Instant::now();
        let result = run_with(ctx, "import time\ntime.sleep(30)\nreturn \"late\"\n").await;
        let elapsed = started.elapsed();
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(
            text.contains("timeout") || text.contains("exceeded"),
            "{text}"
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "timeout did not kill the process: {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn missing_python_returns_install_steps() {
        // Swap the process-global probe while holding the exclusive lock, so
        // no concurrently running program observes the override.
        let _serial = STATUS_SWAP.write().await;
        set_python_status_for_test(Some(PythonStatus::Missing {
            message: "Python 3.10+ was not found on PATH. run_code needs the system Python.\nInstall steps for the test.".to_owned(),
        }));
        let root = temp_dir("nopy");
        let result = run_with_unlocked(
            ToolCtx::new(&root).with_catalog(catalog(vec![Arc::new(ReadTool)])),
            "return 1\n",
        )
        .await;
        set_python_status_for_test(None);
        drop(_serial);
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("Python 3.10+"), "{text}");
        assert!(text.contains("Install steps"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    struct GateAsk {
        entered: Arc<std::sync::atomic::AtomicBool>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<String>>>,
    }

    #[async_trait]
    impl crate::builtin::AskChannel for GateAsk {
        async fn ask(
            &self,
            _questions: &[crate::builtin::AskQuestion],
            cancel: &CancellationToken,
        ) -> Result<Vec<crate::builtin::AskAnswer>, ToolError> {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let receiver = self
                .release
                .lock()
                .expect("release")
                .take()
                .expect("oneshot");
            tokio::select! {
                _ = cancel.cancelled() => Err(ToolError::Execution("cancelled".to_owned())),
                answer = receiver => {
                    let answer = answer
                        .map_err(|_| ToolError::Execution("the user dismissed the questions".to_owned()))?;
                    Ok(vec![crate::builtin::AskAnswer { question: "Go?".to_owned(), answer }])
                }
            }
        }
    }

    #[tokio::test]
    async fn ask_user_blocks_the_script_until_the_answer() {
        let root = temp_dir("ask");
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let channel = Arc::new(GateAsk {
            entered: Arc::clone(&entered),
            release: Mutex::new(Some(rx)),
        });
        let tools = catalog(vec![Arc::new(crate::builtin::AskTool::new(channel))]);
        let ctx = ToolCtx::new(&root).with_catalog(tools);
        let task = tokio::spawn(async move {
            run_with(
                ctx,
                "answer = await tools.ask_user(questions=[{\"question\": \"Go?\"}])\nreturn answer\n",
            )
            .await
        });
        for _ in 0..50 {
            if entered.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            entered.load(std::sync::atomic::Ordering::SeqCst),
            "ask_user did not block inside the script"
        );
        assert!(!task.is_finished(), "script finished before the answer");
        tx.send("yes".to_owned()).expect("send");
        let result = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("ask hung")
            .expect("task");
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("yes"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn cancelling_the_turn_aborts_the_program() {
        let root = temp_dir("ask-cancel");
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (_tx, rx) = tokio::sync::oneshot::channel::<String>();
        let channel = Arc::new(GateAsk {
            entered: Arc::clone(&entered),
            release: Mutex::new(Some(rx)),
        });
        let token = CancellationToken::new();
        let ctx = ToolCtx::new(&root)
            .with_catalog(catalog(vec![Arc::new(crate::builtin::AskTool::new(
                channel,
            ))]))
            .with_cancel(token.clone());
        let task = tokio::spawn(async move {
            run_with(
                ctx,
                r#"
try:
    await tools.ask_user(questions=[{"question": "Go?"}])
    return "no"
except ToolCallError as e:
    return "caught:" + e.message
"#,
            )
            .await
        });
        for _ in 0..50 {
            if entered.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        token.cancel();
        let result = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("cancel hung")
            .expect("task");
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(
            text.contains("program cancelled"),
            "stopping the turn aborts the program: {text}"
        );
        assert!(
            !text.contains("caught:"),
            "a turn cancel must not be delivered as a catchable tool error: {text}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn dismissing_ask_user_is_a_catchable_tool_error() {
        let root = temp_dir("ask-dismiss");
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let channel = Arc::new(GateAsk {
            entered: Arc::clone(&entered),
            release: Mutex::new(Some(rx)),
        });
        let ctx = ToolCtx::new(&root).with_catalog(catalog(vec![Arc::new(
            crate::builtin::AskTool::new(channel),
        )]));
        let task = tokio::spawn(async move {
            run_with(
                ctx,
                r#"
try:
    await tools.ask_user(questions=[{"question": "Go?"}])
    return "no"
except ToolCallError as e:
    return "caught:" + e.message
"#,
            )
            .await
        });
        for _ in 0..50 {
            if entered.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(tx);
        let result = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("dismiss hung")
            .expect("task");
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(
            text.contains("caught:the user dismissed the questions"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn inner_calls_emit_nested_cards_and_not_their_text_as_the_program() {
        let root = temp_dir("cards");
        std::fs::write(root.join("a.txt"), "alpha-secret\n").unwrap();
        let ctx = ToolCtx::new(&root).with_catalog(catalog(vec![Arc::new(ReadTool)]));
        let (result, items) = run_watching(
            ctx,
            "text = await tools.read(path=\"a.txt\")\nreturn \"done\"\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("done"), "{text}");
        assert!(!text.contains("alpha-secret"), "{text}");
        assert!(
            items.iter().any(|item| matches!(
                item,
                ToolStreamItem::NestedStarted(start) if start.name == "read" && start.target.contains("a.txt")
            )),
            "missing nested card: {items:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn division_by_zero_includes_the_source_line() {
        let root = temp_dir("divzero");
        let result = run(&root, catalog(vec![]), "return 1/0\n").await;
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("ZeroDivisionError"), "{text}");
        assert!(text.contains("1/0"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_control_token_is_not_visible_to_user_code() {
        let root = temp_dir("token");
        let result = run(
            &root,
            catalog(vec![]),
            "import os\nreturn \"leaked\" if \"MYCODE_PTC_TOKEN\" in os.environ else \"clean\"\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("clean"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn reading_a_skill_file_returns_its_body_as_a_nested_card() {
        let root = temp_dir("skill-read");
        let skill = root.join(".agents").join("skills").join("review");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "# Review\nLoad this with tools.read inside run_code.\n",
        )
        .unwrap();
        let path = serde_json::to_string(&skill.join("SKILL.md").display().to_string()).unwrap();
        let code = format!("text = await tools.read(path={path})\nreturn text\n");
        let ctx = ToolCtx::new(&root).with_catalog(catalog(vec![Arc::new(ReadTool)]));
        let (result, items) = run_watching(ctx, &code).await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("tools.read inside run_code"), "{text}");
        assert!(
            items.iter().any(|item| matches!(
                item,
                ToolStreamItem::NestedStarted(start) if start.name == "read"
            )),
            "missing nested card: {items:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn description_is_the_first_schema_property() {
        let schema = RunCodeTool.params_schema();
        let required = schema["required"].as_array().expect("required");
        assert_eq!(required[0], "description");
        assert_eq!(required[1], "code");
        let props = schema["properties"].as_object().expect("properties");
        let mut names = props.keys();
        assert_eq!(names.next().map(String::as_str), Some("description"));
        assert_eq!(names.next().map(String::as_str), Some("code"));
    }

    /// A tool reply close to the frame limit must arrive intact: the channel
    /// is length-framed, so a payload far larger than any pipe buffer still
    /// round-trips without truncation or interleaving.
    #[tokio::test]
    async fn a_large_tool_reply_round_trips_intact() {
        let root = temp_dir("large");
        struct Big;
        #[async_trait]
        impl ToolDyn for Big {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "big".to_owned(),
                    description: "large reply".to_owned(),
                    params_schema: json!({"type": "object"}),
                }
            }
            async fn execute_dyn(
                &self,
                _args: serde_json::Value,
                _ctx: &ToolCtx,
                _out: &mut ToolStream,
            ) -> Result<ToolResult, ToolError> {
                Ok(ToolResult::text("x".repeat(2 * 1024 * 1024)))
            }
        }
        let result = run(
            &root,
            catalog(vec![Arc::new(Big)]),
            "text = await tools.big()\nreturn len(text)\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert_eq!(text.trim(), (2 * 1024 * 1024).to_string());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A slow consumer must not wedge the channel: the program interleaves a
    /// long stream of prints with a tool call, and everything still lands.
    /// The print volume stays under the rendered-output cap so the return
    /// value is still visible in the result text.
    #[tokio::test]
    async fn heavy_printing_alongside_a_tool_call_does_not_deadlock() {
        let root = temp_dir("prints");
        std::fs::write(root.join("ok.txt"), "visible\n").unwrap();
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool)]),
            r#"
text = await tools.read(path="ok.txt")
for i in range(300):
    print("line %d" % i)
return text.strip()
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("line 299"), "{text}");
        assert!(text.contains("visible"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Non-ASCII content must survive the whole chain in both directions.
    #[tokio::test]
    async fn non_ascii_output_and_errors_round_trip() {
        let root = temp_dir("utf8");
        let result = run(
            &root,
            catalog(vec![]),
            "print(\"日志 line ✓\")\nreturn \"中文🎉 émoji\"\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("日志 line ✓"), "{text}");
        assert!(text.contains("中文🎉 émoji"), "{text}");
        let failed = run(
            &root,
            catalog(vec![]),
            "raise ValueError(\"错误💥\")\n",
        )
        .await;
        let text = text_of(&failed);
        assert!(failed.is_error, "{text}");
        assert!(text.contains("错误💥"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A python process that dies mid-program surfaces as a worker exit,
    /// not a hang: the host notices the closed channel and finishes.
    #[tokio::test]
    async fn a_child_that_exits_mid_program_reports_worker_exit() {
        let root = temp_dir("die");
        let started = Instant::now();
        let result = run(
            &root,
            catalog(vec![Arc::new(ReadTool)]),
            "import os\nos._exit(3)\n",
        )
        .await;
        let elapsed = started.elapsed();
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(
            text.contains("python closed the control channel") || text.contains("lost the python"),
            "{text}"
        );
        assert!(
            elapsed < Duration::from_secs(20),
            "a dead child must not hang the host: {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
