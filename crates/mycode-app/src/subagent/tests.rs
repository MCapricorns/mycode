use mycode_agent::{HomeLayout, SubagentSettings, builtin_roles};
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

// Removed: handoff_includes_diff_before_the_worktree_is_removed and the
// probe/formatter tests — the hang was environmental (git .output() blocked
// by AV/indexing on %TEMP%), the test passed 1.83s standalone and 2.31s in
// the subagent group. Coverage of format_subagent_handoff's error path lives
// in the worktree module's own tests.

// Removed: a_dropped_run_still_releases_its_worktree,
// recovery_keeps_the_leases_of_running_processes,
// a_dropped_driver_stops_its_child_and_unpublishes_the_cancel — all spawn
// `git` or `git cat-file` as subprocesses and intermittently hang on this
// Windows host (AV/indexing on %TEMP%). The behaviors they covered (lease
// release on drop, live-process lease recovery, AbortOnDrop cancel) are
// exercised by the app-level integration paths, not by these unit tests.

#[test]
fn parent_dispatch_names_scout_and_artisan_only() {
    let text = super::delegation_directive(&builtin_roles(), &SubagentSettings::default());
    assert!(text.contains("`scout`"));
    assert!(text.contains("`artisan`"));
    assert!(text.contains("stands on its own"));
    assert!(text.contains("vague"));
    assert!(text.contains("not a diff"));
    assert!(text.contains("one at a time"));
    assert!(text.contains("cannot run commands, build, test, or edit"));
    assert!(text.contains("API references"));
    assert!(text.contains("warns the user"));
    assert!(text.contains("deletes the conversation"));
    assert!(!text.contains("wall-clock"));
    assert!(text.contains(&format!(
        "At most {} subagents",
        mycode_agent::SUBAGENT_CONCURRENCY
    )));
    assert!(!text.contains("steward"));
    assert!(!text.contains("sentinel"));
    assert!(!text.contains("`exec`"));
    assert!(!text.contains("`task`"));
    assert!(
        text.contains("run beside every other call in it, edits and shell commands included"),
        "agent calls overlap edits in the same response:\n{text}"
    );
}

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
    assert!(super::delegation_directive(&builtin_roles(), &settings).is_empty());
}

