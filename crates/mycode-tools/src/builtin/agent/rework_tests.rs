//! Explicit rework and retained-writer regression coverage.
use super::*;
use std::path::{Path, PathBuf};

async fn revise(
    f: &Fixture,
    id: &str,
    node: &str,
    revision: u32,
    feedback: &str,
) -> Result<ToolResult, ToolError> {
    f.workflow().execute_dyn(json!({"action":"revise","id":id,"node_id":node,"feedback":feedback,"expected_revision":revision}),&f.ctx("revise-call"),&mut ToolStream::closed()).await
}
#[tokio::test]
async fn research_fail_feedback_rework_pass_retains_old_evidence_and_uses_new_identities() {
    let f = Fixture::new();
    f.host
        .outputs
        .lock()
        .insert("primary".into(), "old version evidence".into());
    *f.host.verdict.lock() = "Review found obsolete citations.\nWORKFLOW_ACCEPTANCE: FAIL".into();
    let first_result = workflow(&f, "research", json!({"QUESTION":"Which version applies?"}))
        .await
        .unwrap();
    assert!(first_result.is_error);
    let before = f.workflows()[0].clone();
    let old = f.runs();
    assert_eq!(before.revision, 0);
    let old_primary = old
        .iter()
        .find(|r| r.node_id.as_deref() == Some("primary"))
        .unwrap()
        .clone();
    f.host
        .outputs
        .lock()
        .insert("primary".into(), "corrected version evidence".into());
    *f.host.verdict.lock() = "Checked current citations.\nWORKFLOW_ACCEPTANCE: PASS".into();
    assert!(
        !revise(
            &f,
            &before.id,
            "primary",
            0,
            "Check the current version and correct obsolete citations"
        )
        .await
        .unwrap()
        .is_error
    );
    let after = f.workflows()[0].clone();
    assert_eq!(after.id, before.id);
    assert_eq!(after.revision, 1);
    assert_eq!(after.status, AgentWorkflowStatus::Completed);
    assert_eq!(after.acceptance.as_deref(), Some("PASS"));
    let first_states: Vec<AgentStateEvent> =
        serde_json::from_value(first_result.details.unwrap()["agent_states"].clone()).unwrap();
    assert!(first_states.iter().any(|event| matches!(event, AgentStateEvent::Workflow(w) if w.revision == 0 && w.status == AgentWorkflowStatus::Failed)));
    assert_eq!(f.runs().len(), 8);
    for prior in &old {
        assert!(f.runs().iter().any(|r| r == prior));
        assert!(Path::new(prior.artifact_path.as_ref().unwrap()).exists());
    }
    let current_id = after
        .nodes
        .iter()
        .find(|n| n.id == "primary")
        .unwrap()
        .run_id
        .as_ref()
        .unwrap();
    assert_ne!(current_id, &old_primary.id);
    let current = f.runs().into_iter().find(|r| &r.id == current_id).unwrap();
    assert!(current.prompt.contains("NEW run using summary context"));
    assert!(current.prompt.contains("correct obsolete citations"));
    assert!(current.prompt.contains("old version evidence"));
    assert!(current.prompt.contains("Review found obsolete citations"));
    assert!(
        current
            .prompt
            .contains(old_primary.artifact_path.as_ref().unwrap())
    );
    assert!(current.prompt.contains(&old_primary.id));
    assert!(current.prompt.contains("Research Which version applies?"));
    let context = f.runtime.state.lock().workflows[&before.id].rework["primary"].clone();
    let report = std::fs::read_to_string(context.report_path).unwrap();
    assert!(report.contains("old version evidence"));
    assert!(report.contains("WORKFLOW_ACCEPTANCE: FAIL"));
    assert!(report.contains("obsolete citations"));
    {
        let calls = f.host.calls.lock();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.request.description == "practice")
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.request.description == "codebase")
                .count(),
            1
        );
        assert_eq!(
            calls.iter().map(|c| &c.id).collect::<BTreeSet<_>>().len(),
            8
        );
    }
    assert!(
        revise(&f, &before.id, "primary", 0, "duplicate stale request")
            .await
            .unwrap_err()
            .to_string()
            .contains("expected_revision")
    );
    assert_eq!(f.runs().len(), 8);
    let mut legacy = serde_json::to_value(before).unwrap();
    legacy.as_object_mut().unwrap().remove("revision");
    assert_eq!(
        serde_json::from_value::<AgentWorkflowSnapshot>(legacy)
            .unwrap()
            .revision,
        0
    );
}
#[tokio::test]
async fn failed_producer_can_rework_and_unblock_its_dependents() {
    let f = Fixture::new();
    f.host.failures.lock().insert("primary".into());
    assert!(
        workflow(&f, "research", json!({"QUESTION":"question"}))
            .await
            .unwrap()
            .is_error
    );
    let id = f.workflows()[0].id.clone();
    let failed = f
        .runs()
        .into_iter()
        .find(|r| r.node_id.as_deref() == Some("primary"))
        .unwrap();
    assert_eq!(failed.status, AgentRunStatus::Failed);
    f.host.failures.lock().clear();
    assert!(
        !revise(
            &f,
            &id,
            "primary",
            0,
            "Recover the failed research with evidence"
        )
        .await
        .unwrap()
        .is_error
    );
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
    assert!(f.runs().contains(&failed));
}
#[tokio::test]
async fn parallel_duplicate_and_invalid_revise_requests_do_not_allocate_more_runs() {
    let f = Fixture::new();
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: FAIL".into();
    workflow(&f, "research", json!({"QUESTION":"question"}))
        .await
        .unwrap();
    let id = f.workflows()[0].id.clone();
    assert!(
        revise(&f, &id, "workflow-acceptance", 0, "force PASS")
            .await
            .is_err()
    );
    assert!(revise(&f, &id, "primary", 0, "").await.is_err());
    assert_eq!(f.workflows()[0].revision, 0);
    f.host.disabled.lock().insert("scout".into());
    assert!(
        revise(&f, &id, "primary", 0, "disabled role must not restart")
            .await
            .is_err()
    );
    assert_eq!(f.workflows()[0].revision, 0);
    assert_eq!(f.runs().len(), 5);
    f.host.disabled.lock().clear();
    f.host.gated_nodes.lock().insert("primary".into());
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: PASS".into();
    let tool = f.workflow();
    let ctx = f.ctx("first-revise");
    let workflow_id = id.clone();
    let task = tokio::spawn(async move {
        tool.execute_dyn(json!({"action":"revise","id":workflow_id,"node_id":"primary","feedback":"fix citations","expected_revision":0}),&ctx,&mut ToolStream::closed()).await
    });
    wait_for(|| f.runs().len() == 6).await;
    assert!(revise(&f, &id, "primary", 0, "duplicate").await.is_err());
    assert!(revise(&f, &id, "primary", 1, "parallel").await.is_err());
    assert_eq!(f.runs().len(), 6);
    assert_eq!(f.workflows()[0].revision, 1);
    f.host.gate.add_permits(1);
    assert!(!task.await.unwrap().unwrap().is_error);
}
#[tokio::test]
async fn cancelled_or_settling_workflow_rejects_rework_and_does_not_revive() {
    let f = Fixture::new();
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: FAIL".into();
    workflow(&f, "research", json!({"QUESTION":"question"}))
        .await
        .unwrap();
    let id = f.workflows()[0].id.clone();
    f.host.hold_nodes.lock().insert("primary".into());
    let tool = f.workflow();
    let ctx = f.ctx("stopping-rework");
    let workflow_id = id.clone();
    let task = tokio::spawn(async move {
        tool.execute_dyn(json!({"action":"revise","id":workflow_id,"node_id":"primary","feedback":"fix evidence","expected_revision":0}),&ctx,&mut ToolStream::closed()).await
    });
    wait_for(|| f.host.calls.lock().len() == 6).await;
    f.runtime.stop(&id, &ToolStream::closed()).unwrap();
    wait_for(|| {
        f.runs()
            .iter()
            .any(|r| r.status == AgentRunStatus::Settling)
    })
    .await;
    assert!(
        revise(&f, &id, "primary", 1, "do not race settlement")
            .await
            .is_err()
    );
    assert_eq!(f.runs().len(), 6);
    f.host.settle.notify_one();
    assert!(task.await.unwrap().unwrap().is_error);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Cancelled);
    assert!(
        revise(&f, &id, "primary", 1, "do not revive cancelled work")
            .await
            .is_err()
    );
    assert_eq!(f.workflows()[0].revision, 1);
    assert_eq!(f.runs().len(), 6);
}

