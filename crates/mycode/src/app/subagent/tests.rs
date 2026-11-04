use crate::config::{HomeLayout, SubagentSettings, builtin_roles};
use tokio_util::sync::CancellationToken;

#[test]
fn parent_dispatch_never_names_a_role_switched_off_in_settings() {
    let mut settings = SubagentSettings::default();
    settings.role_mut("artisan").enabled = false;
    let text = super::delegation_directive(&builtin_roles(), &settings);
    assert!(text.contains("`scout`"), "{text}");
    assert!(!text.contains("artisan"), "{text}");

    let mut settings = SubagentSettings::default();
    settings.role_mut("scout").enabled = false;
    let text = super::delegation_directive(&builtin_roles(), &settings);
    assert!(text.contains("`artisan`"), "{text}");
    assert!(!text.contains("scout"), "{text}");

    settings.role_mut("artisan").enabled = false;
    let remaining = super::delegation_directive(&builtin_roles(), &settings);
    assert!(remaining.contains("`sentinel`"));

    for name in builtin_roles().names() {
        settings.role_mut(&name).enabled = false;
    }
    assert!(super::delegation_directive(&builtin_roles(), &settings).is_empty());
}

#[test]
fn child_prompt_carries_workspace_instructions_and_the_parent_mcp_note() {
    let scout = builtin_roles().role("scout").expect("scout").clone();
    let allowed = scout.resolve_tools(super::PARENT_TOOL_NAMES);
    let shell = crate::tools::DetectedShell {
        kind: crate::tools::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry =
        super::child_registry_with(&allowed, crate::tools::ShellTool::forcing(shell.clone()));
    let workspace = super::ChildWorkspace {
        resources: vec!["# AGENTS.md (workspace)\n\nRun `just check` before reporting.".to_owned()],
        mcp_note: Some(super::child_mcp_note(
            "Connected MCP servers:\n- anysearch (1 tool)\n",
            scout.is_write_capable(),
        )),
        ..Default::default()
    };
    let prompt = super::subagent_system_prompt(
        &scout,
        &registry,
        std::path::Path::new("/work"),
        &[],
        "linux",
        "x86_64",
        &shell,
        &workspace,
    );
    assert!(prompt.contains("Run `just check` before reporting."));
    assert!(prompt.contains("<mcp>\nConnected MCP servers"));
    assert!(prompt.find("AGENTS.md (workspace)") > prompt.find("<environment>"));
}

fn shell_tool(kind: crate::tools::ShellKind, program: &str) -> crate::tools::ShellTool {
    crate::tools::ShellTool::forcing(crate::tools::DetectedShell {
        kind,
        program: std::path::PathBuf::from(program),
    })
}

fn shell_names(registry: &crate::tools::ToolRegistry) -> Vec<String> {
    registry
        .names()
        .into_iter()
        .filter(|name| {
            matches!(
                name.as_str(),
                "powershell" | "bash" | "zsh" | "sh" | "cmd" | "shell"
            )
        })
        .collect()
}

#[test]
fn child_registry_keeps_one_shell_tool_for_every_allowlist_alias() {
    let aliases = [
        "read".to_owned(),
        "shell".to_owned(),
        "bash".to_owned(),
        "zsh".to_owned(),
        "sh".to_owned(),
        "powershell".to_owned(),
        "cmd".to_owned(),
    ];
    for (kind, program, expected) in [
        (
            crate::tools::ShellKind::WindowsPowerShell,
            "powershell.exe",
            "powershell",
        ),
        (crate::tools::ShellKind::Pwsh, "pwsh.exe", "powershell"),
        (crate::tools::ShellKind::Bash, "/bin/bash", "bash"),
    ] {
        let registry = super::child_registry_with(&aliases, shell_tool(kind, program));
        assert_eq!(shell_names(&registry), vec![expected.to_owned()]);
    }
}

#[test]
fn scout_stays_read_only_and_does_not_list_a_shell() {
    let scout = builtin_roles().role("scout").expect("scout").clone();
    let allowed = scout.resolve_tools(
        &super::PARENT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    let shell = crate::tools::DetectedShell {
        kind: crate::tools::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    };
    let registry =
        super::child_registry_with(&allowed, crate::tools::ShellTool::forcing(shell.clone()));
    assert!(shell_names(&registry).is_empty());
    let prompt = super::subagent_system_prompt(
        &scout,
        &registry,
        std::path::Path::new(r"C:\work"),
        &[std::path::PathBuf::from(r"C:\other")],
        "windows",
        "x86_64",
        &shell,
        &Default::default(),
    );
    assert!(!prompt.contains("shell_tool:"));
    assert!(registry.get("edit").is_none());
}

#[test]
fn the_read_only_filter_runs_on_the_drained_tools() {
    use crate::tools::ToolDyn;
    use std::sync::Arc;
    let server = Arc::new(crate::app::mcp_tools::McpServerInfo::new("docs", None));
    let readable = crate::app::mcp_tools::stub_catalog_tool_with_read_only(
        "docs",
        "lookup",
        "Look up a doc.",
        true,
    );
    let writable = crate::app::mcp_tools::stub_catalog_tool_with_read_only(
        "docs",
        "write-note",
        "Write a note.",
        false,
    );
    let tools = vec![readable, writable];
    assert_eq!(
        crate::app::mcp_tools::filter_mcp_tools(tools.clone(), true).len(),
        2
    );
    let filtered = crate::app::mcp_tools::filter_mcp_tools(tools, false);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].spec().name, "lookup");

    let registry = crate::tools::ToolRegistry::new();
    let catalog =
        crate::app::mcp_tools::McpCatalog::new(None, std::collections::BTreeSet::new(), Vec::new());
    let index = crate::app::tool_search::ToolSearchIndex::new();
    crate::app::mcp_tools::register_ready_server(&registry, &catalog, &index, server, filtered);
    assert!(registry.get("lookup").is_some());
    assert!(registry.get("write-note").is_none());
}

