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

#[test]
fn parent_agent_tools_follow_enabled_roles() {
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
    for name in ["agent", "agent_status", "agent_stop", "agent_message"] {
        assert!(
            all_on.iter().any(|registered| registered == name),
            "{all_on:?}"
        );
    }

    let mut all_off = SubagentSettings::default();
    for name in catalog.names() {
        all_off.role_mut(&name).enabled = false;
    }
    assert_eq!(build(&all_off), vec!["agent_status", "agent_stop"]);
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
                None,
            )
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("write capability changed after admission")
        );
    }
}
