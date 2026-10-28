//! Regression coverage for code-mode dispatch, cancellation and output.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{CellPhase, CodeCells, RunCodeTool, WaitCodeTool};
use crate::{ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolResult, ToolSpec, ToolStream};

struct ObservedTool {
    token: Arc<Mutex<Option<CancellationToken>>>,
}

impl ToolDyn for ObservedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "observed".to_owned(),
            description: "Records its cancellation token and stays pending.".to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: Value,
        ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            *self.token.lock().unwrap() = Some(ctx.cancel.clone());
            std::future::pending().await
        })
    }
}

fn text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_providers::message::ContentBlock::Text(block) => Some(block.text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn cancelling_a_yielded_cell_stops_an_uncooperative_inner_tool() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let token = Arc::new(Mutex::new(None));
    registry.register(Arc::new(ObservedTool {
        token: Arc::clone(&token),
    }));
    let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir());
    let result = tool.execute_dyn(
        json!({"script": "call(\"observed\", #{}); complete(\"should not run\");", "yield_time_ms": 30}),
        &ctx,
        &mut ToolStream::closed(),
    ).await.unwrap();
    assert!(text(&result).contains("Script running"));
    let cell = cells.get("s", "cell-0").unwrap();
    let epoch = cell.yield_epoch();
    assert!(token.lock().unwrap().is_some(), "inner tool started");
    ctx.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), cell.wait_settled(epoch))
        .await
        .expect("turn cancellation settles a yielded cell");
    assert_eq!(cell.phase(), CellPhase::Terminated);
    assert!(!cell.snapshot().output.contains("should not run"));
    drop(registry);
}

#[tokio::test]
async fn terminating_a_cell_cancels_its_inner_context_without_cancelling_the_turn() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let token = Arc::new(Mutex::new(None));
    registry.register(Arc::new(ObservedTool {
        token: Arc::clone(&token),
    }));
    let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir());
    tool.execute_dyn(
        json!({"script": "call(\"observed\", #{});", "yield_time_ms": 30}),
        &ctx,
        &mut ToolStream::closed(),
    )
    .await
    .unwrap();
    let inner_cancel = token.lock().unwrap().clone().expect("inner tool started");
    let wait = WaitCodeTool::new(Arc::clone(&cells), "s".to_owned());
    wait.execute_dyn(
        json!({"cell_id": "cell-0", "terminate": true}),
        &ctx,
        &mut ToolStream::closed(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), inner_cancel.cancelled())
        .await
        .expect("cell termination reaches detached inner work");
    assert!(
        !ctx.cancel.is_cancelled(),
        "cell stop must not cancel its parent turn"
    );
}

#[tokio::test]
async fn a_precancelled_run_does_not_create_a_script_cell() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir());
    ctx.cancel.cancel();
    let error = tool
        .execute_dyn(
            json!({"script": "complete(\"late\");"}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("interrupted by user"));
    assert!(cells.is_empty(), "a cancelled call must not start work");
}

#[tokio::test]
async fn cancelling_or_dropping_wait_code_stops_the_cell_it_is_collecting() {
    for cancel_wait in [true, false] {
        let cells = Arc::new(CodeCells::new());
        let registry = Arc::new(ToolRegistry::new());
        let token = Arc::new(Mutex::new(None));
        registry.register(Arc::new(ObservedTool {
            token: Arc::clone(&token),
        }));
        let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
        let origin = ToolCtx::new(std::env::temp_dir());
        run.execute_dyn(
            json!({"script": "call(\"observed\", #{});", "yield_time_ms": 30}),
            &origin,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
        let wait = WaitCodeTool::new(Arc::clone(&cells), "s".to_owned());
        let ctx = ToolCtx::new(std::env::temp_dir());
        if cancel_wait {
            ctx.cancel.cancel();
        }
        let result = tokio::time::timeout(
            Duration::from_millis(30),
            wait.execute_dyn(
                json!({"cell_id": "cell-0", "yield_time_ms": 5_000}),
                &ctx,
                &mut ToolStream::closed(),
            ),
        )
        .await;
        if cancel_wait {
            assert!(result.unwrap().is_err());
        } else {
            assert!(result.is_err());
        }
        assert_eq!(
            cells.get("s", "cell-0").unwrap().phase(),
            CellPhase::Terminated
        );
        let inner = token.lock().unwrap().clone().unwrap();
        tokio::time::timeout(Duration::from_secs(1), inner.cancelled())
            .await
            .unwrap();
        assert!(!origin.cancel.is_cancelled());
    }
}

#[tokio::test]
async fn dropping_run_code_cancels_inner_work_and_settles_its_cell() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let token = Arc::new(Mutex::new(None));
    registry.register(Arc::new(ObservedTool {
        token: Arc::clone(&token),
    }));
    let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir());
    let result = tokio::time::timeout(
        Duration::from_millis(30),
        run.execute_dyn(
            json!({"script": "call(\"observed\", #{});", "yield_time_ms": 5_000}),
            &ctx,
            &mut ToolStream::closed(),
        ),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        cells.get("s", "cell-0").unwrap().phase(),
        CellPhase::Terminated
    );
    let inner = token.lock().unwrap().clone().unwrap();
    tokio::time::timeout(Duration::from_secs(1), inner.cancelled())
        .await
        .unwrap();
    assert!(!ctx.cancel.is_cancelled());
}