#[tokio::test]
async fn parent_cancel_stops_the_run_and_its_child_token() {
    let cancel = CancellationToken::new();
    let child = CancellationToken::new();
    cancel.cancel();
    let mut run = tokio::spawn(async {
        std::future::pending::<()>().await;
        Ok::<String, crate::tools::ToolError>("no".to_owned())
    });
    let outcome = super::finish_subagent_task(&mut run, &cancel, &child).await;
    assert!(matches!(outcome, super::SubagentFinish::Cancelled));
    assert!(child.is_cancelled());
    run.abort();
    let _ = (&mut run).await;
}

fn assistant_msg(
    text: &str,
    stop: crate::core::StopReason,
) -> std::sync::Arc<crate::core::Message> {
    std::sync::Arc::new(crate::core::Message::Assistant(
        crate::core::AssistantMessage {
            blocks: vec![crate::core::ContentBlock::Text(
                crate::core::TextBlock::new(text),
            )],
            usage: None,
            stop_reason: stop,
        },
    ))
}

#[test]
fn subagent_answer_extracts_final_text_and_rejects_errors_or_truncation() {
    let messages = vec![
        assistant_msg("earlier", crate::core::StopReason::ToolUse),
        assistant_msg("the answer", crate::core::StopReason::Stop),
    ];
    let answer =
        super::subagent_answer(Ok(crate::core::TurnOutcome::Completed), &messages, "scout")
            .expect("answer");
    assert_eq!(answer, "the answer");
    assert!(
        super::subagent_answer(Ok(crate::core::TurnOutcome::Aborted), &messages, "scout").is_err()
    );
    let truncated = vec![assistant_msg("partial", crate::core::StopReason::Length)];
    assert!(
        super::subagent_answer(
            Ok(crate::core::TurnOutcome::Completed),
            &truncated,
            "sentinel"
        )
        .is_err()
    );
}

#[test]
fn a_child_sizes_its_window_from_its_own_model() {
    use crate::config::{AppSettings, MagpieSettings, SubagentRoleSettings};
    use crate::providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};
    use std::sync::Arc;

    let mut settings = AppSettings {
        magpie: MagpieSettings {
            enabled: true,
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            api_key: "k".to_owned(),
            model: "parent-model".to_owned(),
            hidden_models: Vec::new(),
        },
        ..AppSettings::default()
    };
    settings.subagents.roles.push(SubagentRoleSettings {
        role: "scout".to_owned(),
        enabled: true,
        provider: Some(crate::config::MAGPIE_PROVIDER_ID.to_owned()),
        model: Some("wide-model".to_owned()),
        thinking: None,
    });
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-window")).unwrap();
    let model = |id: &str, context: u64| CatalogModel {
        id: id.to_owned(),
        name: id.to_owned(),
        context,
        ..CatalogModel::default()
    };
    let catalog = Arc::new(std::sync::RwLock::new(Arc::new(CatalogDocument {
        providers: vec![CatalogProvider {
            id: "magpie".to_owned(),
            name: "Magpie".to_owned(),
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            models: vec![model("parent-model", 100_000), model("wide-model", 400_000)],
            ..CatalogProvider::default()
        }],
    })));
    let parent = crate::app::magpie::provider_row(&settings.magpie, &catalog.read().unwrap())
        .expect("gateway row");
    let resolved = crate::providers::ResolvedProvider::resolve(&parent, "parent-model", "k", "ua")
        .expect("resolve");
    let host = super::BridgeAgentHost::new(
        resolved,
        home,
        std::path::PathBuf::from("/tmp"),
        &settings,
        "session".to_owned(),
        Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
        Arc::new(crate::tools::CodeCells::new()),
    )
    .with_context(Arc::clone(&catalog), parent, "parent-model".to_owned());
    assert_eq!(
        host.child_window("scout", &settings),
        ("wide-model".to_owned(), 400_000)
    );
    assert_eq!(
        host.child_window("artisan", &settings),
        ("parent-model".to_owned(), 100_000)
    );
}
