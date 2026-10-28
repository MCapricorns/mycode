//! Code-mode child identity, completion, and progress continuation tests.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    CodeCells, CodeModeProgress, InnerCallPhase, InnerCallSummary, RunCodeTool, WaitCodeTool,
};
use crate::builtin::{AgentHost, AgentTool, SubagentRequest};
use crate::{ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolStream, ToolStreamItem};

#[derive(Default)]
struct RecordingHost {
    calls: Mutex<Vec<String>>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

impl AgentHost for RecordingHost {
    fn run_subagent<'a>(
        &'a self,
        request: SubagentRequest,
        progress: &'a ToolStream,
        _cancel: &'a CancellationToken,
        call_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(call_id.to_owned());
            let child_cancel = CancellationToken::new();
            self.cancels
                .lock()
                .unwrap()
                .insert(call_id.to_owned(), child_cancel.clone());
            progress.progress("agent|scout|queued|detail with | and\na newline");
            match request.prompt.as_str() {
                "cancel" => {
                    child_cancel.cancelled().await;
                    Err(ToolError::Execution(format!(
                        "Worktree retained at fixture. Changes remain available there.\n\n\
                         subagent scout cancelled{}\n\n<worktree-diff>changed</worktree-diff>",
                        crate::tool::INTERRUPTED_NOTE,
                    )))
                }
                "delayed" => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    progress.progress("agent|scout|step|late progress");
                    Ok("done".to_owned())
                }
                "panic" => panic!("child host panicked"),
                _ => Ok("done".to_owned()),
            }
        })
    }
}

fn registry(host: &Arc<RecordingHost>) -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(AgentTool::new(host.clone())));
    registry
}