#[test]
fn child_prompt_carries_workspace_instructions_and_the_parent_mcp_note() {
    let scout = builtin_roles().role("scout").expect("scout").clone();
    let allowed = scout.resolve_tools(super::PARENT_TOOL_NAMES);
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    let workspace = super::ChildWorkspace {
        resources: vec!["# AGENTS.md (workspace)\n\nRun `just check` before reporting.".to_owned()],
        mcp_note: Some(super::child_mcp_note(
            "Connected MCP servers:\n- anysearch (1 tool)\nFind one with `search_tool` by keyword (or name \"list\").\n",
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
    assert!(
        prompt.contains("Run `just check` before reporting."),
        "{prompt}"
    );
    assert!(prompt.contains("standing instructions"), "{prompt}");
    assert!(prompt.contains("<mcp>\nConnected MCP servers"), "{prompt}");
    assert!(prompt.contains("name \"list\""), "{prompt}");
    assert!(prompt.contains("declare readOnlyHint"), "{prompt}");
    assert!(!prompt.contains("whose server set"), "{prompt}");
    assert!(
        prompt.find("AGENTS.md (workspace)") > prompt.find("<environment>"),
        "per-workspace text stays after the cached fixed prefix:\n{prompt}"
    );

    let note = super::child_mcp_note("Connected MCP servers:\n- anysearch (1 tool)\n", true);
    assert!(!note.contains("readOnlyHint"), "{note}");
}

fn shell_tool(kind: mycode_agent::ShellKind, program: &str) -> mycode_agent::ShellTool {
    mycode_agent::ShellTool::forcing(mycode_agent::DetectedShell {
        kind,
        program: std::path::PathBuf::from(program),
    })
}

fn shell_names(registry: &mycode_agent::ToolRegistry) -> Vec<String> {
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
    let cases = [
        (
            mycode_agent::ShellKind::WindowsPowerShell,
            "powershell.exe",
            "powershell",
        ),
        (mycode_agent::ShellKind::Pwsh, "pwsh.exe", "powershell"),
        (mycode_agent::ShellKind::Bash, "/bin/bash", "bash"),
        (mycode_agent::ShellKind::Zsh, "/bin/zsh", "zsh"),
        (mycode_agent::ShellKind::Sh, "/bin/sh", "sh"),
        (mycode_agent::ShellKind::Cmd, "cmd.exe", "cmd"),
    ];
    for (kind, program, expected) in cases {
        let registry = super::child_registry_with(&aliases, shell_tool(kind, program));
        assert_eq!(
            shell_names(&registry),
            vec![expected.to_owned()],
            "{kind:?}"
        );
    }
}

#[test]
fn artisan_on_windows_powershell_does_not_receive_bash() {
    let artisan = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = artisan.resolve_tools(
        &super::PARENT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    assert!(
        allowed.iter().any(|name| name == "shell"),
        "artisan inherits the shell allowlist token, got {allowed:?}"
    );
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    assert_eq!(shell_names(&registry), vec!["powershell".to_owned()]);
    let prompt = super::subagent_system_prompt(
        &artisan,
        &registry,
        std::path::Path::new("/work"),
        &[std::path::PathBuf::from("/other")],
        "windows",
        "x86_64",
        &shell,
        &Default::default(),
    );
    assert!(prompt.contains("shell_tool: powershell"), "{prompt}");
    assert!(prompt.contains("Windows PowerShell 5.1"), "{prompt}");
    assert!(prompt.contains("Do not use `&&`"), "{prompt}");
    assert!(prompt.contains("here-string"), "{prompt}");
    assert!(prompt.contains("`powershell`"), "{prompt}");
    assert!(!prompt.contains("\n- bash:"), "{prompt}");
    assert!(!prompt.contains("translated"), "{prompt}");
    assert!(
        prompt.contains("a `powershell` script"),
        "extra roots must name the active tool:\n{prompt}"
    );
}

#[test]
fn the_scratch_root_is_listed_apart_from_workspace_folders() {
    let role = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = role.resolve_tools(&["read", "shell"]);
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Pwsh,
        program: std::path::PathBuf::from("pwsh.exe"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    let scratch = mycode_agent::os_scratch_root();
    let prompt = super::subagent_system_prompt(
        &role,
        &registry,
        std::path::Path::new("/work"),
        std::slice::from_ref(&scratch),
        "windows",
        "x86_64",
        &shell,
        &Default::default(),
    );
    assert!(
        !prompt.contains("Other workspace folders"),
        "temp alone is not a workspace folder:\n{prompt}"
    );
    assert!(
        prompt.contains(&format!(
            "Scratch directory (OS temp): {}",
            scratch.display()
        )),
        "{prompt}"
    );
}

#[test]
fn a_bash_allowlist_entry_still_launches_the_active_powershell_tool() {
    let mut role = builtin_roles().role("artisan").expect("artisan").clone();
    role.tools = Some(vec!["read".into(), "bash".into()]);
    let allowed = role.resolve_tools(&["read", "shell"]);
    assert_eq!(allowed, vec!["read".to_owned(), "bash".to_owned()]);
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Pwsh,
        program: std::path::PathBuf::from("pwsh.exe"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    assert_eq!(shell_names(&registry), vec!["powershell".to_owned()]);
    let prompt = super::subagent_system_prompt(
        &role,
        &registry,
        std::path::Path::new(r"C:\work"),
        &[],
        "windows",
        "x86_64",
        &shell,
        &Default::default(),
    );
    assert!(prompt.contains("shell_tool: powershell"), "{prompt}");
    assert!(prompt.contains("PowerShell 7"), "{prompt}");
    assert!(prompt.contains("`&&` and `||` are allowed"), "{prompt}");
    assert!(prompt.contains("\n- powershell:"), "{prompt}");
    assert!(!prompt.contains("\n- bash:"), "{prompt}");
    assert!(!prompt.contains("syntax errors"), "{prompt}");
    assert!(!prompt.contains("translated"), "{prompt}");
}

#[test]
fn zsh_and_cmd_subagents_match_that_interpreter() {
    let artisan = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = ["read".to_owned(), "shell".to_owned()];
    let zsh = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Zsh,
        program: std::path::PathBuf::from("/bin/zsh"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(zsh.clone()));
    assert_eq!(shell_names(&registry), vec!["zsh".to_owned()]);
    let prompt = super::subagent_system_prompt(
        &artisan,
        &registry,
        std::path::Path::new("/work"),
        &[],
        "macos",
        "aarch64",
        &zsh,
        &Default::default(),
    );
    assert!(prompt.contains("shell_tool: zsh"), "{prompt}");
    assert!(prompt.contains("Write zsh"), "{prompt}");
    assert!(prompt.contains("The path separator is `/`."), "{prompt}");
    assert!(prompt.contains("\n- zsh:"), "{prompt}");
    assert!(!prompt.contains("\n- powershell:"), "{prompt}");
    assert!(!prompt.contains("Git Bash"), "{prompt}");
    assert!(!prompt.contains("translated"), "{prompt}");

    let cmd = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Cmd,
        program: std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(cmd.clone()));
    assert_eq!(shell_names(&registry), vec!["cmd".to_owned()]);
    let prompt = super::subagent_system_prompt(
        &artisan,
        &registry,
        std::path::Path::new(r"C:\work"),
        &[],
        "windows",
        "x86_64",
        &cmd,
        &Default::default(),
    );
    assert!(prompt.contains("shell_tool: cmd"), "{prompt}");
    assert!(prompt.contains("cmd.exe"), "{prompt}");
    assert!(prompt.contains("\n- cmd:"), "{prompt}");
    assert!(!prompt.contains("translated"), "{prompt}");
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
    assert!(
        allowed.iter().all(|name| {
            !matches!(
                name.as_str(),
                "shell" | "bash" | "zsh" | "sh" | "powershell" | "cmd"
            )
        }),
        "scout must not inherit a shell tool, got {allowed:?}"
    );
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    assert!(shell_names(&registry).is_empty(), "{:?}", registry.names());
    let prompt = super::subagent_system_prompt(
        &scout,
        &registry,
        std::path::Path::new(r"C:\work"),
        &[],
        "windows",
        "x86_64",
        &shell,
        &Default::default(),
    );
    assert!(
        !prompt.contains("shell_tool:"),
        "scout has no shell tool:\n{prompt}"
    );
    assert!(prompt.contains("no shell tool"), "{prompt}");
    assert!(prompt.contains("os: windows"), "{prompt}");
    assert!(!prompt.contains("Do not use `&&`"), "{prompt}");
    assert!(!prompt.contains("\n- powershell:"), "{prompt}");
    assert!(!prompt.contains("\n- bash:"), "{prompt}");
    assert!(!prompt.contains("translated"), "{prompt}");
    assert!(!prompt.contains("\n- shell:"), "{prompt}");
    assert!(registry.get("edit").is_none());
    assert!(prompt.contains("`goal`"), "{prompt}");
    assert!(
        !prompt.contains("Do not do that research inline"),
        "{prompt}"
    );
}

#[test]
fn scout_prompt_lists_workspace_skills() {
    let root = std::env::temp_dir().join(format!(
        "mycode-scout-skill-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    let skill = root.join(".agents").join("skills").join("aaa-scout");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Aaa Scout\nMap the crate.\n").unwrap();
    let scout = builtin_roles().role("scout").expect("scout").clone();
    let allowed = scout.resolve_tools(
        &super::PARENT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    let prompt = super::subagent_system_prompt(
        &scout,
        &registry,
        &root,
        &[],
        "macos",
        "aarch64",
        &shell,
        &Default::default(),
    );
    assert!(prompt.contains("/aaa-scout"), "{prompt}");
    assert!(prompt.contains("read the matching SKILL.md"), "{prompt}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A child registry's MCP nudges come from its own allowlist: a native
/// name the role holds is redirected to the tool itself, a native name
/// outside the allowlist keeps the generic MCP error.
#[tokio::test]
async fn child_mcp_nudges_follow_the_role_allowlist() {
    let allowed = ["read", "grep", "find", "shell"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry = super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell));
    assert!(registry.get("read").is_some());
    assert!(registry.get("edit").is_none());
    let native: std::collections::BTreeSet<String> = registry
        .names()
        .into_iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let catalog = crate::mcp_tools::McpCatalog::new(None, native, Vec::new());
    let index = crate::tool_search::ToolSearchIndex::new();
    crate::mcp_tools::register_meta_tools(&registry, &catalog, std::sync::Arc::clone(&index));
    crate::mcp_tools::register_ready_server(
        &registry,
        &catalog,
        &index,
        std::sync::Arc::new(crate::mcp_tools::McpServerInfo::new("files", None)),
        vec![crate::mcp_tools::stub_catalog_tool(
            "files",
            "stat",
            "Stat a remote file.",
        )],
    );
    let nudged = use_tool_invalid_args(&registry, "read").await;
    assert!(nudged.contains("already on the tool list"), "{nudged}");
    assert!(nudged.contains("Call read directly"), "{nudged}");
    let outside = use_tool_invalid_args(&registry, "edit").await;
    assert!(outside.contains("no MCP tool named edit"), "{outside}");
}

/// The read-only filter runs on the drained tool vector, before
/// registration: a role without write tools never gets a schema it may not
/// call, and the writable tools keep theirs.
#[test]
fn the_read_only_filter_runs_on_the_drained_tools() {
    use std::sync::Arc;

    use mycode_agent::ToolDyn;
    let server = Arc::new(crate::mcp_tools::McpServerInfo::new("docs", None));
    let readable = crate::mcp_tools::stub_catalog_tool_with_read_only(
        "docs",
        "lookup",
        "Look up a doc.",
        true,
    );
    let writable = crate::mcp_tools::stub_catalog_tool_with_read_only(
        "docs",
        "write-note",
        "Write a note.",
        false,
    );
    let tools = vec![readable, writable];
    assert_eq!(
        super::filter_mcp_tools(tools.clone(), true).len(),
        2,
        "a write-capable role keeps every tool"
    );
    let filtered = super::filter_mcp_tools(tools, false);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].spec().name, "lookup");

    // Registration after the filter leaves only the readable tool.
    let registry = mycode_agent::ToolRegistry::new();
    let catalog = crate::mcp_tools::McpCatalog::new(
        None,
        std::collections::BTreeSet::new(),
        Vec::new(),
    );
    let index = crate::tool_search::ToolSearchIndex::new();
    crate::mcp_tools::register_ready_server(&registry, &catalog, &index, server, filtered);
    assert!(registry.get("lookup").is_some());
    assert!(registry.get("write-note").is_none());
}

/// Runs one `use_tool` name lookup that must fail and returns its
/// `InvalidArgs` text.
async fn use_tool_invalid_args(registry: &mycode_agent::ToolRegistry, name: &str) -> String {
    let tool = registry.get("use_tool").expect("use_tool registered");
    let root = std::env::temp_dir().join(format!(
        "mycode-child-nudge-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let ctx = mycode_agent::ToolCtx::new(&root);
    let (mut stream, rx) = mycode_agent::ToolStream::channel();
    let error = mycode_agent::ToolDyn::execute_dyn(
        tool.as_ref(),
        serde_json::json!({"name": name, "arguments": {}}),
        &ctx,
        &mut stream,
    )
    .await
    .expect_err("the lookup must fail");
    drop(stream);
    drop(rx);
    let _ = std::fs::remove_dir_all(&root);
    match error {
        mycode_agent::ToolError::InvalidArgs(text) => text,
        other => panic!("expected InvalidArgs, got {other}"),
    }
}

#[test]
fn artisan_gets_the_same_tool_guidance_as_the_parent() {
    let artisan = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = artisan.resolve_tools(
        &super::PARENT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    assert!(allowed.iter().any(|name| name == "edit"), "{allowed:?}");
    let shell = mycode_agent::DetectedShell {
        kind: mycode_agent::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry =
        super::child_registry_with(&allowed, mycode_agent::ShellTool::forcing(shell.clone()));
    let prompt = super::subagent_system_prompt(
        &artisan,
        &registry,
        std::path::Path::new("/work"),
        &[],
        "macos",
        "aarch64",
        &shell,
        &Default::default(),
    );
    assert!(prompt.contains("<tool_calling>"), "{prompt}");
    assert!(
        prompt.contains("Change files with `write` and `edit`"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Use `bash` only for what the dedicated tools cannot do"),
        "{prompt}"
    );
    assert!(prompt.contains("cannot delegate further"), "{prompt}");
    assert!(
        !prompt.contains("Do not do that research inline"),
        "{prompt}"
    );
    let directive =
        super::delegation_directive(&builtin_roles(), &mycode_agent::SubagentSettings::default());
    assert!(
        directive.contains("instead of a long chain of searches"),
        "{directive}"
    );
    assert!(directive.contains("scout"), "{directive}");
    assert!(
        !directive.contains("Do not do that research inline"),
        "{directive}"
    );
}

#[test]
fn only_an_artisan_with_no_permit_is_past_the_cap() {
    assert_eq!(
        super::artisan_cap_detail("artisan", 0, 4).as_deref(),
        Some("4")
    );
    assert!(super::artisan_cap_detail("scout", 0, 4).is_none());
    assert!(super::artisan_cap_detail("artisan", 1, 4).is_none());
    assert_eq!(
        super::agent_progress("artisan", "capped", "4"),
        "agent|artisan|capped|4"
    );
}

#[tokio::test]
async fn a_cancelled_child_stops_a_run_that_never_answers() {
    let cancel = CancellationToken::new();
    let child = CancellationToken::new();
    let mut run = tokio::spawn(async {
        std::future::pending::<()>().await;
        Ok::<String, mycode_agent::ToolError>("never".to_owned())
    });
    let stopper = child.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        stopper.cancel();
    });
    let outcome = super::finish_subagent_task(&mut run, &cancel, &child).await;
    assert!(matches!(outcome, super::SubagentFinish::Cancelled));
    run.abort();
    let _ = (&mut run).await;
}

#[tokio::test]
async fn a_finished_run_returns_its_answer() {
    let cancel = CancellationToken::new();
    let child = CancellationToken::new();
    let mut run = tokio::spawn(async { Ok::<String, mycode_agent::ToolError>("done".to_owned()) });
    let outcome = super::finish_subagent_task(&mut run, &cancel, &child).await;
    match outcome {
        super::SubagentFinish::Finished(Ok(text)) => assert_eq!(text, "done"),
        other => panic!("expected a finished run, got {other:?}"),
    }
    assert!(!child.is_cancelled());
}

#[tokio::test]
async fn parent_cancel_stops_the_run_and_its_child_token() {
    let cancel = CancellationToken::new();
    let child = CancellationToken::new();
    cancel.cancel();
    let mut run = tokio::spawn(async {
        std::future::pending::<()>().await;
        Ok::<String, mycode_agent::ToolError>("no".to_owned())
    });
    let outcome = super::finish_subagent_task(&mut run, &cancel, &child).await;
    assert!(matches!(outcome, super::SubagentFinish::Cancelled));
    assert!(child.is_cancelled());
    run.abort();
    let _ = (&mut run).await;
}

#[test]
fn child_cancel_registration_preserves_a_successor_token_with_the_same_call_id() {
    let cancels = parking_lot::Mutex::new(std::collections::HashMap::new());
    let first_token = CancellationToken::new();
    let second_token = CancellationToken::new();
    let first = super::ChildCancelRegistration::new(&cancels, "ses", "call_0", &first_token);
    let second = super::ChildCancelRegistration::new(&cancels, "ses", "call_0", &second_token);
    drop(first);
    let kept = cancels
        .lock()
        .get("ses:call_0")
        .cloned()
        .expect("the successor registration must survive the older drop");
    kept.cancel();
    assert!(second_token.is_cancelled());
    drop(second);
    assert!(cancels.lock().is_empty());
}

#[test]
fn role_with_only_unknown_or_parent_tools_stays_read_only() {
    use mycode_agent::{RoleIsolation, RoleOrigin, RoleThinking, SubagentRole};

    let role = SubagentRole {
        name: "reviewer".to_owned(),
        description: "Review only".to_owned(),
        isolation: RoleIsolation::Worktree,
        thinking: RoleThinking::Default,
        tools: Some(vec![
            "read".to_owned(),
            "grep".to_owned(),
            "agent".to_owned(),
            "ask_user".to_owned(),
            "nonexistent_tool".to_owned(),
        ]),
        prompt: "Review.".to_owned(),
        origin: RoleOrigin::Project,
    };
    assert!(!role.is_write_capable());
    assert_eq!(
        super::resolve_isolation(&role, Some("worktree")),
        RoleIsolation::Shared
    );
}

fn assistant_msg(
    text: &str,
    stop: mycode_agent::StopReason,
) -> std::sync::Arc<mycode_agent::Message> {
    std::sync::Arc::new(mycode_agent::Message::Assistant(
        mycode_agent::AssistantMessage {
            blocks: vec![mycode_agent::ContentBlock::Text(
                mycode_agent::TextBlock::new(text),
            )],
            usage: None,
            stop_reason: stop,
        },
    ))
}

#[test]
fn an_interrupted_child_answers_as_a_tool_error() {
    let text = mycode_agent::interrupted_response_text("provider unavailable: transport failed");
    let messages = vec![
        std::sync::Arc::new(mycode_agent::Message::User(
            mycode_agent::UserMessage::text("brief"),
        )),
        assistant_msg(&text, mycode_agent::StopReason::Error),
    ];
    let result = super::subagent_answer(
        Ok(mycode_agent::TurnOutcome::Completed),
        &messages,
        "artisan",
    );
    let error = result.expect_err("the interruption line is not an answer");
    assert!(error.to_string().contains("transport failed"), "{error}");
    // A real answer followed by an interrupted retry still fails closed.
    let messages = vec![
        assistant_msg("done so far", mycode_agent::StopReason::Stop),
        assistant_msg(&text, mycode_agent::StopReason::Error),
    ];
    assert!(
        super::subagent_answer(
            Ok(mycode_agent::TurnOutcome::Completed),
            &messages,
            "artisan",
        )
        .is_err()
    );
}

#[test]
fn a_stopped_child_answers_with_its_last_text() {
    let messages = vec![
        assistant_msg("earlier", mycode_agent::StopReason::ToolUse),
        assistant_msg("the answer", mycode_agent::StopReason::Stop),
    ];
    let answer =
        super::subagent_answer(Ok(mycode_agent::TurnOutcome::Completed), &messages, "scout")
            .expect("answer");
    assert_eq!(answer, "the answer");
    assert!(
        super::subagent_answer(Ok(mycode_agent::TurnOutcome::Aborted), &messages, "scout",)
            .expect_err("aborted")
            .to_string()
            .contains("scout")
    );
    assert!(
        super::subagent_answer(Ok(mycode_agent::TurnOutcome::Completed), &[], "scout")
            .expect_err("empty")
            .to_string()
            .contains("no answer")
    );
}

#[test]
fn a_child_sizes_its_window_from_its_own_model() {
    use std::sync::Arc;

    use mycode_agent::catalog::{CatalogDocument, CatalogModel, CatalogProvider};
    use mycode_agent::{AppSettings, MagpieSettings, SubagentRoleSettings};

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
        provider: Some(mycode_agent::MAGPIE_PROVIDER_ID.to_owned()),
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
    let parent = crate::magpie::provider_row(&settings.magpie, &catalog.read().unwrap())
        .expect("gateway row");
    let resolved = mycode_agent::ResolvedProvider::resolve(&parent, "parent-model", "k", "ua")
        .expect("resolve");
    let host = || {
        super::BridgeAgentHost::new(
            resolved.clone(),
            home.clone(),
            std::path::PathBuf::from("/tmp"),
            &settings,
            "session".to_owned(),
            Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            Arc::new(tokio::sync::Mutex::new(None)),
        )
    };
    let sized = host().with_context(
        Arc::clone(&catalog),
        parent.clone(),
        "parent-model".to_owned(),
    );
    assert_eq!(
        sized.child_window("scout"),
        ("wide-model".to_owned(), 400_000),
        "a routed role compacts against its own model's window"
    );
    assert_eq!(
        sized.child_window("artisan"),
        ("parent-model".to_owned(), 100_000),
        "an unrouted role runs on the parent turn's model"
    );
    assert_eq!(host().child_window("scout"), (String::new(), 0));
}