#[tokio::test]
async fn host_call_limits_and_complete_inside_a_function_end_evaluation() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir());
    let result = run
        .execute_dyn(
            json!({"script": "for i in 0..257 { call(\"missing\", #{}); }"}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(
        result.is_error && text(&result).contains("host call budget exhausted"),
        "{result:?}"
    );
    let result = run
        .execute_dyn(
            json!({"script": "fn stop() { complete(\"done\"); } stop(); throw \"unreachable\";"}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(text(&result), "done");
}

struct StreamingTool;

impl ToolDyn for StreamingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "streaming".to_owned(),
            description: "Finishes synchronously with queued progress and a terminal.".to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        _args: Value,
        _ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            out.progress("agent|scout|running|tail progress");
            out.terminal(ToolResult::error("streamed failure"));
            Ok(ToolResult::text("returned fallback"))
        })
    }
}

#[tokio::test]
async fn inner_dispatch_preserves_queued_progress_and_the_first_terminal() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(StreamingTool));
    let tool = RunCodeTool::new(Arc::clone(&registry), cells, "s".to_owned());
    let (mut out, mut updates) = ToolStream::channel();
    let result = tool
        .execute_dyn(
            json!({"script": "let r = call(\"streaming\", #{}); complete(r);"}),
            &ToolCtx::new(std::env::temp_dir()),
            &mut out,
        )
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(output["is_error"], true);
    assert!(
        output["content"][0]["Text"]
            .as_str()
            .unwrap()
            .contains("streamed failure")
    );
    let mut progress = Vec::new();
    while let Some(item) = updates.try_recv() {
        if let crate::ToolStreamItem::Progress(update) = item {
            progress.push(update.message);
        }
    }
    assert!(
        progress
            .iter()
            .any(|line| matches!(super::CodeModeProgress::parse(line),
                Some(super::CodeModeProgress::Progress { message, .. })
                    if message == "agent|scout|running|tail progress")),
        "{progress:?}"
    );
}

#[tokio::test]
async fn dispatch_refusals_keep_legacy_error_and_the_tool_result_shape() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let tool = RunCodeTool::new(Arc::clone(&registry), cells, "s".to_owned());
    let result = tool
        .execute_dyn(
            json!({"script": "let r = call(\"missing\", #{}); complete(r);"}),
            &ToolCtx::new(std::env::temp_dir()),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&text(&result)).unwrap();
    assert!(
        output["__error__"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
    assert_eq!(output["is_error"], true);
    assert!(output["content"].is_array());
}

#[tokio::test]
async fn yield_is_journalled_after_earlier_queued_logs() {
    let cells = Arc::new(CodeCells::new());
    let registry = Arc::new(ToolRegistry::new());
    let tool = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    tool.execute_dyn(
        json!({"script": "log(\"first\"); yield_control(); complete(\"last\");"}),
        &ToolCtx::new(std::env::temp_dir()),
        &mut ToolStream::closed(),
    )
    .await
    .unwrap();
    let cell = cells.get("s", "cell-0").unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !cell.phase().is_terminal() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let journal = cell.journal();
    let steps: Vec<_> = journal
        .entries()
        .iter()
        .map(|entry| (entry.seq, entry.kind))
        .collect();
    assert_eq!(
        steps,
        vec![
            (0, super::ScriptEventKind::Log),
            (1, super::ScriptEventKind::Yield),
            (2, super::ScriptEventKind::Complete)
        ]
    );
}

#[test]
fn failed_script_output_respects_the_cap() {
    let cells = CodeCells::new();
    let finished = cells.create("s", 16);
    finished.finish(super::ScriptOutcome::Failed("é".repeat(100)));
    assert!(
        finished.snapshot().output.len() < 100,
        "failure text must be bounded"
    );
}
