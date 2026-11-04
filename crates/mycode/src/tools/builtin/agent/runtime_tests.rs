//! Fake-host lifecycle, scope and continuation tests.
use super::*;
use crate::tools::{ToolCtx, ToolDyn, ToolRegistry};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
struct Call {
    id: String,
}
struct FakeHost {
    calls: Mutex<Vec<Call>>,
    disabled: Mutex<BTreeSet<String>>,
    writers: Mutex<BTreeSet<String>>,
    outputs: Mutex<BTreeMap<String, String>>,
    settle: Notify,
    steering: Mutex<Vec<String>>,
    steered: Notify,
}
impl Default for FakeHost {
    fn default() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            disabled: Mutex::new(BTreeSet::new()),
            writers: Mutex::new(["artisan".into(), "custom-writer".into()].into()),
            outputs: Mutex::new(BTreeMap::new()),
            settle: Notify::new(),
            steering: Mutex::new(vec![]),
            steered: Notify::new(),
        }
    }
}
impl AgentHost for FakeHost {
    fn validate_role<'a>(
        &'a self,
        role: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if self.disabled.lock().contains(role) {
                Err(ToolError::Execution("role disabled".into()))
            } else {
                Ok(self.writers.lock().contains(role))
            }
        })
    }
    fn steer_subagent(&self, _id: &str, message: &str) -> Result<(), ToolError> {
        self.steering.lock().push(message.into());
        self.steered.notify_one();
        Ok(())
    }
    fn run_subagent<'a>(
        &'a self,
        request: SubagentRequest,
        progress: &'a ToolStream,
        cancel: &'a CancellationToken,
        call_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().push(Call { id: call_id.into() });
            progress.progress(format!(
                "agent|{}|queued|{}",
                request.agent, request.description
            ));
            progress.progress(format!("agent|{}|step|inspecting evidence", request.agent));
            if request.prompt == "hold" {
                cancel.cancelled().await;
                self.settle.notified().await;
                return Err(ToolError::Execution(format!(
                    "subagent {} cancelled{}",
                    request.agent,
                    crate::tools::tool::INTERRUPTED_NOTE
                )));
            }
            if request.prompt == "steer" {
                self.steered.notified().await;
                return Ok(self.steering.lock().join("\n"));
            }
            Ok(self
                .outputs
                .lock()
                .get(&request.description)
                .cloned()
                .unwrap_or_else(|| format!("{} evidence: {}", request.description, request.prompt)))
        })
    }
}
struct Fixture {
    root: std::path::PathBuf,
    runtime: Arc<AgentRuntime>,
    host: Arc<FakeHost>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("mycode-agent-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            runtime: Arc::new(AgentRuntime::new(root.join("artifacts"))),
            root,
            host: Arc::new(FakeHost::default()),
        }
    }
    fn ctx(&self, id: &str) -> ToolCtx {
        ToolCtx::new(&self.root).with_call_id(id)
    }
    fn agent(&self) -> AgentTool {
        AgentTool::new(self.host.clone()).with_runtime(self.runtime.clone())
    }
    fn runs(&self) -> Vec<AgentRunSnapshot> {
        self.runtime
            .snapshots(None)
            .unwrap()
            .into_iter()
            .map(|AgentStateEvent::Run(r)| r)
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn wait_for(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("state transition timed out");
}
fn text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|b| {
            if let crate::core::message::ContentBlock::Text(t) = b {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn exact_scope_normalization_rejects_wildcards_duplicates_and_detects_path_symbol_overlap() {
    let cwd = std::env::temp_dir();
    let normalize = |value: Value| {
        serde_json::from_value::<AgentScope>(value)
            .unwrap()
            .normalize(&cwd)
    };
    for invalid in [
        json!({"paths":["src/*"]}),
        json!({"paths":["src/a?b"]}),
        json!({"paths":["src/a","src/./a"]}),
        json!({"symbols":[{"path":"src/a","name":"f"},{"path":"src/a","name":"f"}]}),
        json!({}),
    ] {
        assert!(normalize(invalid).is_err());
    }
    let dir = normalize(json!({"paths":["src"]})).unwrap();
    let file = normalize(json!({"paths":["src/a"]})).unwrap();
    let neighbor = normalize(json!({"paths":["src-other/a"]})).unwrap();
    let symbol = normalize(json!({"symbols":[{"path":"src/a","name":"f"}]})).unwrap();
    let other_symbol = normalize(json!({"symbols":[{"path":"src/a","name":"g"}]})).unwrap();
    assert!(dir.overlaps(&file) && file.overlaps(&symbol));
    assert!(!dir.overlaps(&neighbor));
    assert!(!symbol.overlaps(&other_symbol));
    for phase in ["", "space phase", "*", "-first"] {
        assert!(super::scope::validate_phase(Some(phase)).is_err());
    }
}

#[tokio::test]
async fn stopping_writer_holds_scope_until_settled_and_stopped_run_cannot_resume() {
    let f = Fixture::new();
    let tool = f.agent();
    let ctx = f.ctx("writer-host-id");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"agent":"artisan","prompt":"hold","phase_id":"edit-one","scope":{"paths":["src"]}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| f.host.calls.lock().len() == 1).await;
    let id = f.runs()[0].id.clone();
    f.runtime.stop(&id, &ToolStream::closed()).unwrap();
    assert_eq!(f.runs()[0].status, AgentRunStatus::Settling);
    let overlap = f
        .agent()
        .execute_dyn(
            json!({"agent":"artisan","prompt":"another","scope":{"symbols":[{"path":"src/a.rs","name":"change"}]}}),
            &f.ctx("other-host"),
            &mut ToolStream::closed(),
        )
        .await;
    assert!(overlap.is_err());
    f.host.settle.notify_one();
    assert!(task.await.unwrap().unwrap().is_error);
    assert_eq!(f.runs()[0].status, AgentRunStatus::Cancelled);
    let messages = AgentMessageTool::new(f.host.clone(), f.runtime.clone());
    assert!(
        messages
            .execute_dyn(
                json!({"id":id,"message":"resume"}),
                &f.ctx("message"),
                &mut ToolStream::closed()
            )
            .await
            .is_err()
    );
    let success = f
        .agent()
        .execute_dyn(
            json!({"agent":"artisan","prompt":"new phase","scope":{"paths":["src/a.rs"]}}),
            &f.ctx("new-host"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!success.is_error);
}

#[tokio::test]
async fn active_steering_and_summary_continuation_use_real_identity_and_a_new_run() {
    let f = Fixture::new();
    let tool = f.agent();
    let ctx = f.ctx("steer-host");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"agent":"scout","prompt":"steer"}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| !f.host.calls.lock().is_empty()).await;
    let id = f.runs()[0].id.clone();
    let messages = AgentMessageTool::new(f.host.clone(), f.runtime.clone());
    messages
        .execute_dyn(
            json!({"id":id,"message":"inspect the caller too"}),
            &f.ctx("message-host"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(text(&task.await.unwrap().unwrap()).contains("caller too"));
    messages
        .execute_dyn(
            json!({"id":id,"message":"answer the follow-up"}),
            &f.ctx("continuation-host"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert_eq!(f.runs().len(), 2);
}

#[tokio::test]
async fn codemode_keeps_real_child_identity_and_typed_progress() {
    let f = Fixture::new();
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.agent()));
    let tool = crate::tools::RunCodeTool::new(
        registry.clone(),
        Arc::new(crate::tools::CodeCells::new()),
        "session".into(),
    );
    let (mut stream, _updates) = ToolStream::channel();
    let result = tool
        .execute_dyn(
            json!({"script":"let answer = call('agent', {agent:'scout', prompt:'evidence'}); if(answer.is_error) throw 'failed'; complete('done');"}),
            &f.ctx("outer"),
            &mut stream,
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(f.runs()[0].call_id, "outer.0");
    assert_eq!(f.host.calls.lock()[0].id, "outer.0");
}
