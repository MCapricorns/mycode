//! Code-mode tool tests: recursion refusal, a real `read` call through
//! `call`, and the yield/wait/terminate lifecycle.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::code_mode::{CodeCells, RunCodeTool, WaitCodeTool};
use crate::ctx::ToolCtx;
use crate::registry::ToolRegistry;
use crate::stream::{ToolStream, ToolStreamItem};
use crate::tool::{ToolDyn, ToolError, ToolResult};
use crate::ToolSpec;

struct SlowTool;

impl ToolDyn for SlowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow".to_owned(),
            description: "A tool that keeps the script waiting.".to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(ToolResult::text("slow done"))
        })
    }
}

/// Panics while executing, to prove the dispatcher settles its cell.
struct PanicTool;

impl ToolDyn for PanicTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "panic".to_owned(),
            description: "A tool that panics.".to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move { panic!("tool exploded") })
    }
}

/// Short enough that a `wait_code` collecting it is visible in wall time.
struct QuickTool;

impl ToolDyn for QuickTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "quick".to_owned(),
            description: "A tool that answers after a short delay.".to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(ToolResult::text("quick done"))
        })
    }
}

struct Workspace(std::path::PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn workspace(label: &str) -> Workspace {
    let root = std::env::temp_dir().join(format!(
        "mycode-code-mode-{label}-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    Workspace(root)
}

fn registry() -> Arc<ToolRegistry> {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(crate::builtin::ReadTool));
    registry.register(Arc::new(SlowTool));
    registry.register(Arc::new(QuickTool));
    registry.register(Arc::new(PanicTool));
    Arc::new(registry)
}

fn text_of(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            mycode_providers::message::ContentBlock::Text(text) => text.text.clone(),
            _ => String::new(),
        })
        .collect()
}

/// The session every helper script belongs to.
const SCOPE: &str = "session-a";

async fn run_script(
    cells: &Arc<CodeCells>,
    root: &std::path::Path,
    args: Value,
) -> Result<ToolResult, ToolError> {
    // The tool holds the registry weakly; the caller owns it, as the turn
    // does.
    let registry = registry();
    let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(cells), SCOPE.to_owned());
    let ctx = ToolCtx::new(root);
    let mut stream = ToolStream::closed();
    tool.execute_dyn(args, &ctx, &mut stream).await
}

async fn wait_cell(
    cells: &Arc<CodeCells>,
    root: &std::path::Path,
    args: Value,
) -> Result<ToolResult, ToolError> {
    let tool = WaitCodeTool::new(Arc::clone(cells), SCOPE.to_owned());
    let ctx = ToolCtx::new(root);
    let mut stream = ToolStream::closed();
    tool.execute_dyn(args, &ctx, &mut stream).await
}

#[tokio::test]
async fn calling_run_code_from_a_script_is_refused() {
    let workspace = workspace("recurse");
    let cells = Arc::new(CodeCells::new());
    for script in [
        r#"call("run_code", #{ script: "complete(1)" });"#,
        r#"call("wait_code", #{ cell_id: "cell-0" });"#,
    ] {
        let result = run_script(
            &cells,
            &workspace.0,
            json!({"script": script, "yield_time_ms": 5_000}),
        )
        .await
        .expect("the tool runs and reports the refusal as a failed script");
        assert!(result.is_error, "{result:?}");
        let text = text_of(&result);
        assert!(text.contains("cannot invoke itself"), "{text}");
    }
}

#[tokio::test]
async fn a_script_call_reads_a_file_through_the_registry() {
    let workspace = workspace("read");
    std::fs::write(workspace.0.join("note.txt"), "alpha body\n").unwrap();
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"
let r = call("read", #{ path: "note.txt" });
complete(r.content[0]);
"#,
            "yield_time_ms": 10_000,
        }),
    )
    .await
    .expect("the script runs");
    assert!(!result.is_error, "{result:?}");
    let text = text_of(&result);
    assert!(text.contains("alpha body"), "{text}");
    let details = result.details.expect("cell details");
    assert_eq!(details["phase"], "completed");
    let journal = details["journal"].as_array().expect("journal");
    assert!(
        journal.iter().any(|entry| entry["kind"] == "call"),
        "{journal:?}"
    );
}

#[tokio::test]
async fn an_unknown_tool_is_reported_to_the_script() {
    let workspace = workspace("unknown");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"let r = call("nope", #{}); complete(r.__error__);"#,
            "yield_time_ms": 10_000,
        }),
    )
    .await
    .expect("the script runs");
    let text = text_of(&result);
    assert!(text.contains("unknown tool: nope"), "{text}");
}