fn events(items: Vec<ToolStreamItem>) -> Vec<CodeModeProgress> {
    items
        .into_iter()
        .filter_map(|item| match item {
            ToolStreamItem::Progress(update) => CodeModeProgress::parse(&update.message),
            ToolStreamItem::Terminal(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn inner_child_ids_match_host_registration_and_keep_independent_completion_states() {
    let host = Arc::new(RecordingHost::default());
    let registry = registry(&host);
    let cells = Arc::new(CodeCells::new());
    let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let (mut out, mut updates) = ToolStream::channel();
    let ctx = ToolCtx::new(std::env::temp_dir()).with_call_id("outer");
    let turn_cancel = ctx.cancel.clone();
    let task = tokio::spawn(async move {
        let _registry = registry;
        run.execute_dyn(
            json!({"script": r#"
let first = call("agent", #{ agent: "scout", prompt: "cancel" });
if !first.is_error { throw "first child should have been cancelled"; }
let second = call("agent", #{ agent: "scout", prompt: "success" });
if second.is_error { throw "second child should finish"; }
complete("done");
"#}),
            &ctx,
            &mut out,
        )
        .await
    });
    let mut items = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(item) = updates.recv().await {
            if let ToolStreamItem::Progress(update) = &item
                && let Some(CodeModeProgress::Progress {
                    call_id, message, ..
                }) = CodeModeProgress::parse(&update.message)
            {
                assert_eq!(call_id, "outer.0");
                assert_eq!(message, "agent|scout|queued|detail with | and\na newline");
                host.cancels
                    .lock()
                    .unwrap()
                    .get(&call_id)
                    .expect("same id registered by the host")
                    .cancel();
                items.push(item);
                break;
            }
            items.push(item);
        }
        items.extend(updates.drain().await);
    })
    .await
    .expect("independent child cancellation lets the script continue");
    let result = task.await.unwrap().unwrap();
    assert!(!result.is_error, "{result:?}");
    assert!(!turn_cancel.is_cancelled());
    assert_eq!(*host.calls.lock().unwrap(), ["outer.0", "outer.1"]);
    let events = events(items);
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            CodeModeProgress::Completed {
                call_id,
                phase,
                is_error,
                ..
            } => Some((call_id.as_str(), *phase, *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(
        completed,
        [
            ("outer.0", InnerCallPhase::Cancelled, true),
            ("outer.1", InnerCallPhase::Completed, false)
        ]
    );
    let calls = &result.details.as_ref().unwrap()["inner_calls"];
    assert_eq!(calls[0]["call_id"], "outer.0");
    assert_eq!(calls[0]["phase"], "cancelled");
    assert_eq!(calls[1]["call_id"], "outer.1");
    assert_eq!(calls[1]["phase"], "completed");
}

#[tokio::test]
async fn wait_code_forwards_live_child_progress_with_the_original_call_id() {
    let host = Arc::new(RecordingHost::default());
    let registry = registry(&host);
    let cells = Arc::new(CodeCells::new());
    let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir()).with_call_id("outer");
    let running = run.execute_dyn(
        json!({"script": "call(\"agent\", #{ agent: \"scout\", prompt: \"delayed\" });", "yield_time_ms": 20}),
        &ctx, &mut ToolStream::closed(),
    ).await.unwrap();
    assert_eq!(
        running.details.as_ref().unwrap()["inner_calls"][0]["call_id"],
        "outer.0"
    );
    assert_eq!(
        running.details.as_ref().unwrap()["inner_calls"][0]["phase"],
        "running"
    );
    let wait = WaitCodeTool::new(Arc::clone(&cells), "s".to_owned());
    let (mut out, mut updates) = ToolStream::channel();
    let done = wait
        .execute_dyn(
            json!({"cell_id": "cell-0"}),
            &ToolCtx::new(std::env::temp_dir()).with_call_id("waiter"),
            &mut out,
        )
        .await
        .unwrap();
    drop(out);
    let events = events(updates.drain().await);
    assert!(
        events.iter().any(|event| matches!(event,
        CodeModeProgress::Progress { call_id, message, .. }
            if call_id == "outer.0" && message == "agent|scout|step|late progress")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(event,
        CodeModeProgress::Completed { call_id, phase: InnerCallPhase::Completed, .. }
            if call_id == "outer.0")),
        "{events:?}"
    );
    assert_eq!(
        done.details.as_ref().unwrap()["inner_calls"][0]["phase"],
        "completed"
    );
}

#[tokio::test]
async fn a_panicking_child_publishes_one_failed_inner_completion() {
    let host = Arc::new(RecordingHost::default());
    let registry = registry(&host);
    let run = RunCodeTool::new(
        Arc::clone(&registry),
        Arc::new(CodeCells::new()),
        "s".to_owned(),
    );
    let (mut out, mut updates) = ToolStream::channel();
    let result = run
        .execute_dyn(
            json!({"script": "call(\"agent\", #{ agent: \"scout\", prompt: \"panic\" });"}),
            &ToolCtx::new(std::env::temp_dir()).with_call_id("outer"),
            &mut out,
        )
        .await
        .unwrap();
    assert!(result.is_error);
    drop(out);
    let completed: Vec<_> = events(updates.drain().await)
        .into_iter()
        .filter(|event| matches!(event, CodeModeProgress::Completed { .. }))
        .collect();
    assert_eq!(
        completed,
        [CodeModeProgress::Completed {
            call_id: "outer.0".to_owned(),
            name: "agent".to_owned(),
            phase: InnerCallPhase::Failed,
            is_error: true,
        }]
    );
    assert_eq!(result.details.unwrap()["inner_calls"][0]["phase"], "failed");
}

#[tokio::test]
async fn cancelling_the_parent_publishes_one_cancelled_inner_completion_before_returning() {
    let host = Arc::new(RecordingHost::default());
    let registry = registry(&host);
    let cells = Arc::new(CodeCells::new());
    let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
    let ctx = ToolCtx::new(std::env::temp_dir()).with_call_id("outer");
    let cancel = ctx.cancel.clone();
    let (mut out, mut updates) = ToolStream::channel();
    let task = tokio::spawn(async move {
        let _registry = registry;
        run.execute_dyn(
            json!({"script": "call(\"agent\", #{ agent: \"scout\", prompt: \"cancel\" });"}),
            &ctx,
            &mut out,
        )
        .await
    });
    let mut items = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(item) = updates.recv().await {
            let started = matches!(&item, ToolStreamItem::Progress(update)
                if matches!(CodeModeProgress::parse(&update.message), Some(CodeModeProgress::Progress { .. })));
            items.push(item);
            if started { cancel.cancel(); break; }
        }
        items.extend(updates.drain().await);
    }).await.unwrap();
    assert!(task.await.unwrap().is_err());
    let completed: Vec<_> = events(items)
        .into_iter()
        .filter(|event| matches!(event, CodeModeProgress::Completed { .. }))
        .collect();
    assert_eq!(
        completed,
        [CodeModeProgress::Completed {
            call_id: "outer.0".to_owned(),
            name: "agent".to_owned(),
            phase: InnerCallPhase::Cancelled,
            is_error: true,
        }]
    );
    let cell = cells.get("s", "cell-0").unwrap();
    assert_eq!(cell.inner_calls()[0].phase, Some(InnerCallPhase::Cancelled));
}

#[test]
fn old_inner_call_summaries_and_pipe_containing_messages_remain_compatible() {
    let old = json!({"seq": 0, "name": "agent", "target": "scout", "is_error": false});
    let summary: InnerCallSummary = serde_json::from_value(old.clone()).unwrap();
    assert!(summary.call_id.is_none() && summary.phase.is_none());
    assert_eq!(serde_json::to_value(summary).unwrap(), old);
    let event = CodeModeProgress::Progress {
        call_id: "id|with\nseparator.0".to_owned(),
        name: "agent".to_owned(),
        message: "agent|scout|step|a|b\nline".to_owned(),
    };
    assert_eq!(CodeModeProgress::parse(&event.encode()), Some(event));
    assert!(CodeModeProgress::parse("agent|scout|step|legacy").is_none());
    assert!(CodeModeProgress::parse("code_mode|broken JSON").is_none());
}