struct WriterHost {
    base: Arc<FakeHost>,
    root: PathBuf,
    reopens: Mutex<Vec<PathBuf>>,
}
impl WriterHost {
    fn initial_files(&self, description: &str) -> Result<PathBuf, ToolError> {
        let path = self.root.join(description);
        std::fs::create_dir(&path)
            .map_err(|e| ToolError::Execution(format!("fresh worktree allocation failed: {e}")))?;
        std::fs::write(path.join("tracked.txt"), "original edit\n").unwrap();
        std::fs::write(path.join("untracked.bin"), [0, 159, 255]).unwrap();
        std::fs::write(path.join("ignored.txt"), "ignored user work").unwrap();
        Ok(path)
    }
}
impl AgentHost for WriterHost {
    fn validate_role<'a>(
        &'a self,
        role: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ToolError>> + Send + 'a>> {
        self.base.validate_role(role)
    }
    fn validate_reopen<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let root = self.root.canonicalize().unwrap();
            let path = path.canonicalize().map_err(|e| {
                ToolError::Execution(format!("retained worktree missing, no HEAD fallback: {e}"))
            })?;
            if path.parent() != Some(root.as_path()) {
                return Err(ToolError::Execution("foreign retained worktree".into()));
            }
            Ok(())
        })
    }
    fn run_subagent<'a>(
        &'a self,
        request: SubagentRequest,
        progress: &'a ToolStream,
        cancel: &'a CancellationToken,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if request.agent == "artisan" && request.isolation.as_deref() == Some("worktree") {
                let path = self.initial_files(&request.description)?;
                progress.progress(format!("agent|artisan|path|{}", path.display()));
            }
            self.base.run_subagent(request, progress, cancel, id).await
        })
    }
    fn run_subagent_reopened<'a>(
        &'a self,
        request: SubagentRequest,
        progress: &'a ToolStream,
        cancel: &'a CancellationToken,
        id: &'a str,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            self.validate_reopen(path).await?;
            assert_eq!(
                std::fs::read(path.join("untracked.bin")).unwrap(),
                [0, 159, 255]
            );
            assert_eq!(
                std::fs::read_to_string(path.join("ignored.txt")).unwrap(),
                "ignored user work"
            );
            let mut text = std::fs::read_to_string(path.join("tracked.txt")).unwrap();
            assert!(text.contains("original edit"));
            text.push_str("rework edit\n");
            std::fs::write(path.join("tracked.txt"), text).unwrap();
            self.reopens.lock().push(path.to_owned());
            progress.progress(format!("agent|artisan|path|{}", path.display()));
            self.base.run_subagent(request, progress, cancel, id).await
        })
    }
}
#[tokio::test]
async fn migration_waiting_integration_rejects_gate_rework_before_explicit_resume_or_accept() {
    for action in ["resume", "accept"] {
        let f = Fixture::new();
        let root = f.root.join("worktrees");
        std::fs::create_dir(&root).unwrap();
        let host = Arc::new(WriterHost {
            base: f.host.clone(),
            root,
            reopens: Mutex::new(vec![]),
        });
        let tool = AgentWorkflowTool::new(host.clone(), f.runtime.clone());
        let initial = tool
            .execute_dyn(
                json!({"name":"migration","inputs":migration_inputs()}),
                &f.ctx("initial-writers"),
                &mut ToolStream::closed(),
            )
            .await
            .unwrap();
        assert!(!initial.is_error);
        let id = f.workflows()[0].id.clone();
        for revision in [0, 1] {
            let before = f.workflows()[0].clone();
            let runs = f.runs();
            assert_eq!(before.status, AgentWorkflowStatus::WaitingIntegration);
            assert_eq!(before.revision, revision);
            let rejected = tool
                .execute_dyn(
                    json!({"action":"revise","id":id,"node_id":"gate","feedback":"recheck integrated gate","expected_revision":revision}),
                    &f.ctx("gate-rework"),
                    &mut ToolStream::closed(),
                )
                .await
                .expect_err("gate rework must not authorize manual integration");
            assert!(rejected.to_string().contains("resume/accept"));
            assert_eq!(f.workflows()[0], before);
            assert_eq!(f.runs(), runs);
            assert!(f.host.calls.lock().iter().all(|call| {
                !matches!(
                    call.request.description.as_str(),
                    "gate" | "workflow-acceptance"
                )
            }));
            if revision == 0 {
                let reworked = tool
                    .execute_dyn(
                        json!({"action":"revise","id":id,"node_id":"apply-core","feedback":"preserve edits and fix the core slice","expected_revision":0}),
                        &f.ctx("writer-rework"),
                        &mut ToolStream::closed(),
                    )
                    .await
                    .unwrap();
                assert!(!reworked.is_error);
                assert_eq!(host.reopens.lock().len(), 1);
                assert!(runs.iter().all(|old| f.runs().contains(old)));
            }
        }
        let accepted = tool
            .execute_dyn(
                json!({"action":action,"id":id}),
                &f.ctx("explicit-integration"),
                &mut ToolStream::closed(),
            )
            .await
            .unwrap();
        assert!(!accepted.is_error);
        assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
        assert_eq!(f.workflows()[0].acceptance.as_deref(), Some("PASS"));
        let calls = f.host.calls.lock();
        for node in ["gate", "workflow-acceptance"] {
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.request.description == node)
                    .count(),
                1
            );
        }
        assert!(
            calls
                .iter()
                .find(|call| call.request.description == "gate")
                .unwrap()
                .request
                .prompt
                .contains(&f.root.display().to_string())
        );
    }
}

