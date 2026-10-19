use super::worktree::{
    LeaseGuard, WorktreeLease, format_subagent_handoff, git_command, handoff_worktree,
};
use mycode_config::{HomeLayout, SubagentSettings, builtin_roles};
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

fn git(repo: &std::path::Path, args: &[&str]) {
    let output = git_command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "mycode")
        .env("GIT_AUTHOR_EMAIL", "mycode@example.com")
        .env("GIT_COMMITTER_NAME", "mycode")
        .env("GIT_COMMITTER_EMAIL", "mycode@example.com")
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn handoff_includes_diff_before_the_worktree_is_removed() {
    let root = std::env::temp_dir().join(format!(
        "mycode-wt-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    let repo = root.join("repo");
    let home_root = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home_root).unwrap();
    git(&repo, &["init"]);
    std::fs::write(repo.join("tracked.txt"), "old\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "init"]);
    let home = HomeLayout::from_root(&home_root).unwrap();
    let lease = WorktreeLease::acquire(&home, &repo).unwrap();
    let path = lease.path.clone();
    std::fs::write(path.join("tracked.txt"), "new\n").unwrap();
    std::fs::write(path.join("created.txt"), "created-body\n").unwrap();
    std::fs::write(path.join("with space.txt"), "spaced-body\n").unwrap();
    let text = handoff_worktree(lease, Ok("answer".to_owned())).unwrap();
    assert!(text.contains("answer"), "{text}");
    assert!(text.contains("created-body"), "{text}");
    assert!(text.contains("spaced-body"), "{text}");
    assert!(text.contains("new"), "{text}");
    assert!(
        !path.exists(),
        "worktree was removed before the diff was captured"
    );
    let error = format_subagent_handoff("subagent failed", "diff-body");
    assert!(error.contains("subagent failed") && error.contains("diff-body"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_dropped_run_still_releases_its_worktree() {
    let root = std::env::temp_dir().join(format!(
        "mycode-wt-drop-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    let repo = root.join("repo");
    let home_root = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home_root).unwrap();
    git(&repo, &["init"]);
    std::fs::write(repo.join("tracked.txt"), "old\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-m", "init"]);
    let home = HomeLayout::from_root(&home_root).unwrap();
    let lease = WorktreeLease::acquire(&home, &repo).unwrap();
    let path = lease.path.clone();
    let manifest = lease.manifest.clone();
    // A parent cancel drops the `agent` call mid-await with the lease held.
    drop(LeaseGuard(Some(lease)));
    let released = tokio::time::timeout(Duration::from_secs(30), async {
        while path.exists() || manifest.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(released.is_ok(), "the dropped lease stayed on disk");
    let listed = git_command()
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert_eq!(
        listed.matches("worktree ").count(),
        1,
        "the repository still lists the leased checkout: {listed}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_dropped_driver_stops_its_child_and_unpublishes_the_cancel() {
    let cancels = parking_lot::Mutex::new(std::collections::HashMap::new());
    let child = CancellationToken::new();
    let registration = super::ChildCancelRegistration::new(&cancels, "ses", "call-1", &child);
    assert!(cancels.lock().contains_key("ses:call-1"));
    let run = tokio::spawn(std::future::pending::<()>());
    let guard = super::AbortOnDrop {
        cancel: child.clone(),
        task: run.abort_handle(),
    };
    drop(guard);
    drop(registration);
    assert!(child.is_cancelled());
    let joined = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("the child run kept going after its driver was dropped");
    assert!(joined.unwrap_err().is_cancelled());
    assert!(cancels.lock().is_empty());
}

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
        mycode_config::SUBAGENT_CONCURRENCY
    )));
    assert!(!text.contains("steward"));
    assert!(!text.contains("sentinel"));
    assert!(!text.contains("`exec`"));
    assert!(!text.contains("`task`"));
}

fn shell_tool(kind: mycode_tools::ShellKind, program: &str) -> mycode_tools::ShellTool {
    mycode_tools::ShellTool::forcing(mycode_tools::DetectedShell {
        kind,
        program: std::path::PathBuf::from(program),
    })
}

fn shell_names(registry: &mycode_tools::ToolRegistry) -> Vec<String> {
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
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-shell")).unwrap();
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
            mycode_tools::ShellKind::WindowsPowerShell,
            "powershell.exe",
            "powershell",
        ),
        (mycode_tools::ShellKind::Pwsh, "pwsh.exe", "powershell"),
        (mycode_tools::ShellKind::Bash, "/bin/bash", "bash"),
        (mycode_tools::ShellKind::Zsh, "/bin/zsh", "zsh"),
        (mycode_tools::ShellKind::Sh, "/bin/sh", "sh"),
        (mycode_tools::ShellKind::Cmd, "cmd.exe", "cmd"),
    ];
    for (kind, program, expected) in cases {
        let registry = super::child_registry_with(&home, &aliases, shell_tool(kind, program));
        assert_eq!(
            shell_names(&registry),
            vec![expected.to_owned()],
            "{kind:?}"
        );
    }
}

#[test]
fn artisan_on_windows_powershell_does_not_receive_bash() {
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-ps")).unwrap();
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
    let shell = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(shell.clone()),
    );
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
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-scratch")).unwrap();
    let role = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = role.resolve_tools(&["read", "shell"]);
    let shell = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::Pwsh,
        program: std::path::PathBuf::from("pwsh.exe"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(shell.clone()),
    );
    let scratch = mycode_tools::os_scratch_root();
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
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-alias")).unwrap();
    let mut role = builtin_roles().role("artisan").expect("artisan").clone();
    role.tools = Some(vec!["read".into(), "bash".into()]);
    let allowed = role.resolve_tools(&["read", "shell"]);
    assert_eq!(allowed, vec!["read".to_owned(), "bash".to_owned()]);
    let shell = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::Pwsh,
        program: std::path::PathBuf::from("pwsh.exe"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(shell.clone()),
    );
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
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-zsh")).unwrap();
    let artisan = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = ["read".to_owned(), "shell".to_owned()];
    let zsh = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::Zsh,
        program: std::path::PathBuf::from("/bin/zsh"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(zsh.clone()),
    );
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

    let cmd = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::Cmd,
        program: std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(cmd.clone()),
    );
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
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-scout")).unwrap();
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
    let shell = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(shell.clone()),
    );
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
    let home = HomeLayout::from_root(&root).unwrap();
    let scout = builtin_roles().role("scout").expect("scout").clone();
    let allowed = scout.resolve_tools(
        &super::PARENT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    let shell = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(shell.clone()),
    );
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