#[tokio::test]
async fn a_long_script_yields_a_cell_and_wait_code_stops_it() {
    let workspace = workspace("yield");
    let cells = Arc::new(CodeCells::new());
    let running = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"call("slow", #{}); complete("done");"#,
            "yield_time_ms": 20,
        }),
    )
    .await
    .expect("run_code returns a running cell");
    assert!(!running.is_error, "{running:?}");
    let text = text_of(&running);
    assert!(text.contains("Script running with cell ID cell-0"), "{text}");
    let cell_id = running
        .details
        .as_ref()
        .and_then(|details| details["cell_id"].as_str())
        .expect("cell id")
        .to_owned();
    assert_eq!(cell_id, "cell-0");

    let stopped = wait_cell(
        &cells,
        &workspace.0,
        json!({"cell_id": cell_id, "terminate": true}),
    )
    .await
    .expect("wait_code stops the cell");
    let text = text_of(&stopped);
    assert!(text.contains("terminated"), "{text}");
    assert_eq!(cells.get(SCOPE, "cell-0").unwrap().phase(), super::CellPhase::Terminated);
}

#[tokio::test]
async fn yield_control_returns_control_without_stopping_the_script() {
    let workspace = workspace("yield-control");
    let cells = Arc::new(CodeCells::new());
    let started = std::time::Instant::now();
    let running = tokio::time::timeout(
        Duration::from_secs(5),
        run_script(
            &cells,
            &workspace.0,
            json!({
                "script": r#"yield_control(); call("slow", #{}); complete("late");"#,
                "yield_time_ms": 60_000,
            }),
        ),
    )
    .await
    .expect("yield_control settles the call instead of waiting 60 s")
    .expect("run_code returns");
    assert!(started.elapsed() < Duration::from_secs(4));
    let text = text_of(&running);
    assert!(text.contains("Script running with cell ID"), "{text}");
    let _ = wait_cell(
        &cells,
        &workspace.0,
        json!({"cell_id": "cell-0", "terminate": true}),
    )
    .await;
}

#[tokio::test]
async fn wait_code_continues_a_yielded_cell_to_completion() {
    let workspace = workspace("wait-continue");
    let cells = Arc::new(CodeCells::new());
    let running = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"yield_control(); call("quick", #{}); complete("late");"#,
            "yield_time_ms": 5_000,
        }),
    )
    .await
    .expect("run_code returns the yielded cell");
    assert!(
        text_of(&running).contains("Script running with cell ID cell-0"),
        "{running:?}"
    );

    // The cell is already `Yielded`; this wait must be for the *next*
    // transition (the script completing), not return at once.
    let started = std::time::Instant::now();
    let done = wait_cell(
        &cells,
        &workspace.0,
        json!({"cell_id": "cell-0", "yield_time_ms": 5_000}),
    )
    .await
    .expect("wait_code collects the cell");
    let text = text_of(&done);
    assert!(text.contains("late"), "{text}");
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "wait_code returned without waiting: {:?}",
        started.elapsed()
    );
    assert_eq!(
        cells.get(SCOPE, "cell-0").unwrap().phase(),
        super::CellPhase::Completed
    );
}