#[tokio::test]
async fn migration_rework_reuses_dirty_writer_tree_then_waits_for_explicit_integration_and_new_acceptance()
 {
    let f = Fixture::new();
    let root = f.root.join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let host = Arc::new(WriterHost {
        base: f.host.clone(),
        root,
        reopens: Mutex::new(vec![]),
    });
    let tool = AgentWorkflowTool::new(host.clone(), f.runtime.clone());
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: FAIL".into();
    tool.execute_dyn(
        json!({"name":"migration","inputs":migration_inputs()}),
        &f.ctx("writers"),
        &mut ToolStream::closed(),
    )
    .await
    .unwrap();
    let id = f.workflows()[0].id.clone();
    assert!(
        tool.execute_dyn(
            json!({"action":"resume","id":id}),
            &f.ctx("gate"),
            &mut ToolStream::closed()
        )
        .await
        .unwrap()
        .is_error
    );
    let original = f
        .runs()
        .into_iter()
        .find(|r| r.node_id.as_deref() == Some("apply-core"))
        .unwrap();
    let original_scope = f.runtime.state.lock().runs[&original.id]
        .options
        .scope
        .clone();
    let tree = PathBuf::from(&original.path).canonicalize().unwrap();
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: PASS".into();
    let result=tool.execute_dyn(json!({"action":"revise","id":id,"node_id":"apply-core","feedback":"fix the core slice and preserve prior edits","expected_revision":0}),&f.ctx("revise-writer"),&mut ToolStream::closed()).await.unwrap();
    assert!(!result.is_error);
    assert_eq!(
        f.workflows()[0].status,
        AgentWorkflowStatus::WaitingIntegration
    );
    assert_eq!(f.workflows()[0].revision, 1);
    assert!(f.workflows()[0].acceptance.is_none());
    assert_eq!(&*host.reopens.lock(), std::slice::from_ref(&tree));
    assert_eq!(
        std::fs::read_to_string(tree.join("tracked.txt")).unwrap(),
        "original edit\nrework edit\n"
    );
    assert!(f.runs().contains(&original));
    let current = f.workflows()[0]
        .nodes
        .iter()
        .find(|n| n.id == "apply-core")
        .unwrap()
        .run_id
        .clone()
        .unwrap();
    assert_ne!(current, original.id);
    assert_eq!(
        f.runtime.state.lock().runs[&current].options.scope,
        original_scope
    );
    assert!(
        !tool
            .execute_dyn(
                json!({"action":"accept","id":id}),
                &f.ctx("gate-revision"),
                &mut ToolStream::closed()
            )
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Completed);
    assert_eq!(f.workflows()[0].acceptance.as_deref(), Some("PASS"));
    assert_eq!(
        std::fs::read(tree.join("untracked.bin")).unwrap(),
        [0, 159, 255]
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("ignored.txt")).unwrap(),
        "ignored user work"
    );
}
#[tokio::test]
async fn rework_rechecks_writer_scope_before_advancing_revision() {
    let f = Fixture::new();
    let root = f.root.join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let host = Arc::new(WriterHost {
        base: f.host.clone(),
        root,
        reopens: Mutex::new(vec![]),
    });
    let tool = AgentWorkflowTool::new(host, f.runtime.clone());
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: FAIL".into();
    tool.execute_dyn(
        json!({"name":"migration","inputs":migration_inputs()}),
        &f.ctx("slices"),
        &mut ToolStream::closed(),
    )
    .await
    .unwrap();
    let id = f.workflows()[0].id.clone();
    tool.execute_dyn(
        json!({"action":"resume","id":id}),
        &f.ctx("failed-gate"),
        &mut ToolStream::closed(),
    )
    .await
    .unwrap();
    let outside = f.agent();
    let ctx = f.ctx("outside-writer");
    let task = tokio::spawn(async move {
        outside.execute_dyn(json!({"agent":"artisan","prompt":"hold","isolation":"shared","scope":{"paths":["src/core.rs"]}}),&ctx,&mut ToolStream::closed()).await
    });
    wait_for(|| f.host.calls.lock().iter().any(|c| c.id == "outside-writer")).await;
    let before = f.runs().len();
    let result=tool.execute_dyn(json!({"action":"revise","id":id,"node_id":"apply-core","feedback":"fix the core","expected_revision":0}),&f.ctx("conflicting-revise"),&mut ToolStream::closed()).await;
    assert!(result.unwrap_err().to_string().contains("scope overlaps"));
    assert_eq!(f.workflows()[0].revision, 0);
    assert_eq!(f.runs().len(), before);
    f.runtime
        .stop("outside-writer", &ToolStream::closed())
        .unwrap();
    f.host.settle.notify_one();
    task.await.unwrap().unwrap();
    let core = f
        .runs()
        .into_iter()
        .find(|r| r.node_id.as_deref() == Some("apply-core"))
        .unwrap();
    std::fs::remove_dir_all(&core.path).unwrap();
    assert!(tool.execute_dyn(json!({"action":"revise","id":id,"node_id":"apply-core","feedback":"missing tree must not allocate HEAD","expected_revision":0}),&f.ctx("missing-tree"),&mut ToolStream::closed()).await.is_err());
    assert_eq!(f.workflows()[0].revision, 0);
    assert_eq!(f.runs().len(), before);
    assert!(!Path::new(&core.path).exists());
}