#[test]
fn artisan_gets_the_same_tool_guidance_as_the_parent() {
    let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-artisan")).unwrap();
    let artisan = builtin_roles().role("artisan").expect("artisan").clone();
    let allowed = artisan.resolve_tools(
        &super::PARENT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    );
    assert!(allowed.iter().any(|name| name == "edit"), "{allowed:?}");
    let shell = mycode_tools::DetectedShell {
        kind: mycode_tools::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    };
    let registry = super::child_registry_with(
        &home,
        &allowed,
        mycode_tools::ShellTool::forcing(shell.clone()),
    );
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
    let directive = super::delegation_directive(
        &builtin_roles(),
        &mycode_config::SubagentSettings::default(),
    );
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
        Ok::<String, mycode_tools::ToolError>("never".to_owned())
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
    let mut run = tokio::spawn(async { Ok::<String, mycode_tools::ToolError>("done".to_owned()) });
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
        Ok::<String, mycode_tools::ToolError>("no".to_owned())
    });
    let outcome = super::finish_subagent_task(&mut run, &cancel, &child).await;
    assert!(matches!(outcome, super::SubagentFinish::Cancelled));
    assert!(child.is_cancelled());
    run.abort();
    let _ = (&mut run).await;
}

#[tokio::test]
async fn a_routed_role_sends_the_oauth_access_token_not_the_vault_blob() {
    use std::sync::Arc;

    use mycode_config::{
        AppSettings, AuthorityRevision, HomeLayout, ProviderSecrets, ProviderSettings,
        SubagentRoleSettings, SubagentSettings, replace_provider_secrets,
    };
    use mycode_providers::{OAuthSecret, OPENAI_CODEX_PROVIDER_ID, ResolvedProvider};

    let root = std::env::temp_dir().join(format!(
        "mycode-subagent-auth-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let home = HomeLayout::from_root(&root).unwrap();
    let blob = OAuthSecret {
        kind: "oauth".to_owned(),
        access: "live-access-token".to_owned(),
        refresh: "refresh-token".to_owned(),
        expires: u64::MAX / 2,
        account_id: Some("acct_123".to_owned()),
    }
    .encode();
    replace_provider_secrets(
        &home,
        AuthorityRevision::ABSENT,
        &ProviderSecrets::new().with_key(OPENAI_CODEX_PROVIDER_ID, Some(&blob)),
    )
    .unwrap();
    let parent = ProviderSettings {
        id: "openai".to_owned(),
        kind: "openai-completions".to_owned(),
        base_url: "https://api.openai.com/v1".to_owned(),
        models: vec!["gpt-parent".to_owned()],
        enabled: true,
        context_limit: None,
        max_output: None,
        hidden_models: Vec::new(),
    };
    let codex = ProviderSettings {
        id: OPENAI_CODEX_PROVIDER_ID.to_owned(),
        kind: "openai-responses".to_owned(),
        base_url: "https://chatgpt.com/backend-api/codex".to_owned(),
        models: vec!["gpt-5.5".to_owned()],
        enabled: true,
        context_limit: None,
        max_output: None,
        hidden_models: Vec::new(),
    };
    let mut subagents = SubagentSettings::default();
    subagents.roles.push(SubagentRoleSettings {
        role: "scout".to_owned(),
        enabled: true,
        provider: Some(OPENAI_CODEX_PROVIDER_ID.to_owned()),
        model: Some("gpt-5.5".to_owned()),
        thinking: None,
    });
    let settings = AppSettings {
        providers: vec![parent.clone(), codex],
        subagents,
        ..AppSettings::default()
    };
    let resolved = ResolvedProvider::resolve(&parent, "gpt-parent", "parent-key", "ua").unwrap();
    let host = super::BridgeAgentHost::new(
        resolved,
        home,
        std::path::PathBuf::from("/tmp"),
        &settings,
        "session".to_owned(),
        Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    let routed = host.resolve_route("scout").await.expect("route");
    let authorization = routed
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str())
        .unwrap_or_default();
    assert_eq!(authorization, "Bearer live-access-token");
    assert!(!authorization.contains("refresh-token"));
    assert!(
        routed
            .headers
            .iter()
            .any(|(name, value)| name == "originator" && value == mycode_providers::ORIGINATOR)
    );
    assert!(
        routed
            .headers
            .iter()
            .any(|(name, value)| { name == "chatgpt-account-id" && value == "acct_123" })
    );
    let _ = std::fs::remove_dir_all(&root);
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
    use mycode_config::{RoleIsolation, RoleOrigin, RoleThinking, SubagentRole};

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
    stop: mycode_core::StopReason,
) -> std::sync::Arc<mycode_core::Message> {
    std::sync::Arc::new(mycode_core::Message::Assistant(
        mycode_core::AssistantMessage {
            blocks: vec![mycode_core::ContentBlock::Text(
                mycode_core::TextBlock::new(text),
            )],
            usage: None,
            stop_reason: stop,
        },
    ))
}

#[test]
fn an_interrupted_child_answers_as_a_tool_error() {
    let text = mycode_core::interrupted_response_text("provider unavailable: transport failed");
    let messages = vec![
        std::sync::Arc::new(mycode_core::Message::User(mycode_core::UserMessage::text(
            "brief",
        ))),
        assistant_msg(&text, mycode_core::StopReason::Error),
    ];
    let result = super::subagent_answer(
        Ok(mycode_core::TurnOutcome::Completed),
        &messages,
        "artisan",
    );
    let error = result.expect_err("the interruption line is not an answer");
    assert!(error.to_string().contains("transport failed"), "{error}");
    // A real answer followed by an interrupted retry still fails closed.
    let messages = vec![
        assistant_msg("done so far", mycode_core::StopReason::Stop),
        assistant_msg(&text, mycode_core::StopReason::Error),
    ];
    assert!(
        super::subagent_answer(
            Ok(mycode_core::TurnOutcome::Completed),
            &messages,
            "artisan",
        )
        .is_err()
    );
}

#[test]
fn a_stopped_child_answers_with_its_last_text() {
    let messages = vec![
        assistant_msg("earlier", mycode_core::StopReason::ToolUse),
        assistant_msg("the answer", mycode_core::StopReason::Stop),
    ];
    let answer =
        super::subagent_answer(Ok(mycode_core::TurnOutcome::Completed), &messages, "scout")
            .expect("answer");
    assert_eq!(answer, "the answer");
    assert!(
        super::subagent_answer(Ok(mycode_core::TurnOutcome::Aborted), &messages, "scout",)
            .expect_err("aborted")
            .to_string()
            .contains("scout")
    );
    assert!(
        super::subagent_answer(Ok(mycode_core::TurnOutcome::Completed), &[], "scout")
            .expect_err("empty")
            .to_string()
            .contains("no answer")
    );
}
