//! Host integration checks without provider requests.
use super::*;
use mycode_agent::builtin::AgentHost;

struct Fixture {
    root: PathBuf,
    home: HomeLayout,
    cwd: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mycode-host-test-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let home = HomeLayout::from_root(root.join("home")).unwrap();
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        Self { root, home, cwd }
    }
    fn host(&self, mut settings: AppSettings) -> BridgeAgentHost {
        settings.magpie.enabled = true;
        settings.magpie.base_url = "http://127.0.0.1:1/v1".into();
        settings.magpie.model = "fixture".into();
        settings.magpie.api_key = "fixture-secret-key".into();
        let provider =
            crate::magpie::provider_row(&settings.magpie, &CatalogDocument::default()).unwrap();
        let resolved =
            ResolvedProvider::resolve(&provider, "fixture", "fixture-secret-key", "fixture")
                .unwrap();
        BridgeAgentHost::new(
            resolved,
            self.home.clone(),
            self.cwd.clone(),
            &settings,
            "session".into(),
            Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            Arc::new(tokio::sync::Mutex::new(None)),
            Arc::new(mycode_agent::CodeCells::new()),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[tokio::test]
async fn actual_host_validates_custom_role_overrides_and_disabled_roles_before_running() {
    let f = Fixture::new();
    let roles = f.home.root().join("agents");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(roles.join("artisan.md"),"---\nname: artisan\ndescription: Custom read-only artisan.\ntools: read, grep\nisolation: shared\n---\nInspect only.\n").unwrap();
    std::fs::write(roles.join("custom.md"),"---\nname: custom\ndescription: Custom bounded writer.\ntools: read, edit\nisolation: shared\n---\nImplement the brief.\n").unwrap();
    let host = f.host(AppSettings::default());
    assert!(!host.validate_role("artisan").await.unwrap());
    assert!(host.validate_role("custom").await.unwrap());
    assert!(!host.validate_role("sentinel").await.unwrap());
    let mut settings = AppSettings::default();
    settings.subagents.role_mut("custom").enabled = false;
    let disabled = f.host(settings);
    assert!(
        disabled
            .validate_role("custom")
            .await
            .unwrap_err()
            .to_string()
            .contains("disabled")
    );
    assert!(disabled.validate_role("missing-role").await.is_err());
}
#[tokio::test]
async fn queued_actual_host_cancels_without_allocating_a_worktree_and_keeps_scope_until_settled() {
    use mycode_agent::builtin::{
        AgentRunStatus, AgentRuntime, AgentStateEvent, AgentStatusTool, AgentTool,
    };
    use mycode_agent::{ToolCtx, ToolDyn, ToolStream};
    let f = Fixture::new();
    let host = Arc::new(
        f.host(AppSettings::default())
            .with_slots(Arc::new(Semaphore::new(0))),
    );
    let runtime = Arc::new(AgentRuntime::new(f.home.root().join("results")));
    let tool = AgentTool::new(host.clone()).with_runtime(runtime.clone());
    let ctx = ToolCtx::new(&f.cwd).with_call_id("queued-child");
    let task = tokio::spawn(async move {
        tool.execute_dyn(serde_json::json!({"agent":"artisan","prompt":"queued only","scope":{"paths":["src/owned.rs"]}}),&ctx,&mut ToolStream::closed()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !host.cancels.lock().contains_key("session:queued-child") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let conflict=AgentTool::new(host.clone()).with_runtime(runtime.clone()).execute_dyn(serde_json::json!({"agent":"artisan","prompt":"colliding","scope":{"paths":["src/owned.rs"]}}),&ToolCtx::new(&f.cwd).with_call_id("other"),&mut ToolStream::closed()).await;
    assert!(conflict.unwrap_err().to_string().contains("active writer"));
    host.cancels.lock()["session:queued-child"].cancel();
    assert!(task.await.unwrap().unwrap().is_error);
    let status = AgentStatusTool::new(runtime)
        .execute_dyn(
            serde_json::json!({}),
            &ToolCtx::new(&f.cwd),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    let states: Vec<AgentStateEvent> =
        serde_json::from_value(status.details.unwrap()["agent_states"].clone()).unwrap();
    assert!(
        matches!(&states[0],AgentStateEvent::Run(r) if r.call_id=="queued-child" && r.status==AgentRunStatus::Cancelled)
    );
    assert!(!f.home.root().join("agent-worktrees").exists());
    assert!(host.cancels.lock().is_empty());
}

#[test]
fn actual_host_steers_only_the_registered_child_inbox_and_redacts_known_credentials() {
    let f = Fixture::new();
    let host = f.host(AppSettings::default());
    let first = Arc::new(mycode_agent::SteerInbox::new());
    let second = Arc::new(mycode_agent::SteerInbox::new());
    host.steers.lock().insert("inner.0".into(), first.clone());
    host.steers.lock().insert("inner.1".into(), second.clone());
    let registration = ChildSteerRegistration {
        steers: &host.steers,
        call_id: "inner.0",
    };
    host.steer_subagent("inner.0", "inspect callers").unwrap();
    assert_eq!(first.drain(), vec!["inspect callers"]);
    assert!(second.drain().is_empty());
    drop(registration);
    assert!(host.steer_subagent("inner.0", "late").is_err());
    assert_eq!(
        host.redact_text("result fixture-secret-key"),
        "result [REDACTED]"
    );
}
