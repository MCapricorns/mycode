//! Host integration checks without provider requests.
use super::*;
use crate::tools::builtin::AgentHost;

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
            crate::core::unix_now().as_nanos()
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
            crate::app::magpie::provider_row(&settings.magpie, &CatalogDocument::default())
                .unwrap();
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
            Arc::new(crate::tools::CodeCells::new()),
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
async fn cancelled_actual_host_stops_without_allocating_a_worktree_and_settles_scope() {
    use crate::tools::builtin::{AgentRuntime, AgentStateEvent, AgentStatusTool, AgentTool};
    use crate::tools::{ToolCtx, ToolDyn, ToolStream};
    let f = Fixture::new();
    let host = Arc::new(f.host(AppSettings::default()));
    let runtime = Arc::new(AgentRuntime::new(f.home.root().join("results")));
    let tool = AgentTool::new(host.clone()).with_runtime(runtime.clone());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ToolCtx::new(&f.cwd)
        .with_call_id("cancelled-child")
        .with_cancel(cancel);
    let outcome = tool
        .execute_dyn(
            serde_json::json!({"agent":"artisan","prompt":"cancelled before start","scope":{"paths":["src/owned.rs"]}}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await;
    assert!(outcome.is_err());
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
    assert!(states.is_empty(), "{states:?}");
    assert!(!f.home.root().join("agent-worktrees").exists());
    assert!(host.cancels.lock().is_empty());
}

#[test]
fn actual_host_steers_only_the_registered_child_inbox_and_redacts_known_credentials() {
    let f = Fixture::new();
    let host = f.host(AppSettings::default());
    let first = Arc::new(crate::agent::SteerInbox::new());
    let second = Arc::new(crate::agent::SteerInbox::new());
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

#[test]
fn parent_agent_tools_follow_enabled_roles_and_workflow_gate() {
    let f = Fixture::new();
    let catalog = crate::config::builtin_roles();
    let build = |settings: &SubagentSettings| {
        let registry = ToolRegistry::new();
        let host = Arc::new(f.host(AppSettings::default()));
        let runtime = Arc::new(crate::tools::builtin::AgentRuntime::new(
            f.home.root().join("results"),
        ));
        super::register_parent_agent_tools(&registry, &catalog, settings, host, runtime);
        registry.names()
    };

    let all_on = build(&SubagentSettings::default());
    for name in [
        "agent",
        "agent_status",
        "agent_stop",
        "agent_message",
        "agent_workflow",
    ] {
        assert!(
            all_on.iter().any(|registered| registered == name),
            "{all_on:?}"
        );
    }

    let mut without_sentinel = SubagentSettings::default();
    without_sentinel.role_mut("sentinel").enabled = false;
    let names = build(&without_sentinel);
    assert!(names.iter().any(|registered| registered == "agent"));
    assert!(names.iter().any(|registered| registered == "agent_status"));
    assert!(
        !names
            .iter()
            .any(|registered| registered == "agent_workflow"),
        "{names:?}"
    );

    let mut all_off = SubagentSettings::default();
    for name in catalog.names() {
        all_off.role_mut(&name).enabled = false;
    }
    assert_eq!(build(&all_off), vec!["agent_status", "agent_stop"]);
}

#[tokio::test]
async fn disabling_roles_keeps_prior_results_and_stop_controls_reachable() {
    use crate::tools::builtin::{AgentRuntime, AgentStateEvent, AgentTool};
    use crate::tools::{ToolCtx, ToolDyn, ToolStream};

    struct ReportHost;
    impl AgentHost for ReportHost {
        fn run_subagent<'a>(
            &'a self,
            _request: crate::tools::builtin::SubagentRequest,
            _progress: &'a ToolStream,
            _cancel: &'a CancellationToken,
            _call_id: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<String, crate::tools::ToolError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok("retained evidence".to_owned()) })
        }
    }

    let f = Fixture::new();
    let runtime = Arc::new(AgentRuntime::new(f.home.root().join("results")));
    let ctx = ToolCtx::new(&f.cwd).with_call_id("before-disable");
    AgentTool::new(Arc::new(ReportHost))
        .with_runtime(runtime.clone())
        .execute_dyn(
            serde_json::json!({"agent": "scout", "prompt": "Inspect one source."}),
            &ctx,
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    let catalog = crate::config::builtin_roles();
    let mut settings = AppSettings::default();
    for name in catalog.names() {
        settings.subagents.role_mut(&name).enabled = false;
    }
    let registry = ToolRegistry::new();
    super::register_parent_agent_tools(
        &registry,
        &catalog,
        &settings.subagents,
        Arc::new(f.host(settings.clone())),
        runtime,
    );
    assert!(registry.get("agent").is_none());
    assert!(registry.get("agent_message").is_none());
    assert!(registry.get("agent_workflow").is_none());
    for name in ["agent_status", "agent_stop"] {
        let result = registry
            .get(name)
            .unwrap()
            .execute_dyn(
                serde_json::json!({"id": "before-disable"}),
                &ctx,
                &mut ToolStream::closed(),
            )
            .await
            .unwrap();
        let states: Vec<AgentStateEvent> =
            serde_json::from_value(result.details.unwrap()["agent_states"].clone()).unwrap();
        assert!(states.iter().any(|state| matches!(
            state,
            AgentStateEvent::Run(run) if run.result.as_deref() == Some("retained evidence")
        )));
    }
}

#[tokio::test]
async fn newly_dispatched_children_observe_saved_role_and_skill_settings() {
    let fixture = Fixture::new();
    let host = fixture.host(AppSettings::default());
    assert!(host.validate_role("artisan").await.unwrap());
    let mut settings = host.settings.clone();
    settings.subagents.role_mut("artisan").enabled = false;
    settings.skills.set_enabled("deploy", false);
    settings.magpie.api_key = "rotated-child-secret".into();
    crate::config::replace_app_settings(
        &fixture.home,
        crate::config::AuthorityRevision::ABSENT,
        &settings,
    )
    .unwrap();
    assert!(
        host.validate_role("artisan")
            .await
            .unwrap_err()
            .to_string()
            .contains("disabled")
    );
    assert!(
        !host
            .current_settings()
            .await
            .unwrap()
            .skills
            .is_enabled("deploy")
    );
}

#[tokio::test]
async fn refreshed_gateway_credentials_remain_redacted() {
    let fixture = Fixture::new();
    let host = fixture.host(AppSettings::default());
    let mut settings = host.settings.clone();
    settings.magpie.api_key = "rotated-child-secret".into();
    crate::config::replace_app_settings(
        &fixture.home,
        crate::config::AuthorityRevision::ABSENT,
        &settings,
    )
    .unwrap();
    host.current_settings().await.unwrap();
    assert_eq!(
        host.redact_text("fixture-secret-key rotated-child-secret"),
        "[REDACTED] [REDACTED]"
    );
}

#[tokio::test]
async fn changed_role_capability_is_refused_before_child_setup() {
    let fixture = Fixture::new();
    let host = fixture.host(AppSettings::default());
    let roles = fixture.home.root().join("agents");
    std::fs::create_dir_all(&roles).unwrap();
    let write_role = |writer: bool| {
        let tools = if writer { "read, edit" } else { "read" };
        std::fs::write(roles.join("custom.md"), format!(
            "---\nname: custom\ndescription: Mutable role.\ntools: {tools}\nisolation: worktree\n---\nFollow the brief.\n"
        )).unwrap();
    };
    for admitted_writer in [false, true] {
        write_role(admitted_writer);
        assert_eq!(host.validate_role("custom").await.unwrap(), admitted_writer);
        write_role(!admitted_writer);
        // Includes retained-worktree dispatch: rejecting configuration drift
        // must happen before trying to reopen this deliberately missing path.
        for retained in [None, Some(fixture.cwd.as_path())] {
            let result = host
                .run_subagent_validated(
                    crate::tools::builtin::SubagentRequest {
                        agent: "custom".into(),
                        prompt: "Inspect the target".into(),
                        description: "capability drift".into(),
                        isolation: None,
                    },
                    &crate::tools::ToolStream::closed(),
                    &CancellationToken::new(),
                    "capability-drift",
                    admitted_writer,
                    retained,
                )
                .await;
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("write capability changed after admission")
            );
        }
        assert!(host.steers.lock().is_empty());
        assert!(host.cancels.lock().is_empty());
        assert!(!fixture.home.root().join("agent-worktrees").exists());
    }
}
