//! Code-mode tool tests: recursion refusal, a real `read` call through
//! `call`, and the yield/wait/terminate lifecycle.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ToolSpec;
use crate::code_mode::{CodeCells, RunCodeTool, WaitCodeTool};
use crate::ctx::ToolCtx;
use crate::registry::ToolRegistry;
use crate::stream::{ToolStream, ToolStreamItem};
use crate::tool::{ToolDyn, ToolError, ToolResult};

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
    registry.register(Arc::new(crate::builtin::WriteTool));
    registry.register(Arc::new(crate::builtin::EditTool));
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
async fn javascript_values_and_array_transforms_execute_in_process() {
    let workspace = workspace("js-transform");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": "[1, 2, 3].map(n => ({ value: n * 2 })).filter(row => row.value > 2);"}),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&text_of(&result)).unwrap(),
        json!([{ "value": 4 }, { "value": 6 }])
    );
}

#[tokio::test]
async fn javascript_transforms_file_data_and_writes_through_registered_tools() {
    let workspace = workspace("js-file-transform");
    std::fs::write(
        workspace.0.join("rows.json"),
        r#"[{"name":"β","score":2},{"name":"a","score":1}]"#,
    )
    .unwrap();
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": r#"
const input = call("read", { path: "rows.json" });
if (input.is_error) throw new Error(input.content[0].Text);
const source = input.content[0].Text.replace(/\n\[revision [^\n]*\]$/, "");
const rows = JSON.parse(source).sort((a, b) => a.score - b.score)
    .map(({ name, score }) => ({ name: name.toUpperCase(), score: score * 10 }));
const output = call("write", { path: "result.json", content: JSON.stringify(rows) });
if (output.is_error) throw new Error(output.content[0].Text);
const edited = call("edit", { path: "result.json", old_string: "A", new_string: "ALPHA" });
if (edited.is_error) throw new Error(edited.content[0].Text);
complete(rows.length);
"#}),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(text_of(&result), "2");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(workspace.0.join("result.json")).unwrap())
            .unwrap();
    assert_eq!(
        saved,
        json!([{ "name": "ALPHA", "score": 10 }, { "name": "Β", "score": 20 }])
    );
}

#[tokio::test]
async fn javascript_errors_limits_and_serialization_are_reported() {
    let workspace = workspace("js-errors");
    let cells = Arc::new(CodeCells::new());
    for (script, expected) in [
        ("const = ;", "error:"),
        ("throw new Error('broken');", "broken"),
        ("throw 'plain failure';", "plain failure"),
        (
            "try { while (true) {} } catch (_) { complete('escaped'); }",
            "operation budget exhausted",
        ),
        ("'x'.repeat(64 * 1024 * 1024);", "memory"),
        (
            "function recurse() { return recurse(); } recurse();",
            "stack",
        ),
        (
            "const cycle = {}; cycle.self = cycle; complete(cycle);",
            "circular",
        ),
        ("complete(1n);", "BigInt"),
        ("(async () => 1)();", "Async JavaScript is not supported"),
        (
            "(async () => { await 1; call('missing', {}); })(); void 0;",
            "Async JavaScript is not supported",
        ),
    ] {
        let result = run_script(
            &cells,
            &workspace.0,
            json!({"script": script, "yield_time_ms": 5_000}),
        )
        .await
        .unwrap();
        assert!(result.is_error, "{script}: {result:?}");
        assert!(
            text_of(&result).contains(expected),
            "{script}: {}",
            text_of(&result)
        );
        assert_eq!(result.details.as_ref().unwrap()["phase"], "failed");
    }
}

#[tokio::test]
async fn javascript_completion_bypasses_catch_and_finally_and_preserves_json() {
    let workspace = workspace("js-completion");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": r#"
try { complete({ rows: [1, null, "中文"], valid: true }); }
catch (_) { call("read", { path: "unreachable" }); }
finally { log("unreachable"); }
log("unreachable");
"#}),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&text_of(&result)).unwrap(),
        json!({"rows": [1, null, "中文"], "valid": true})
    );
    assert_eq!(result.details.as_ref().unwrap()["calls"], 0);
    let null = run_script(
        &cells,
        &workspace.0,
        json!({"script": "complete(undefined);"}),
    )
    .await
    .unwrap();
    assert_eq!(text_of(&null), "null");
    let empty = run_script(&cells, &workspace.0, json!({"script": "const value = 1;"}))
        .await
        .unwrap();
    assert_eq!(text_of(&empty), "(no output)");
}

