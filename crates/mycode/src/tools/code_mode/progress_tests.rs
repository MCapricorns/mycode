//! Code-mode child identity, completion, and progress continuation tests.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{CodeCells, CodeModeProgress, InnerCallPhase, RunCodeTool, WaitCodeTool};
use crate::tools::builtin::{AgentHost, AgentTool, SubagentRequest};
use crate::tools::{ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolStream, ToolStreamItem};

#[derive(Default)]
struct RecordingHost {
    calls: Mutex<Vec<String>>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
    resume_delayed: tokio::sync::Notify,
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
                        crate::tools::tool::INTERRUPTED_NOTE,
                    )))
                }
                "delayed" => {
                    self.resume_delayed.notified().await;
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
let first = call("agent", { agent: "scout", prompt: "cancel" });
if (!first.is_error) { throw "first child should have been cancelled"; }
let second = call("agent", { agent: "scout", prompt: "success" });
if (second.is_error) { throw "second child should finish"; }
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
                && message.starts_with("agent|")
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
    run.execute_dyn(
        json!({"script": "call(\"agent\", { agent: \"scout\", prompt: \"delayed\" });", "yield_time_ms": 0}),
        &ctx, &mut ToolStream::closed(),
    ).await.unwrap();
    let wait = WaitCodeTool::new(Arc::clone(&cells), "s".to_owned());
    let (mut out, mut updates) = ToolStream::channel();
    let wait_ctx = ToolCtx::new(std::env::temp_dir()).with_call_id("waiter");
    tokio::time::timeout(Duration::from_secs(5), async {
        let (done, ()) = tokio::join!(
            wait.execute_dyn(json!({"cell_id": "cell-0"}), &wait_ctx, &mut out),
            async {
                // Both futures run in this task: the wait has subscribed before
                // its initial status can be received here and the child released.
                updates.recv().await.expect("wait_code started waiting");
                host.resume_delayed.notify_one();
            },
        );
        done.unwrap();
    })
    .await
    .expect("wait_code forwards the released child's progress");
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
            json!({"script": "call(\"agent\", { agent: \"scout\", prompt: \"cancel\" });"}),
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