/// The sandbox table in the design: the clock/sleep/exit/print functions are
/// stubbed with an error and `eval` is disabled, so the journal cannot be
/// bypassed.
#[tokio::test]
async fn side_channel_and_time_functions_are_refused() {
    let workspace = workspace("sandbox");
    let cells = Arc::new(CodeCells::new());
    for (script, expected) in [
        (r#"print("side channel");"#, "print"),
        (r#"debug("side channel");"#, "debug"),
        ("timestamp();", "timestamp is not available"),
        ("sleep(1);", "sleep is not available"),
        ("exit();", "exit is not available"),
        (r#"eval("1 + 1");"#, "eval"),
    ] {
        let result = run_script(
            &cells,
            &workspace.0,
            json!({"script": script, "yield_time_ms": 5_000}),
        )
        .await
        .expect("the tool reports the refusal as a failed script");
        assert!(result.is_error, "{script}: {result:?}");
        let text = text_of(&result);
        assert!(text.contains(expected), "{script}: {text}");
    }
}

#[tokio::test]
async fn wait_code_rejects_an_unknown_cell() {
    let workspace = workspace("missing-cell");
    let cells = Arc::new(CodeCells::new());
    let error = wait_cell(&cells, &workspace.0, json!({"cell_id": "cell-9"}))
        .await
        .expect_err("an unknown cell is invalid arguments");
    assert!(matches!(error, ToolError::InvalidArgs(_)), "{error:?}");
}

/// The turn's dispatcher stops reading progress only when every sender is
/// gone. A `ToolStream` clone held by the background script dispatcher used
/// to keep that stream open, so the turn waited for the script instead of
/// receiving the cell id.
#[tokio::test]
async fn the_progress_stream_closes_when_run_code_returns_a_running_cell() {
    let workspace = workspace("stream-close");
    let cells = Arc::new(CodeCells::new());
    let (mut producer, mut consumer) = ToolStream::channel();
    let root = workspace.0.clone();
    let task_cells = Arc::clone(&cells);
    let handle = tokio::spawn(async move {
        let registry = registry();
        let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&task_cells), SCOPE.to_owned());
        let ctx = ToolCtx::new(&root);
        tool.execute_dyn(
            json!({
                "script": r#"call("slow", #{}); complete("done");"#,
                "yield_time_ms": 20,
            }),
            &ctx,
            &mut producer,
        )
        .await
    });

    let items = tokio::time::timeout(Duration::from_secs(3), consumer.drain())
        .await
        .expect("the progress stream closes when run_code returns");
    assert!(
        items
            .iter()
            .all(|item| !matches!(item, ToolStreamItem::Terminal(_))),
        "run_code streams progress only; the terminal is the returned result"
    );
    assert!(
        items
            .iter()
            .any(|item| matches!(item, ToolStreamItem::Progress(_))),
        "the call progress reached the stream: {items:?}"
    );
    let result = handle.await.expect("the task ran").expect("run_code returns");
    assert!(text_of(&result).contains("Script running with cell ID cell-0"));
    if let Some(cell) = cells.get(SCOPE, "cell-0") {
        cell.terminate();
    }
}

/// `log` and `complete` are queued just before the script ends; the
/// dispatcher must drain them, not discard them in favour of `done`.
#[tokio::test]
async fn a_finished_script_journals_its_tail_log_and_complete() {
    let workspace = workspace("journal-tail");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"log("hello"); complete("bye");"#,
            "yield_time_ms": 5_000,
        }),
    )
    .await
    .expect("the script runs");
    assert!(!result.is_error, "{result:?}");
    let kinds: Vec<&str> = result
        .details
        .as_ref()
        .and_then(|details| details["journal"].as_array())
        .expect("journal")
        .iter()
        .filter_map(|entry| entry["kind"].as_str())
        .collect();
    assert_eq!(kinds, ["log", "complete"], "{result:?}");
}

/// A tool that panics must not leave the cell Running forever.
#[tokio::test]
async fn a_panicking_tool_settles_its_cell() {
    let workspace = workspace("panic");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": r#"call("panic", #{});"#, "yield_time_ms": 5_000}),
    )
    .await
    .expect("run_code reports the panic as a failed cell");
    assert!(result.is_error, "{result:?}");
    let text = text_of(&result);
    assert!(text.contains("panicked"), "{text}");
    assert_eq!(
        cells.get(SCOPE, "cell-0").expect("cell").phase(),
        super::CellPhase::Failed
    );
}

/// The registry owns its tools; a strong handle inside `run_code` would make
/// every turn's registry — and the MCP clients behind it — leak.
#[test]
fn run_code_does_not_keep_its_registry_alive() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(RunCodeTool::new(
        Arc::clone(&registry),
        Arc::clone(&cells),
        "s".to_owned(),
    )));
    let weak = Arc::downgrade(&registry);
    drop(registry);
    assert!(
        weak.upgrade().is_none(),
        "the registry leaked through its own run_code tool"
    );
}

/// One process-wide cell table, but a session can only reach its own cells.
#[tokio::test]
async fn wait_code_cannot_reach_another_sessions_cell() {
    let workspace = workspace("scope");
    let cells = Arc::new(CodeCells::new());
    let running = run_script(
        &cells,
        &workspace.0,
        json!({"script": r#"call("slow", #{});"#, "yield_time_ms": 20}),
    )
    .await
    .expect("run_code returns the running cell");
    assert!(text_of(&running).contains("Script running with cell ID cell-0"));

    let tool = WaitCodeTool::new(Arc::clone(&cells), "session-b".to_owned());
    let ctx = ToolCtx::new(&workspace.0);
    let mut stream = ToolStream::closed();
    let error = tool
        .execute_dyn(json!({"cell_id": "cell-0"}), &ctx, &mut stream)
        .await
        .expect_err("another session's cell is not addressable");
    assert!(matches!(error, ToolError::InvalidArgs(_)), "{error:?}");
    if let Some(cell) = cells.get(SCOPE, "cell-0") {
        cell.terminate();
    }
}