#[tokio::test]
async fn javascript_output_and_source_limits_are_enforced() {
    let workspace = workspace("js-output");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({"script": "log('é'.repeat(2048)); complete('tail');", "max_output_bytes": 1024}),
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{result:?}");
    let text = text_of(&result);
    assert!(text.contains("output truncated at 1024 bytes"), "{text}");
    assert!(text.len() < 1100);
    let script = " ".repeat(super::engine::MAX_SCRIPT_BYTES + 1);
    let result = run_script(&cells, &workspace.0, json!({"script": script}))
        .await
        .unwrap();
    assert!(
        result.is_error && text_of(&result).contains("script exceeds"),
        "{result:?}"
    );
}

#[tokio::test]
async fn read_images_reach_native_run_and_wait_results_once_without_base64_text() {
    use base64::Engine as _;
    use mycode_providers::message::ContentBlock;

    let workspace = workspace("js-image");
    let encoded = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ1sAAAAASUVORK5CYII=";
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    std::fs::write(workspace.0.join("pixel.png"), png).unwrap();
    for (script, first_has_image, is_error) in [
        (
            r#"complete(call("read", { path: "pixel.png" }));"#,
            true,
            false,
        ),
        (
            r#"call("quick", {}); complete(call("read", { path: "pixel.png" }));"#,
            false,
            false,
        ),
        (
            r#"const image = call("read", { path: "pixel.png" }); yield_control(); call("quick", {}); complete(image);"#,
            true,
            false,
        ),
        (
            r#"call("read", { path: "pixel.png" }); throw new Error("after image");"#,
            true,
            true,
        ),
    ] {
        let cells = Arc::new(CodeCells::new());
        let yield_ms = if first_has_image { 5_000 } else { 10 };
        // The delayed case yields before the read; an explicit yield emits
        // an already-read image with the running cell result.
        let first = run_script(
            &cells,
            &workspace.0,
            json!({"script": script, "yield_time_ms": yield_ms}),
        )
        .await
        .unwrap();
        let images = |result: &ToolResult| {
            result
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Image(image) => Some(image.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let initial_images = images(&first);
        assert_eq!(
            initial_images.len(),
            usize::from(first_has_image),
            "{first:?}"
        );
        let cell_id = first.details.as_ref().unwrap()["cell_id"].as_str().unwrap();
        let collected = wait_cell(
            &cells,
            &workspace.0,
            json!({"cell_id": cell_id, "yield_time_ms": 5_000}),
        )
        .await
        .unwrap();
        assert_eq!(collected.is_error, is_error, "{collected:?}");
        let all_images: Vec<_> = initial_images
            .into_iter()
            .chain(images(&collected))
            .collect();
        assert_eq!(all_images.len(), 1);
        assert_eq!(all_images[0].mime_type, "image/png");
        assert_eq!(all_images[0].data, encoded);
        for result in [&first, &collected] {
            let text = text_of(result);
            assert!(!text.contains(encoded), "base64 must remain native");
            assert!(text.len() < 1024, "{text}");
        }
        let repeated = wait_cell(&cells, &workspace.0, json!({"cell_id": cell_id}))
            .await
            .unwrap();
        assert!(
            images(&repeated).is_empty(),
            "a collected image must not be emitted twice"
        );
    }
}

#[tokio::test]
async fn calling_run_code_from_a_script_is_refused() {
    let workspace = workspace("recurse");
    let cells = Arc::new(CodeCells::new());
    for script in [
        r#"call("run_code", { script: "complete(1)" });"#,
        r#"call("wait_code", { cell_id: "cell-0" });"#,
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
let r = call("read", { path: "note.txt" });
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
async fn the_result_names_its_inner_calls_for_the_ui() {
    let workspace = workspace("inner-calls");
    std::fs::write(workspace.0.join("note.txt"), "alpha body\n").unwrap();
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"
let a = call("read", { path: "note.txt" });
let b = call("nope", {});
complete("done");
"#,
            "yield_time_ms": 10_000,
        }),
    )
    .await
    .expect("the script runs");
    assert!(!result.is_error, "{result:?}");
    let details = result.details.expect("cell details");
    let calls = details["inner_calls"].as_array().expect("inner calls");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0]["name"], "read");
    assert_eq!(calls[0]["target"], "note.txt");
    assert_eq!(calls[0]["is_error"], false);
    assert_eq!(calls[1]["name"], "nope");
    assert_eq!(calls[1]["is_error"], true);
}