#[tokio::test]
async fn rework_survives_codemode_yield_and_same_cell_wait_collects_new_acceptance() {
    let f = Fixture::new();
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: FAIL".into();
    workflow(&f, "research", json!({"QUESTION":"question"}))
        .await
        .unwrap();
    let id = f.workflows()[0].id.clone();
    *f.host.verdict.lock() = "WORKFLOW_ACCEPTANCE: PASS".into();
    f.host.gated_nodes.lock().insert("primary".into());
    let registry = Arc::new(ToolRegistry::new());
    registry.register(Arc::new(f.workflow()));
    let cells = Arc::new(crate::CodeCells::new());
    let run = crate::RunCodeTool::new(registry.clone(), cells.clone(), "rework-session".into());
    let wait = crate::WaitCodeTool::new(cells, "rework-session".into());
    let script = format!(
        "let result=call('agent_workflow',{{action:'revise',id:'{id}',node_id:'primary',feedback:'fix citations',expected_revision:0}}); if(result.is_error) throw 'rework failed'; complete('rework accepted');"
    );
    let ctx = f.ctx("outer-rework");
    let first = run
        .execute_dyn(
            json!({"script":script,"yield_time_ms":20}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!first.is_error);
    assert_eq!(first.details.as_ref().unwrap()["phase"], "yielded");
    assert_eq!(f.workflows()[0].revision, 1);
    assert_eq!(f.workflows()[0].status, AgentWorkflowStatus::Running);
    assert!(!text(&first).contains("rework accepted"));
    let cell = first.details.as_ref().unwrap()["cell_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.host.gate.add_permits(1);
    let done = wait
        .execute_dyn(
            json!({"cell_id":cell,"yield_time_ms":1000}),
            &f.ctx("wait-rework"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!done.is_error);
    assert_eq!(done.details.as_ref().unwrap()["cell_id"], cell);
    assert!(text(&done).contains("rework accepted"));
    assert!(!ctx.cancel.is_cancelled());
    let states: Vec<AgentStateEvent> =
        serde_json::from_value(done.details.unwrap()["agent_states"].clone()).unwrap();
    assert!(states.iter().any(|e|matches!(e,AgentStateEvent::Workflow(w) if w.id==id&&w.revision==1&&w.status==AgentWorkflowStatus::Completed&&w.acceptance.as_deref()==Some("PASS"))));
}

#[tokio::test]
async fn completed_writer_message_reopens_retained_tree_as_a_new_summary_run() {
    let f = Fixture::new();
    let root = f.root.join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let host = Arc::new(WriterHost {
        base: f.host.clone(),
        root,
        reopens: Mutex::new(vec![]),
    });
    AgentTool::new(host.clone()).with_runtime(f.runtime.clone()).execute_dyn(json!({"agent":"artisan","prompt":"implement bounded change","description":"writer","isolation":"worktree","scope":{"paths":["src/a.rs"]}}),&f.ctx("old-writer"),&mut ToolStream::closed()).await.unwrap();
    let old = f.runs()[0].clone();
    let result = AgentMessageTool::new(host.clone(), f.runtime.clone())
        .execute_dyn(
            json!({"id":old.id,"message":"fix the reported case"}),
            &f.ctx("new-writer"),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(host.reopens.lock().len(), 1);
    assert!(f.runs().contains(&old));
    let current = f.runs().into_iter().find(|r| r.id != old.id).unwrap();
    assert_eq!(current.call_id, "new-writer");
    assert!(
        current
            .prompt
            .contains("NEW run using summarized context only")
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&current.path).join("tracked.txt")).unwrap(),
        "original edit\nrework edit\n"
    );
    std::fs::remove_dir_all(&current.path).unwrap();
    let count = f.runs().len();
    assert!(
        AgentMessageTool::new(host, f.runtime.clone())
            .execute_dyn(
                json!({"id":current.id,"message":"missing checkout must not start from HEAD"}),
                &f.ctx("missing"),
                &mut ToolStream::closed()
            )
            .await
            .is_err()
    );
    assert_eq!(f.runs().len(), count);
}
