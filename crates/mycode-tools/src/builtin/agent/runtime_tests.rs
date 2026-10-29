//! Fake-host lifecycle, scheduling, scope and acceptance regression tests.
#[path = "rework_tests.rs"]
mod rework_tests;
use super::*;
use crate::{ToolCtx, ToolDyn, ToolRegistry, ToolStreamItem};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
struct Call {
    request: SubagentRequest,
    id: String,
}
struct FakeHost {
    calls: Mutex<Vec<Call>>,
    disabled: Mutex<BTreeSet<String>>,
    outputs: Mutex<BTreeMap<String, String>>,
    failures: Mutex<BTreeSet<String>>,
    hold_nodes: Mutex<BTreeSet<String>>,
    gated_nodes: Mutex<BTreeSet<String>>,
    gate: tokio::sync::Semaphore,
    settle: Notify,
    steering: Mutex<Vec<String>>,
    steered: Notify,
    verdict: Mutex<String>,
}
impl Default for FakeHost {
    fn default() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            disabled: Mutex::new(BTreeSet::new()),
            outputs: Mutex::new(BTreeMap::new()),
            failures: Mutex::new(BTreeSet::new()),
            hold_nodes: Mutex::new(BTreeSet::new()),
            gated_nodes: Mutex::new(BTreeSet::new()),
            gate: tokio::sync::Semaphore::new(0),
            settle: Notify::new(),
            steering: Mutex::new(vec![]),
            steered: Notify::new(),
            verdict: Mutex::new("Independent evidence verified.\nWORKFLOW_ACCEPTANCE: PASS".into()),
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
                Ok(matches!(role, "artisan" | "custom-writer"))
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
            self.calls.lock().push(Call {
                request: request.clone(),
                id: call_id.into(),
            });
            progress.progress(format!(
                "agent|{}|queued|{}",
                request.agent, request.description
            ));
            progress.progress(format!("agent|{}|step|inspecting evidence", request.agent));
            let hold =
                self.hold_nodes.lock().contains(&request.description) || request.prompt == "hold";
            if hold {
                cancel.cancelled().await;
                self.settle.notified().await;
                return Err(ToolError::Execution(format!(
                    "subagent {} cancelled{}",
                    request.agent,
                    crate::tool::INTERRUPTED_NOTE
                )));
            }
            let gated = self.gated_nodes.lock().contains(&request.description);
            if gated {
                tokio::select! {
                    permit = self.gate.acquire() => permit.unwrap().forget(),
                    () = cancel.cancelled() => return Err(ToolError::Execution(format!("gated child cancelled{}", crate::tool::INTERRUPTED_NOTE))),
                }
            }
            if request.prompt == "steer" {
                self.steered.notified().await;
                return Ok(self.steering.lock().join("\n"));
            }
            if self.failures.lock().contains(&request.description) {
                return Err(ToolError::Execution(
                    "worker failed despite a possible PASS claim".into(),
                ));
            }
            if request.description == "workflow-acceptance" {
                return Ok(self.verdict.lock().clone());
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
    fn workflow(&self) -> AgentWorkflowTool {
        AgentWorkflowTool::new(self.host.clone(), self.runtime.clone())
    }
    fn runs(&self) -> Vec<AgentRunSnapshot> {
        self.runtime
            .snapshots(None)
            .unwrap()
            .into_iter()
            .filter_map(|e| {
                if let AgentStateEvent::Run(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect()
    }
    fn workflows(&self) -> Vec<AgentWorkflowSnapshot> {
        self.runtime
            .snapshots(None)
            .unwrap()
            .into_iter()
            .filter_map(|e| {
                if let AgentStateEvent::Workflow(w) = e {
                    Some(w)
                } else {
                    None
                }
            })
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
            if let mycode_providers::message::ContentBlock::Text(t) = b {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
async fn workflow(f: &Fixture, name: &str, inputs: Value) -> Result<ToolResult, ToolError> {
    f.workflow()
        .execute_dyn(
            json!({"name":name,"inputs":inputs}),
            &f.ctx("workflow-call"),
            &mut ToolStream::closed(),
        )
        .await
}
fn migration_inputs() -> Value {
    json!({"TARGET":"new interface","CONTRACT":"one shared contract","CORE_CHANGE":"change core","ADAPTER_CHANGE":"change adapters","CORE_SCOPE":"src/core.rs","CORE_TEST_SCOPE":"tests/core.rs","ADAPTER_SCOPE":"src/adapter.rs","ADAPTER_TEST_SCOPE":"tests/adapter.rs"})
}

#[tokio::test]
async fn old_agent_api_retains_bounded_unicode_results_and_sanitized_full_artifact() {
    let f = Fixture::new();
    let full = format!(
        "api_key=never-retain-this\n{}",
        "证据".repeat(MAX_AGENT_ANSWER_CHARS)
    );
    f.host.outputs.lock().insert("old".into(), full.clone());
    let (mut stream, mut updates) = ToolStream::channel();
    let result = f
        .agent()
        .execute_dyn(
            json!({"agent":"scout","prompt":"old compatible prompt","description":"old"}),
            &f.ctx("actual-inner-call"),
            &mut stream,
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(text(&result).contains("truncated"));
    let runs = f.runs();
    let r = &runs[0];
    assert_eq!(r.call_id, "actual-inner-call");
    assert_eq!(r.status, AgentRunStatus::Completed);
    assert!(r.finished_ms.is_some());
    let artifact = tokio::fs::read_to_string(r.artifact_path.as_ref().unwrap())
        .await
        .unwrap();
    assert!(!artifact.contains("never-retain-this"));
    assert!(artifact.ends_with(&"证据".repeat(MAX_AGENT_ANSWER_CHARS)));
    assert!(artifact.len() > text(&result).len());
    assert_eq!(
        result.details.as_ref().unwrap()["agent_states"][0]["kind"],
        "run"
    );
    let mut states = vec![];
    while let Some(item) = updates.try_recv() {
        if let ToolStreamItem::Progress(p) = item
            && let Some(json) = p.message.strip_prefix(AGENT_STATE_PREFIX)
        {
            states.push(serde_json::from_str::<AgentStateEvent>(json).unwrap());
        }
    }
    assert!(
        states
            .iter()
            .any(|s| matches!(s,AgentStateEvent::Run(r) if r.status==AgentRunStatus::Settling))
    );
    assert!(
        matches!(states.last(),Some(AgentStateEvent::Run(r)) if r.status==AgentRunStatus::Completed)
    );
    assert_eq!(f.runtime.snapshots(Some(&r.id)).unwrap().len(), 1);
    assert!(
        AgentRuntime::new(f.root.join("other-session"))
            .snapshots(Some(&r.id))
            .is_err()
    );
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
        tool.execute_dyn(json!({"agent":"artisan","prompt":"hold","phase_id":"edit-one","scope":{"paths":["src"]}}),&ctx,&mut ToolStream::closed()).await
    });
    wait_for(|| f.host.calls.lock().len() == 1).await;
    let id = f.runs()[0].id.clone();
    f.runtime.stop(&id, &ToolStream::closed()).unwrap();
    assert_eq!(f.runs()[0].status, AgentRunStatus::Settling);
    let overlap=f.agent().execute_dyn(json!({"agent":"artisan","prompt":"another","scope":{"symbols":[{"path":"src/a.rs","name":"change"}]}}),&f.ctx("other-host"),&mut ToolStream::closed()).await;
    assert!(overlap.is_err());
    assert_eq!(f.runs().len(), 1);
    assert_eq!(f.host.calls.lock().len(), 1);
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
    assert!(
        f.agent()
            .execute_dyn(
                json!({"agent":"artisan","prompt":"duplicate","phase_id":"edit-one"}),
                &f.ctx("duplicate"),
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
async fn dropping_caller_cancels_owned_driver_but_retains_record_and_waits_for_settlement() {
    let f = Fixture::new();
    let tool = f.agent();
    let ctx = f.ctx("dropped-host");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"agent":"artisan","prompt":"hold","scope":{"paths":["src/a.rs"]}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| !f.host.calls.lock().is_empty()).await;
    task.abort();
    let _ = task.await;
    wait_for(|| f.runs()[0].status == AgentRunStatus::Settling).await;
    assert!(f.runs()[0].finished_ms.is_none());
    f.host.settle.notify_one();
    wait_for(|| f.runs()[0].status == AgentRunStatus::Cancelled).await;
    assert!(f.runs()[0].artifact_path.is_some());
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
    let unsupported = messages
        .execute_dyn(
            json!({"id":id,"message":"continue","mode":"session"}),
            &f.ctx("unsupported"),
            &mut ToolStream::closed(),
        )
        .await;
    assert!(unsupported.is_err());
    assert_eq!(f.runs().len(), 1);
    messages
        .execute_dyn(
            json!({"id":id,"message":"answer the follow-up"}),
            &f.ctx("continuation-host"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert_eq!(f.runs().len(), 2);
    let calls = f.host.calls.lock();
    assert_eq!(calls[1].id, "continuation-host");
    assert!(
        calls[1]
            .request
            .prompt
            .contains("NEW run using summarized context only")
    );
    assert!(calls[1].request.prompt.contains("caller too"));
}

#[tokio::test]
async fn unsupported_host_declines_writer_reopen_before_allocating_a_fresh_head() {
    let f = Fixture::new();
    f.agent()
        .execute_dyn(
            json!({"agent":"artisan","prompt":"done","isolation":"worktree"}),
            &f.ctx("worktree"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    let id = f.runs()[0].id.clone();
    let result = AgentMessageTool::new(f.host.clone(), f.runtime.clone())
        .execute_dyn(
            json!({"id":id,"message":"continue"}),
            &f.ctx("new"),
            &mut ToolStream::closed(),
        )
        .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("safely reopen retained worktrees")
    );
    assert_eq!(f.host.calls.lock().len(), 1);
}

#[tokio::test]
async fn disabled_roles_and_disabled_acceptance_fail_before_any_allocation() {
    let f = Fixture::new();
    f.host.disabled.lock().insert("scout".into());
    assert!(
        f.agent()
            .execute_dyn(
                json!({"agent":"scout","prompt":"no"}),
                &f.ctx("disabled"),
                &mut ToolStream::closed()
            )
            .await
            .is_err()
    );
    assert!(f.runs().is_empty());
    f.host.disabled.lock().clear();
    f.host.disabled.lock().insert("sentinel".into());
    assert!(
        workflow(&f, "research", json!({"QUESTION":"question"}))
            .await
            .is_err()
    );
    assert!(f.runs().is_empty() && f.workflows().is_empty() && f.host.calls.lock().is_empty());
    assert!(
        workflow(&f, "research", json!({"QUSETION":"typo"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn research_runs_dependencies_then_independent_acceptance_and_retains_deliverable() {
    let f = Fixture::new();
    let result = workflow(
        &f,
        "research",
        json!({"QUESTION":"Which interface is supported?"}),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    let calls = f.host.calls.lock();
    let names: Vec<_> = calls
        .iter()
        .map(|c| c.request.description.as_str())
        .collect();
    assert_eq!(&names[3..], ["synthesis", "workflow-acceptance"]);
    assert!(calls[3].request.prompt.contains("primary evidence"));
    assert!(
        calls[4]
            .request
            .prompt
            .contains("independent acceptance officer")
    );
    let w = &f.workflows()[0];
    assert_eq!(w.status, AgentWorkflowStatus::Completed);
    assert_eq!(w.acceptance.as_deref(), Some("PASS"));
    assert!(w.result.as_ref().unwrap().contains("synthesis"));
    for run in f.runs() {
        assert!(calls.iter().any(|c| c.id == run.call_id));
        assert_eq!(run.workflow_id.as_ref(), Some(&w.id));
    }
    assert!(
        result.details.as_ref().unwrap()["agent_states"]
            .as_array()
            .unwrap()
            .len()
            >= 6
    );
}

#[tokio::test]
async fn dependency_failure_blocks_synthesis_and_acceptance_even_if_host_would_say_pass() {
    let f = Fixture::new();
    f.host.failures.lock().insert("primary".into());
    assert!(
        workflow(&f, "research", json!({"QUESTION":"question"}))
            .await
            .unwrap()
            .is_error
    );
    let w = &f.workflows()[0];
    assert_eq!(w.status, AgentWorkflowStatus::Failed);
    assert!(w.acceptance.is_none());
    assert!(
        w.nodes
            .iter()
            .filter(|n| ["synthesis", "workflow-acceptance"].contains(&n.id.as_str()))
            .all(|n| n.status == AgentRunStatus::Blocked)
    );
    assert_eq!(f.host.calls.lock().len(), 3);
}

#[tokio::test]
async fn acceptance_requires_unique_exact_terminal_pass_and_cannot_clear_failed_work() {
    for verdict in [
        "no marker",
        "WORKFLOW_ACCEPTANCE: FAIL",
        "WORKFLOW_ACCEPTANCE: PASS\nmore",
        "WORKFLOW_ACCEPTANCE: PASS\nWORKFLOW_ACCEPTANCE: PASS",
        "quoted WORKFLOW_ACCEPTANCE: PASS",
    ] {
        let f = Fixture::new();
        *f.host.verdict.lock() = verdict.into();
        let r = workflow(&f, "code-review", json!({"TARGET":"current diff"}))
            .await
            .unwrap();
        assert!(r.is_error, "{verdict}");
        assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Failed);
    }
    let f = Fixture::new();
    f.host.failures.lock().insert("workflow-acceptance".into());
    assert!(
        workflow(&f, "code-review", json!({"TARGET":"diff"}))
            .await
            .unwrap()
            .is_error
    );
    let f = Fixture::new();
    *f.host.verdict.lock() = format!("{}\nWORKFLOW_ACCEPTANCE: PASS", "evidence ".repeat(4000));
    assert!(
        !workflow(
            &f,
            "code-review",
            json!({"TARGET":"diff with confirmed defects"})
        )
        .await
        .unwrap()
        .is_error,
        "verdict must be parsed from full artifact, not clipped snapshot"
    );
}

#[tokio::test]
async fn migration_waits_for_manual_integration_then_runs_shared_gate_and_acceptance() {
    let f = Fixture::new();
    let first = workflow(&f, "migration", migration_inputs()).await.unwrap();
    assert!(!first.is_error);
    let w = f.workflows()[0].clone();
    assert_eq!(w.status, AgentWorkflowStatus::WaitingIntegration);
    assert_eq!(f.host.calls.lock().len(), 2);
    assert!(w.acceptance.is_none());
    assert!(f.agent().execute_dyn(json!({"agent":"artisan","prompt":"collide with pending gate","scope":{"paths":["src/core.rs"]}}),&f.ctx("outside"),&mut ToolStream::closed()).await.is_err());
    let resumed = f
        .workflow()
        .execute_dyn(
            json!({"action":"resume","id":w.id}),
            &f.ctx("resume-host"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!resumed.is_error);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
    let calls = f.host.calls.lock();
    assert_eq!(calls[2].request.description, "gate");
    assert_eq!(calls[2].request.isolation.as_deref(), Some("shared"));
    assert!(calls[2].request.prompt.contains("actual parent cwd"));
    assert!(calls[3].request.prompt.contains(f.root.to_str().unwrap()));
    assert_eq!(
        calls.iter().map(|c| &c.id).collect::<BTreeSet<_>>().len(),
        4
    );
}

#[tokio::test]
async fn overlapping_workflow_slices_and_pending_workflow_claims_are_refused_before_allocation() {
    let f = Fixture::new();
    let mut inputs = migration_inputs();
    inputs["ADAPTER_SCOPE"] = json!("src/core.rs");
    assert!(workflow(&f, "migration", inputs).await.is_err());
    assert!(f.workflows().is_empty() && f.runs().is_empty());
    workflow(&f, "migration", migration_inputs()).await.unwrap();
    let mut inputs = migration_inputs();
    inputs["TARGET"] = json!("other target");
    assert!(workflow(&f, "migration", inputs).await.is_err());
    assert_eq!(f.workflows().len(), 1);
    let w = f.workflows()[0].clone();
    f.runtime.stop(&w.id, &ToolStream::closed()).unwrap();
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Cancelled);
    assert!(
        f.workflow()
            .execute_dyn(
                json!({"action":"accept","id":w.id}),
                &f.ctx("resume"),
                &mut ToolStream::closed()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn workflow_cancellation_stops_descendants_and_never_starts_dependents() {
    let f = Fixture::new();
    f.host
        .hold_nodes
        .lock()
        .extend(["primary".into(), "practice".into(), "codebase".into()]);
    let tool = f.workflow();
    let ctx = f.ctx("parent");
    let cancel = ctx.cancel.clone();
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"name":"research","inputs":{"QUESTION":"question"}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| f.host.calls.lock().len() == 3).await;
    cancel.cancel();
    for _ in 0..3 {
        f.host.settle.notify_one();
        tokio::task::yield_now().await;
    }
    assert!(task.await.unwrap().unwrap().is_error);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Cancelled);
    assert_eq!(f.host.calls.lock().len(), 3);
    assert!(
        f.runs()
            .iter()
            .all(|r| r.status == AgentRunStatus::Cancelled)
    );
}

#[tokio::test]
async fn retention_evicts_only_terminal_records_and_preserves_overflow_artifacts() {
    let f = Fixture::new();
    for i in 0..=super::runtime::MAX_RETAINED_AGENT_RUNS {
        let result = f
            .agent()
            .execute_dyn(
                json!({"agent":"scout","prompt":"bounded"}),
                &f.ctx(&format!("host-{i}")),
                &mut ToolStream::closed(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
    }
    assert_eq!(f.runs().len(), super::runtime::MAX_RETAINED_AGENT_RUNS);
    assert_eq!(
        std::fs::read_dir(f.root.join("artifacts")).unwrap().count(),
        super::runtime::MAX_RETAINED_AGENT_RUNS + 1
    );
}

#[tokio::test]
async fn codemode_keeps_real_child_identity_typed_progress_and_reload_snapshots() {
    let f = Fixture::new();
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.agent()));
    let tool = crate::RunCodeTool::new(
        registry.clone(),
        Arc::new(crate::CodeCells::new()),
        "session".into(),
    );
    let (mut stream, mut updates) = ToolStream::channel();
    let result=tool.execute_dyn(json!({"script":"let answer = call('agent', {agent:'scout', prompt:'evidence'}); if(answer.is_error) throw 'failed'; complete('done');"}),&f.ctx("outer"),&mut stream).await.unwrap();
    assert!(!result.is_error);
    assert_eq!(f.runs()[0].call_id, "outer.0");
    assert_eq!(f.host.calls.lock()[0].id, "outer.0");
    let mut seen = false;
    while let Some(item) = updates.try_recv() {
        if let ToolStreamItem::Progress(p) = item
            && let Some(crate::CodeModeProgress::Progress { message, .. }) =
                crate::CodeModeProgress::parse(&p.message)
            && let Some(json) = message.strip_prefix(AGENT_STATE_PREFIX)
        {
            let event: AgentStateEvent = serde_json::from_str(json).unwrap();
            if let AgentStateEvent::Run(r) = event {
                assert_eq!(r.call_id, "outer.0");
                seen = true;
            }
        }
    }
    assert!(seen);
    assert_eq!(
        result.details.as_ref().unwrap()["agent_states"][0]["snapshot"]["call_id"],
        "outer.0"
    );
}

#[tokio::test]
async fn codemode_cancelled_history_does_not_cancel_successful_status_queries() {
    let f = Fixture::new();
    f.host.gated_nodes.lock().insert("primary".into());
    let tool = f.workflow();
    let ctx = f.ctx("cancelled-history");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"name":"research","inputs":{"QUESTION":"question"}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| {
        f.runs()
            .iter()
            .any(|run| run.node_id.as_deref() == Some("primary"))
    })
    .await;
    let id = f.workflows()[0].id.clone();
    f.runtime.stop(&id, &ToolStream::closed()).unwrap();
    assert!(task.await.unwrap().unwrap().is_error);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Cancelled);
    let cancelled = f
        .runs()
        .into_iter()
        .find(|run| run.status == AgentRunStatus::Cancelled)
        .unwrap();
    f.host.gated_nodes.lock().clear();
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.agent()));
    registry.register(Arc::new(AgentStatusTool::new(f.runtime.clone())));
    registry.register(Arc::new(AgentStopTool::new(f.runtime.clone())));
    registry.register(Arc::new(f.workflow()));
    let run = crate::RunCodeTool::new(
        registry.clone(),
        Arc::new(crate::CodeCells::new()),
        "history-session".into(),
    );
    let script = format!(
        "for (let query of [{{}},{{id:'{}'}}]) {{ if(call('agent_status',query).is_error) throw 'status failed'; }}
         for (let query of [{{action:'status'}},{{action:'status',id:'{id}'}},{{action:'stop',id:'{id}'}}]) {{ if(call('agent_workflow',query).is_error) throw 'workflow query failed'; }}
         if(call('agent_stop',{{id:'{}'}}).is_error) throw 'stop failed';
         if(call('agent',{{agent:'scout',prompt:'fresh evidence'}}).is_error) throw 'agent failed';
         if(call('agent_workflow',{{name:'research',inputs:{{QUESTION:'fresh question'}}}}).is_error) throw 'fresh workflow failed';
         complete('done');",
        cancelled.id, cancelled.id,
    );
    let (mut stream, mut updates) = ToolStream::channel();
    let result = run
        .execute_dyn(
            json!({"script":script}),
            &f.ctx("history-queries"),
            &mut stream,
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    let details = result.details.unwrap();
    let calls = details["inner_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 8);
    for call in calls {
        assert_eq!(call["phase"], "completed", "{call}");
        assert_eq!(call["is_error"], false, "{call}");
    }
    let mut completed = 0;
    while let Some(item) = updates.try_recv() {
        if let ToolStreamItem::Progress(update) = item
            && let Some(crate::CodeModeProgress::Completed {
                phase, is_error, ..
            }) = crate::CodeModeProgress::parse(&update.message)
        {
            assert_eq!(phase, crate::InnerCallPhase::Completed);
            assert!(!is_error);
            completed += 1;
        }
    }
    assert_eq!(completed, calls.len());
    let retained: Vec<AgentStateEvent> =
        serde_json::from_value(details["agent_states"].clone()).unwrap();
    assert!(
        retained
            .iter()
            .any(|event| matches!(event, AgentStateEvent::Run(run) if run == &cancelled))
    );
    assert!(retained.iter().any(|event| matches!(event, AgentStateEvent::Workflow(workflow) if workflow.id == id && workflow.status == AgentWorkflowStatus::Cancelled)));
}

#[tokio::test]
async fn codemode_failed_rework_ignores_cancelled_runs_from_earlier_rounds() {
    let f = Fixture::new();
    f.host.gated_nodes.lock().insert("primary".into());
    let tool = f.workflow();
    let ctx = f.ctx("failed-history");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"name":"research","inputs":{"QUESTION":"question"}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| {
        f.runs()
            .iter()
            .any(|run| run.node_id.as_deref() == Some("primary"))
    })
    .await;
    let primary = f
        .runs()
        .into_iter()
        .find(|run| run.node_id.as_deref() == Some("primary"))
        .unwrap();
    f.runtime.stop(&primary.id, &ToolStream::closed()).unwrap();
    assert!(task.await.unwrap().unwrap().is_error);
    let before = f.workflows()[0].clone();
    assert_eq!(before.status, AgentWorkflowStatus::Failed);
    let cancelled = f
        .runs()
        .into_iter()
        .find(|run| run.id == primary.id)
        .unwrap();
    assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.workflow()));
    let run = crate::RunCodeTool::new(
        registry.clone(),
        Arc::new(crate::CodeCells::new()),
        "history-rework".into(),
    );
    let script = format!(
        "let result=call('agent_workflow',{{action:'revise',id:'{}',node_id:'practice',feedback:'refresh experience evidence',expected_revision:0}}); if(!result.is_error) throw 'cancelled dependency still blocks acceptance'; complete('failed as expected');",
        before.id
    );
    let result = run
        .execute_dyn(
            json!({"script":script}),
            &f.ctx("fresh-round"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    let details = result.details.unwrap();
    assert_eq!(details["inner_calls"][0]["phase"], "failed");
    assert_eq!(details["inner_calls"][0]["is_error"], true);
    let after = f.workflows()[0].clone();
    assert_eq!(after.revision, 1);
    assert_eq!(after.status, AgentWorkflowStatus::Failed);
    assert!(f.runs().contains(&cancelled));
    assert_ne!(
        before
            .nodes
            .iter()
            .find(|node| node.id == "practice")
            .unwrap()
            .run_id,
        after
            .nodes
            .iter()
            .find(|node| node.id == "practice")
            .unwrap()
            .run_id
    );
}

#[tokio::test]
async fn codemode_current_workflow_stop_still_reports_cancelled() {
    let f = Fixture::new();
    f.host.gated_nodes.lock().insert("review".into());
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.workflow()));
    let run = crate::RunCodeTool::new(
        registry.clone(),
        Arc::new(crate::CodeCells::new()),
        "current-stop".into(),
    );
    let ctx = f.ctx("current-workflow");
    let cancel = ctx.cancel.clone();
    let task = tokio::spawn(async move {
        let _registry = registry;
        run.execute_dyn(json!({"script":"if(!call('agent_workflow',{name:'code-review',inputs:{TARGET:'diff'}}).is_error) throw 'expected cancellation'; complete('stopped');"}), &ctx, &mut ToolStream::closed()).await
    });
    wait_for(|| {
        f.runs()
            .iter()
            .any(|run| run.node_id.as_deref() == Some("review"))
    })
    .await;
    f.runtime
        .stop(&f.workflows()[0].id, &ToolStream::closed())
        .unwrap();
    let result = task.await.unwrap().unwrap();
    assert!(!result.is_error);
    let details = result.details.unwrap();
    assert_eq!(details["inner_calls"][0]["phase"], "cancelled");
    assert_eq!(details["inner_calls"][0]["is_error"], true);
    assert!(!cancel.is_cancelled());
}

#[tokio::test]
async fn queued_workflow_node_is_scheduled_once_while_independent_nodes_finish() {
    let f = Fixture::new();
    f.host.gated_nodes.lock().insert("primary".into());
    let tool = f.workflow();
    let ctx = f.ctx("queued-workflow");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"name":"research","inputs":{"QUESTION":"question"}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| {
        f.workflows().first().is_some_and(|w| {
            w.nodes
                .iter()
                .filter(|n| ["practice", "codebase"].contains(&n.id.as_str()))
                .all(|n| n.status == AgentRunStatus::Completed)
        })
    })
    .await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!task.is_finished());
    f.host.gate.add_permits(1);
    assert!(
        !task.await.unwrap().unwrap().is_error,
        "a host-queued run must not be re-admitted under its existing phase"
    );
    assert_eq!(
        f.host
            .calls
            .lock()
            .iter()
            .filter(|c| c.request.description == "primary")
            .count(),
        1
    );
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
}

#[tokio::test]
async fn native_workflow_execute_waits_for_work_and_independent_acceptance() {
    let f = Fixture::new();
    f.host
        .gated_nodes
        .lock()
        .extend(["review".into(), "workflow-acceptance".into()]);
    let tool = f.workflow();
    let ctx = f.ctx("native-workflow");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"name":"code-review","inputs":{"TARGET":"current diff"}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| f.host.calls.lock().len() == 1).await;
    assert!(
        !task.is_finished(),
        "native execute must not return a startup acknowledgment"
    );
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Running);
    f.host.gate.add_permits(1);
    wait_for(|| f.host.calls.lock().len() == 2).await;
    assert!(
        !task.is_finished(),
        "native execute must also await independent acceptance"
    );
    assert!(f.workflows()[0].acceptance.is_none());
    f.host.gate.add_permits(1);
    assert!(!task.await.unwrap().unwrap().is_error);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
    assert_eq!(f.workflows()[0].acceptance.as_deref(), Some("PASS"));
}

#[tokio::test]
async fn workflow_survives_codemode_yield_and_wait_code_collects_same_cell_to_acceptance() {
    let f = Fixture::new();
    f.host.gated_nodes.lock().insert("review".into());
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.workflow()));
    let cells = Arc::new(crate::CodeCells::new());
    let run = crate::RunCodeTool::new(registry.clone(), cells.clone(), "workflow-session".into());
    let wait = crate::WaitCodeTool::new(cells, "workflow-session".into());
    let ctx = f.ctx("outer-workflow");
    let first=run.execute_dyn(json!({"script":"let result=call('agent_workflow',{name:'code-review',inputs:{TARGET:'current diff'}}); if(result.is_error) throw 'workflow failed'; complete('workflow accepted');","yield_time_ms":20}),&ctx,&mut ToolStream::closed()).await.unwrap();
    assert!(!first.is_error);
    assert_eq!(first.details.as_ref().unwrap()["phase"], "yielded");
    assert!(!text(&first).contains("workflow accepted"));
    wait_for(|| f.host.calls.lock().len() == 1).await;
    let id = f.workflows()[0].id.clone();
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Running);
    assert!(f.workflows()[0].acceptance.is_none());
    assert!(!ctx.cancel.is_cancelled());
    let cell_id = first.details.as_ref().unwrap()["cell_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.host.gate.add_permits(1);
    let done = wait
        .execute_dyn(
            json!({"cell_id":cell_id,"yield_time_ms":1000}),
            &f.ctx("wait-workflow"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!done.is_error);
    assert_eq!(done.details.as_ref().unwrap()["cell_id"], cell_id);
    assert_eq!(done.details.as_ref().unwrap()["phase"], "completed");
    assert!(text(&done).contains("workflow accepted"));
    assert_eq!(f.workflows()[0].id, id);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
    assert_eq!(f.workflows()[0].acceptance.as_deref(), Some("PASS"));
    assert!(!ctx.cancel.is_cancelled());
    let calls = f.host.calls.lock();
    for r in f.runs() {
        assert!(calls.iter().any(|c| c.id == r.call_id));
        assert!(r.call_id.starts_with("outer-workflow.0:"));
        assert_eq!(r.status, AgentRunStatus::Completed);
    }
    let retained: Vec<AgentStateEvent> =
        serde_json::from_value(done.details.as_ref().unwrap()["agent_states"].clone()).unwrap();
    assert!(retained.iter().any(|e|matches!(e,AgentStateEvent::Workflow(w) if w.id==id && w.status==AgentWorkflowStatus::Completed)));
}

struct NativeHost {
    entered: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
impl AgentHost for NativeHost {
    fn run_subagent<'a>(
        &'a self,
        _request: SubagentRequest,
        _progress: &'a ToolStream,
        cancel: &'a CancellationToken,
        _call_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let receiver = self.release.lock().take().unwrap();
            let entered = self.entered.clone();
            let finished = self.finished.clone();
            let worker = crate::builtin::blocking::run_blocking_supervised(
                "native writer",
                cancel,
                move |_| {
                    entered.store(true, Ordering::Release);
                    receiver.recv().unwrap();
                    finished.store(true, Ordering::Release);
                    Ok("native settled".to_owned())
                },
            );
            tokio::pin!(worker);
            tokio::select! {result=&mut worker=>result,()=cancel.cancelled()=>Err(ToolError::Execution("native host cancelled".into()))}
        })
    }
}
#[tokio::test]
async fn cancelled_native_supervisor_must_join_before_scope_is_released() {
    let f = Fixture::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let entered = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let host = Arc::new(NativeHost {
        entered: entered.clone(),
        finished: finished.clone(),
        release: Mutex::new(Some(release_rx)),
    });
    let tool = AgentTool::new(host).with_runtime(f.runtime.clone());
    let ctx = f.ctx("native");
    let task = tokio::spawn(async move {
        tool.execute_dyn(
            json!({"agent":"artisan","prompt":"native","scope":{"paths":["src/native.rs"]}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
    });
    wait_for(|| entered.load(Ordering::Acquire)).await;
    let id = f.runs()[0].id.clone();
    f.runtime.stop(&id, &ToolStream::closed()).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(f.runs()[0].status, AgentRunStatus::Settling);
    assert!(!finished.load(Ordering::Acquire));
    assert!(
        f.agent()
            .execute_dyn(
                json!({"agent":"artisan","prompt":"overlap","scope":{"paths":["src/native.rs"]}}),
                &f.ctx("overlap"),
                &mut ToolStream::closed()
            )
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    assert!(task.await.unwrap().unwrap().is_error);
    assert!(finished.load(Ordering::Acquire));
    assert_eq!(f.runs()[0].status, AgentRunStatus::Cancelled);
}