#[tokio::test]
async fn an_unknown_tool_is_reported_to_the_script() {
    let workspace = workspace("unknown");
    let cells = Arc::new(CodeCells::new());
    let result = run_script(
        &cells,
        &workspace.0,
        json!({
            "script": r#"let r = call("nope", {}); complete(r.__error__);"#,
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
            "script": r#"call("slow", {}); complete("done");"#,
            "yield_time_ms": 20,
        }),
    )
    .await
    .expect("run_code returns a running cell");
    assert!(!running.is_error, "{running:?}");
    let text = text_of(&running);
    assert!(
        text.contains("Script running with cell ID cell-0"),
        "{text}"
    );
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
    assert_eq!(
        cells.get(SCOPE, "cell-0").unwrap().phase(),
        super::CellPhase::Terminated
    );
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
                "script": r#"yield_control(); call("slow", {}); complete("late");"#,
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
            "script": r#"yield_control(); call("quick", {}); complete("late");"#,
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

/// The host provides only its registered bridge functions. File and network
/// access must go through tools, and dynamic code entry points are disabled.
#[tokio::test]
async fn side_channel_and_time_functions_are_refused() {
    let workspace = workspace("sandbox");
    let cells = Arc::new(CodeCells::new());
    for (script, expected) in [
        (r#"print("side channel");"#, "print"),
        (r#"debug("side channel");"#, "debug"),
        ("timestamp();", "timestamp"),
        ("sleep(1);", "sleep"),
        ("exit();", "exit"),
        ("require('fs');", "require"),
        ("process.cwd();", "process"),
        ("fetch('https://example.com');", "fetch"),
        ("new Date();", "Date"),
        ("Promise.resolve(1);", "Promise"),
        ("setTimeout(() => {}, 1);", "setTimeout"),
        ("Function('return 1')();", "Function"),
        ("(() => {}).constructor('return 1')();", "function"),
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
        let tool = RunCodeTool::new(
            Arc::clone(&registry),
            Arc::clone(&task_cells),
            SCOPE.to_owned(),
        );
        let ctx = ToolCtx::new(&root);
        tool.execute_dyn(
            json!({
                "script": r#"call("slow", {}); complete("done");"#,
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
    let result = handle
        .await
        .expect("the task ran")
        .expect("run_code returns");
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
        json!({"script": r#"call("panic", {});"#, "yield_time_ms": 5_000}),
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

/// Stopping a script must retain the native shell's process cleanup path.
#[cfg(windows)]
#[tokio::test]
async fn terminating_a_code_cell_reaps_its_shell_process() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let workspace = workspace("cancel-process");
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let shell = crate::ShellTool::new();
    let name = shell.spec().name;
    registry.register(Arc::new(shell));
    let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), SCOPE.to_owned());
    let command = "[IO.File]::WriteAllText((Join-Path $PWD 'started.pid'), [string]$PID); Start-Sleep -Seconds 30; Set-Content -Path late.txt -Value late";
    let script = format!("call({}, {{ command: {} }});", json!(name), json!(command));
    let ctx = ToolCtx::new(&workspace.0);
    let result = tool
        .execute_dyn(
            json!({"script": script, "yield_time_ms": 10}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(text_of(&result).contains("Script running"), "{result:?}");
    let cell = cells.get(SCOPE, "cell-0").unwrap();
    // Ensure an assertion failure also stops the test's process.
    let _stop_on_drop = super::TerminateOnDrop::new(Arc::clone(&cell));
    let pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(workspace.0.join("started.pid"))
                && let Ok(pid) = text.parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the script shell started");
    // SAFETY: OpenProcess creates an owned handle for this test's child.
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(
        !raw.is_null(),
        "open child process: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `raw` is non-null and uniquely owned after OpenProcess.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    cell.terminate();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // SAFETY: the owned handle stays valid throughout this wait.
            if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("terminating code mode reaps its shell process");
    assert!(!workspace.0.join("late.txt").exists());
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
        json!({"script": r#"call("slow", {});"#, "yield_time_ms": 20}),
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
