use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use mycode_agent::ToolDyn;
use serde_json::{Value, json};

use super::{
    McpCatalog, McpServerInfo, McpSlot, UseTool, bound_mcp_result, register_meta_tools,
    register_ready_server,
};
use crate::mcp_client::McpTool;
use crate::tool_search::{ToolSearchIndex, ToolSearchTool};

/// The catalog comes only from magpie's `~/.mycode/mcp.json` now: a project
/// file or a settings row can no longer add or shadow a server.
#[test]
fn only_the_magpie_library_feeds_the_catalog() {
    let root = std::env::temp_dir().join(format!(
        "mycode-magpie-only-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    // A project `.mycode/mcp.json` is ignored outright.
    std::fs::create_dir_all(root.join(".mycode")).unwrap();
    std::fs::write(
        root.join(".mycode").join("mcp.json"),
        r#"{"mcpServers":{"attacker":{"url":"https://attacker.example/mcp"}}}"#,
    )
    .unwrap();
    // magpie's own file is `home.root()/mcp.json`.
    std::fs::write(
        root.join("mcp.json"),
        r#"{"mcpServers":{"docs":{"url":"https://docs.example/mcp"}}}"#,
    )
    .unwrap();
    let home = mycode_agent::HomeLayout::from_root(&root).unwrap();
    let merged = super::merged_mcp_servers(&home);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(merged.servers.len(), 1, "{:?}", merged.servers);
    assert_eq!(merged.servers[0].id, "docs");
    assert_eq!(merged.magpie_ids.iter().next().map(String::as_str), Some("docs"));

    // A magpie row never takes a vault key: its file carries the headers.
    let secrets =
        mycode_agent::ProviderSecrets::new().with_key("mcp-docs", Some("stale-key"));
    assert_eq!(
        super::stored_mcp_key(&secrets, &merged.magpie_ids, "docs"),
        None
    );
}

/// Lowercased registry-name set, mirroring how the turn snapshots the
/// registry before the meta tools register.
fn native(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_ascii_lowercase()).collect()
}

/// Builds a catalog the way a turn does: `empty` plus one `insert_ready`
/// per server, in server order.
fn catalog_from(
    tools: Vec<Arc<super::DynamicMcpTool>>,
    warning: Option<String>,
    native: &BTreeSet<String>,
) -> Arc<McpCatalog> {
    let catalog = McpCatalog::new(warning, native.clone(), Vec::new());
    let mut groups: Vec<(Arc<McpServerInfo>, Vec<Arc<super::DynamicMcpTool>>)> = Vec::new();
    for tool in tools {
        match groups
            .iter_mut()
            .find(|(server, _)| server.id == tool.server.id)
        {
            Some((_, group)) => group.push(tool),
            None => groups.push((Arc::clone(&tool.server), vec![tool])),
        }
    }
    for (server, tools) in groups {
        catalog.insert_ready(server, tools);
    }
    catalog
}

#[test]
fn mcp_snippets_say_what_the_tool_does() {
    assert_eq!(
        super::mcp_snippet("docs", Some("Look up a library.\n  Returns  pages. More.")),
        "Look up a library. (MCP server 'docs')"
    );
    assert_eq!(
        super::mcp_snippet("docs", None),
        "MCP tool on server 'docs'."
    );
    let long = super::mcp_snippet("docs", Some(&"word ".repeat(80)));
    assert!(long.ends_with("\u{2026} (MCP server 'docs')"), "{long}");
}

/// The prompt line a server contributes is one bounded sentence: server
/// instructions are untrusted text, so they never reach the prompt as
/// several lines.
#[test]
fn a_server_summary_is_one_bounded_line() {
    assert_eq!(
        super::mcp_server_summary("Look things up.\nSecond line."),
        Some("Look things up.".to_owned())
    );
    assert_eq!(super::mcp_server_summary(" \n\t "), None);
    let long = super::mcp_server_summary(&"word ".repeat(80)).expect("summary");
    assert!(long.ends_with('\u{2026}'), "{long}");
    assert!(long.chars().count() <= 161, "{long}");
}

#[test]
fn oversized_results_are_truncated_and_stored() {
    let dir = std::env::temp_dir().join(format!(
        "mycode-mcp-result-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    let text = "y".repeat(30 * 1024);
    let shown = bound_mcp_result(&dir, &text);
    assert!(shown.len() < text.len());
    assert!(shown.contains("full text stored at"));
    let stored = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .next()
        .unwrap();
    assert_eq!(stored, text);
    assert_eq!(bound_mcp_result(&dir, "short"), "short");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn storing_a_result_sweeps_week_old_results_only() {
    let dir = std::env::temp_dir().join(format!(
        "mycode-mcp-prune-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let stale = dir.join("mcp-1-1.txt");
    let fresh = dir.join("mcp-1-2.txt");
    let unrelated = dir.join("notes.txt");
    for path in [&stale, &fresh, &unrelated] {
        std::fs::write(path, "body").unwrap();
    }
    let eight_days_ago =
        std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 60 * 60);
    for path in [&stale, &unrelated] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(eight_days_ago)
            .unwrap();
    }
    bound_mcp_result(&dir, &"z".repeat(30 * 1024));
    let kept = (!stale.exists(), fresh.exists(), unrelated.exists());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        kept,
        (true, true, true),
        "stale removed, fresh and unrelated kept"
    );
}

#[test]
fn list_names_tools_without_schemas() {
    let catalog = catalog_from(
        vec![
            catalog_tool("anysearch", "search", "Search the web."),
            catalog_tool("github", "create_issue", "Open a GitHub issue."),
        ],
        None,
        &native(&[]),
    );
    let listed = catalog.list_text();
    assert!(listed.contains("search — Search the web."), "{listed}");
    assert!(
        listed.contains("create_issue — Open a GitHub issue."),
        "{listed}"
    );
    assert!(!listed.contains("inputSchema"));
    let note = catalog.prompt_note();
    assert!(note.contains("\"list\"") || note.contains("search_tool"), "{note}");
    assert!(!note.contains("create_issue"), "{note}");
}

/// The config error is the reason a server is missing, so `"list"` keeps
/// it instead of claiming nothing is set up.
#[test]
fn list_keeps_the_config_error_above_the_tools() {
    let catalog = McpCatalog::new(
        Some("MCP config unreadable: access denied".to_owned()),
        native(&[]),
        Vec::new(),
    );
    let listed = catalog.list_text();
    assert!(listed.contains("MCP config: MCP config unreadable: access denied"));
    assert!(listed.contains("No MCP tools are connected."));
}

struct ScriptedMcp {
    calls: std::sync::Mutex<Vec<serde_json::Value>>,
}

impl crate::mcp_client::JsonRpcChannel for ScriptedMcp {
    fn request<'a>(
        &'a self,
        _id: u64,
        method: &'a str,
        params: serde_json::Value,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, crate::mcp_client::McpError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            match method {
                "initialize" => Ok(serde_json::json!({
                    "protocolVersion": "2025-06-18",
                    "instructions": "Look up docs.\nSecond line."
                })),
                "tools/list" => Ok(serde_json::json!({
                "tools": [
                {
                "name": "lookup",
                "description": "Look up a doc",
                "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
                },
                "annotations": {"readOnlyHint": true}
                },
                {
                "name": "write-note",
                "description": "Write a note",
                "inputSchema": {"type": "object"},
                "annotations": {"readOnlyHint": false}
                }
                ]
                })),
                "tools/call" => {
                    self.calls.lock().expect("calls").push(params);
                    Ok(serde_json::json!({
                    "content": [{"type": "text", "text": "pong"}],
                    "isError": false
                    }))
                }
                other => Err(crate::mcp_client::McpError::protocol(other)),
            }
        })
    }

    fn notify<'a>(
        &'a self,
        _method: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), crate::mcp_client::McpError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn mcp_call_runs_the_server_tool() {
    let scripted = Arc::new(ScriptedMcp {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let mut client = crate::mcp_client::McpClient::new(scripted.clone());
    client.initialize().await.expect("handshake");
    assert_eq!(client.instructions(), Some("Look up docs.\nSecond line."));
    let listed = client.list_tools().await.expect("list");
    assert!(
        listed
            .iter()
            .any(|tool| tool.name == "lookup" && tool.read_only)
    );
    assert!(
        listed
            .iter()
            .any(|tool| tool.name == "write-note" && !tool.read_only)
    );
    let server = Arc::new(super::McpServerInfo::new("docs", None));
    let broken = Arc::new(AtomicBool::new(false));
    let result_dir = Arc::new(std::env::temp_dir());
    let client = Arc::new(tokio::sync::Mutex::new(client));
    let mut tools = Vec::new();
    for tool in listed {
        tools.push(Arc::new(super::DynamicMcpTool::new(
            server.clone(),
            tool,
            client.clone(),
            broken.clone(),
            result_dir.clone(),
        )));
    }
    let readonly: Vec<_> = tools
        .iter()
        .filter(|tool| tool.read_only())
        .cloned()
        .collect();
    assert_eq!(readonly.len(), 1);
    assert_eq!(readonly[0].spec().name, "lookup");
    assert!(tools.iter().any(|tool| tool.spec().name == "write-note"));
    let lookup = readonly[0].clone();
    let root = std::env::temp_dir().join(format!(
        "mycode-mcp-call-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let ctx = mycode_agent::ToolCtx::new(&root);
    let (mut stream, rx) = mycode_agent::ToolStream::channel();
    let result = mycode_agent::ToolDyn::execute_dyn(
        lookup.as_ref(),
        serde_json::json!({"query": "direct"}),
        &ctx,
        &mut stream,
    )
    .await
    .expect("call the MCP tool");
    drop(stream);
    drop(rx);
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_agent::message::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(!result.is_error, "{text}");
    assert!(text.contains("pong"), "{text}");
    // A required field under another framework's spelling is repaired
    // before validation instead of costing the model a turn.
    let (mut stream, rx) = mycode_agent::ToolStream::channel();
    let repaired = mycode_agent::ToolDyn::execute_dyn(
        lookup.as_ref(),
        serde_json::json!({"q": "repaired"}),
        &ctx,
        &mut stream,
    )
    .await
    .expect("call the MCP tool with a repaired argument");
    drop(stream);
    drop(rx);
    assert!(!repaired.is_error);
    let calls = scripted.calls.lock().expect("calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["name"], "lookup");
    assert_eq!(calls[0]["arguments"]["query"], "direct");
    assert_eq!(calls[1]["arguments"]["query"], "repaired");
    drop(calls);
    let _ = std::fs::remove_dir_all(&root);

    let catalog = catalog_from(tools, None, &native(&[]));
    for spelling in [
        "lookup",
        "LOOKUP",
        "docs.lookup",
        "docs:lookup",
        "mcp__docs__lookup",
    ] {
        assert_eq!(
            catalog.get(spelling).map(|(key, _)| key),
            Some("lookup".to_owned()),
            "{spelling}"
        );
    }
    assert!(catalog.get("docs.missing").is_none());
}

fn server_info(id: &str, instructions: Option<&str>) -> Arc<super::McpServerInfo> {
    Arc::new(super::McpServerInfo::new(id, instructions))
}

fn tool_on(
    server: &Arc<super::McpServerInfo>,
    name: &str,
    description: &str,
) -> Arc<super::DynamicMcpTool> {
    let channel = Arc::new(ScriptedMcp {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let client = crate::mcp_client::McpClient::new(channel);
    Arc::new(super::DynamicMcpTool::new(
        Arc::clone(server),
        McpTool {
            name: name.to_owned(),
            description: Some(description.to_owned()),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: true,
        },
        Arc::new(tokio::sync::Mutex::new(client)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(std::env::temp_dir()),
    ))
}

fn catalog_tool(server: &str, name: &str, description: &str) -> Arc<super::DynamicMcpTool> {
    tool_on(&server_info(server, None), name, description)
}

#[test]
fn same_named_tools_on_two_servers_both_stay_reachable() {
    let tools = vec![
        catalog_tool("alpha", "search", "Search alpha issues."),
        catalog_tool("beta", "search", "Search beta wiki pages."),
    ];
    let catalog = catalog_from(tools, None, &native(&[]));
    assert_eq!(
        catalog.get("search").map(|(key, _)| key),
        Some("search".to_owned())
    );
    let (key, tool) = catalog.get("beta__search").expect("second server");
    assert_eq!((key.as_str(), tool.server_id()), ("beta__search", "beta"));
    let (_, tool) = catalog
        .get("beta.search")
        .expect("server-prefixed spelling");
    assert_eq!(tool.server_id(), "beta");
    let listed = catalog.list_text();
    assert!(
        listed.contains("beta__search — Search beta wiki pages."),
        "{listed}"
    );
}

/// The note names every connected server with its tool count and its own
/// summary, and preloads no tool: a tool is only findable when the model
/// knows which servers are up and how to reach their tools.
#[test]
fn the_prompt_note_names_connected_servers() {
    let anysearch = server_info("anysearch", None);
    let github = server_info(
        "github",
        Some("Manage GitHub issues, pull requests, and code.\nSecond line."),
    );
    let tools = vec![
        tool_on(&anysearch, "search", "Search the web."),
        tool_on(&github, "create_issue", "Open a GitHub issue."),
        tool_on(&github, "list_pulls", "List pull requests."),
    ];
    let catalog = catalog_from(tools, None, &native(&[]));
    let note = catalog.prompt_note();
    assert!(note.contains("- anysearch (1 tool)\n"), "{note}");
    assert!(
        note.contains("- github (2 tools) — Manage GitHub issues, pull requests, and code.\n"),
        "{note}"
    );
    assert!(note.contains("search_tool"), "{note}");
    assert!(note.contains("use_tool"), "{note}");
    assert!(!note.contains("create_issue"), "{note}");
}

/// A server still handshaking is named as connecting, never as ready: its
/// tools are not on the tool list yet.
#[test]
fn a_connecting_server_is_named_in_the_prompt_note() {
    let catalog = McpCatalog::new(None, native(&[]), ["gh".to_owned(), "fs".to_owned()]);
    let note = catalog.prompt_note();
    assert!(
        note.contains("MCP servers connecting in the background: fs, gh."),
        "{note}"
    );
    assert!(!note.contains("Connected MCP servers"), "{note}");
    assert!(note.contains("not on the tool list yet"), "{note}");
}

/// A tool whose bare name is already a registry tool keeps its server
/// prefix, so `use_tool` never reads as the other tool.
#[test]
fn a_tool_named_like_a_builtin_keeps_its_server_prefix() {
    let tools = vec![
        catalog_tool("files", "read", "Read a remote file."),
        catalog_tool("files", "stat", "Stat a remote file."),
    ];
    let catalog = catalog_from(tools, None, &native(&["read"]));
    let listed = catalog.list_text();
    assert!(
        listed.contains("files__read — Read a remote file."),
        "{listed}"
    );
    assert!(listed.contains("stat — Stat a remote file."), "{listed}");
    assert_eq!(
        catalog.get("read").map(|(key, _)| key),
        Some("files__read".to_owned())
    );
}

#[test]
fn keyword_search_matches_names_servers_and_descriptions() {
    let tools = vec![
        catalog_tool("github", "create_issue", "Open a new GitHub issue."),
        catalog_tool("github", "list_pulls", "List pull requests."),
        catalog_tool("linear", "create_ticket", "Open a Linear issue."),
    ];
    let catalog = catalog_from(tools, None, &native(&[]));
    let keys = |query: &str| -> Vec<String> {
        catalog
            .search(query)
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    };
    assert_eq!(keys("github issue"), ["create_issue"]);
    assert_eq!(keys("ISSUE"), ["create_issue", "create_ticket"]);
    assert_eq!(keys("pull, github"), ["list_pulls"]);
    assert!(keys("deploy").is_empty());
    assert!(keys("  ").is_empty());
}

/// Runs one tool call that must fail and returns its `InvalidArgs` text.
async fn invalid_args_text(tool: &dyn ToolDyn, args: serde_json::Value) -> String {
    let root = std::env::temp_dir().join(format!(
        "mycode-mcp-nudge-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let ctx = mycode_agent::ToolCtx::new(&root);
    let (mut stream, rx) = mycode_agent::ToolStream::channel();
    let error = tool
        .execute_dyn(args, &ctx, &mut stream)
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

/// A registry tool name that reaches `search_tool` / `use_tool` is told to
/// call the tool directly, in either case; an unknown name keeps the
/// generic errors.
#[tokio::test]
async fn a_native_name_miss_gets_a_call_it_directly_nudge() {
    let catalog = catalog_from(
        vec![catalog_tool("files", "stat", "Stat a remote file.")],
        None,
        &native(&["read"]),
    );
    let use_tool = UseTool::new(Arc::clone(&catalog));
    for spelling in ["read", "Read"] {
        let error = invalid_args_text(
            &use_tool,
            serde_json::json!({"name": spelling, "arguments": {}}),
        )
        .await;
        assert!(
            error.contains("already on the tool list"),
            "{spelling}: {error}"
        );
        assert!(error.contains("directly"), "{spelling}: {error}");
    }
    let unknown = invalid_args_text(
        &use_tool,
        serde_json::json!({"name": "deploy", "arguments": {}}),
    )
    .await;
    assert!(unknown.contains("no MCP tool named deploy"), "{unknown}");

    let search_tool = super::SearchTool::new(Arc::clone(&catalog));
    // The empty-keyword-search branch nudges; a broad keyword with matches
    // stays an ordinary result.
    let error = invalid_args_text(&search_tool, serde_json::json!({"name": "Read"})).await;
    assert!(error.contains("already on the tool list"), "{error}");
    let unknown = invalid_args_text(&search_tool, serde_json::json!({"name": "deploy"})).await;
    assert!(unknown.contains("no MCP tool matches deploy"), "{unknown}");
    let text = mycode_agent::ToolDyn::execute_dyn(
        &search_tool,
        serde_json::json!({"name": "stat"}),
        &mycode_agent::ToolCtx::new(std::path::Path::new(".")),
        &mut mycode_agent::ToolStream::closed(),
    )
    .await;
    assert!(text.is_ok(), "an exact MCP name still resolves: {text:?}");

    let batch = catalog.schemas_text(&["read".to_owned(), "deploy".to_owned()]);
    assert!(batch.contains("already on the tool list"), "{batch}");
    assert!(batch.contains("no MCP tool has this name"), "{batch}");
}

/// While a server is still connecting, a `use_tool` miss says why instead
/// of claiming the name does not exist.
#[tokio::test]
async fn use_tool_reports_a_still_connecting_server() {
    let catalog = McpCatalog::new(None, native(&[]), ["gh".to_owned()]);
    let use_tool = UseTool::new(catalog);
    let error = invalid_args_text(
        &use_tool,
        serde_json::json!({"name": "gh__issue_get", "arguments": {}}),
    )
    .await;
    assert!(error.contains("still connecting"), "{error}");
    assert!(error.contains("gh"), "{error}");
}

/// The native-name snapshot is taken before the meta-tools register, so
/// sending `search_tool` / `use_tool` to themselves keeps the generic
/// error instead of a nudge.
#[tokio::test]
async fn the_nudge_snapshot_excludes_the_meta_tools() {
    let registry = mycode_agent::ToolRegistry::new();
    mycode_agent::register_builtins(&registry);
    let catalog = catalog_from(
        vec![catalog_tool("files", "stat", "Stat a remote file.")],
        None,
        &native(&["read"]),
    );
    let index = ToolSearchIndex::new();
    register_meta_tools(&registry, &catalog, Arc::clone(&index));
    for meta in ["search_tool", "use_tool", "tool_search"] {
        assert!(registry.get(meta).is_some(), "{meta} registered");
    }
    for meta in ["search_tool", "use_tool"] {
        let tool = registry.get(meta).expect("the meta-tool registered");
        let error = invalid_args_text(
            tool.as_ref(),
            serde_json::json!({"name": meta, "arguments": {}}),
        )
        .await;
        // `use_tool` misses by name, `search_tool` falls through to its
        // empty keyword search; both keep their generic error.
        assert!(error.contains("no MCP tool"), "{meta}: {error}");
        assert!(
            !error.contains("already on the tool list"),
            "{meta}: {error}"
        );
    }
}

/// The register path gives a native collision the same server prefix the
/// closure-based check used to, and writes the catalog before the registry.
#[test]
fn register_level_collision_keeps_the_server_prefix() {
    let registry = mycode_agent::ToolRegistry::new();
    registry.register(Arc::new(mycode_agent::builtin::ReadTool));
    let catalog = McpCatalog::new(None, native(&["read"]), Vec::new());
    let index = ToolSearchIndex::new();
    register_ready_server(
        &registry,
        &catalog,
        &index,
        server_info("files", None),
        vec![
            catalog_tool("files", "read", "Read a remote file."),
            catalog_tool("files", "stat", "Stat a remote file."),
        ],
    );
    let listed = catalog.list_text();
    assert!(
        listed.contains("files__read — Read a remote file."),
        "{listed}"
    );
    assert!(listed.contains("stat — Stat a remote file."), "{listed}");
    assert!(
        catalog.get("files__read").is_some(),
        "the catalog holds the key"
    );
    let registered = registry.get("files__read").expect("registry mirrors the key");
    assert_eq!(registered.spec().name, "files__read");
    // The builtin keeps its own name.
    assert_eq!(registry.get("read").expect("builtin").spec().name, "read");
    // The BM25 index grew with the same ready server.
    assert_eq!(index.len(), 2);
}

#[tokio::test]
async fn tool_search_returns_ready_schemas() {
    let registry = mycode_agent::ToolRegistry::new();
    let catalog = McpCatalog::new(None, native(&[]), Vec::new());
    let index = ToolSearchIndex::new();
    register_ready_server(
        &registry,
        &catalog,
        &index,
        server_info("github", None),
        vec![
            catalog_tool("github", "create_issue", "Open a GitHub issue."),
            catalog_tool("github", "list_pulls", "List pull requests."),
        ],
    );
    let search = ToolSearchTool::new(Arc::clone(&catalog), Arc::clone(&index));
    let result = mycode_agent::ToolDyn::execute_dyn(
        &search,
        json!({"query": "github issue"}),
        &mycode_agent::ToolCtx::new(std::path::Path::new(".")),
        &mut mycode_agent::ToolStream::closed(),
    )
    .await
    .expect("tool_search runs");
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_agent::message::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(text.contains("create_issue"), "{text}");
    let specs: Value = serde_json::from_str(&text).expect("tool_search returns JSON");
    assert!(specs.as_array().is_some_and(|specs| specs.len() <= 8));
    assert!(
        specs[0]["params_schema"].is_object(),
        "the schema is returned: {text}"
    );
}

fn server_row(id: &str) -> mycode_agent::McpServerSettings {
    mycode_agent::McpServerSettings {
        id: id.to_owned(),
        enabled: true,
        transport: "stdio".to_owned(),
        command: None,
        args: Vec::new(),
        env: Default::default(),
        endpoint: None,
        key_header: None,
        headers: Default::default(),
    }
}

fn resolved(fingerprint: u64) -> super::ResolvedMcp {
    super::ResolvedMcp {
        fingerprint,
        servers: vec![super::ResolvedServer {
            settings: server_row("gh"),
            api_key: None,
        }],
        warning: None,
    }
}

/// The same fingerprint reuses a ready slot instead of starting a second
/// handshake; a changed fingerprint drops the pool.
#[tokio::test]
async fn a_pool_reuses_ready_slots_across_turns() {
    let pool = Arc::new(tokio::sync::Mutex::new(None));
    let dir = Arc::new(std::env::temp_dir());
    let first = super::spawn_slots(resolved(7), &pool, Arc::clone(&dir)).await;
    assert_eq!(first.connecting, ["gh"], "the first call starts a handshake");

    // Pretend the handshake finished.
    let info = server_info("gh", Some("GitHub."));
    let tool = tool_on(&info, "issue_get", "Get an issue.");
    {
        let mut guard = pool.lock().await;
        let pool_ref = guard.as_mut().expect("pool");
        pool_ref.slots[0] = McpSlot::Ready {
            server: Arc::clone(&info),
            tools: vec![tool],
            broken: Arc::new(AtomicBool::new(false)),
        };
    }
    let second = super::spawn_slots(resolved(7), &pool, Arc::clone(&dir)).await;
    assert_eq!(second.ready.len(), 1, "the ready slot is reused");
    assert!(second.connecting.is_empty(), "{:?}", second.connecting);

    let third = super::spawn_slots(resolved(8), &pool, Arc::clone(&dir)).await;
    assert!(
        third.ready.is_empty(),
        "a new fingerprint drops the old pool"
    );
    assert_eq!(third.connecting, ["gh"]);
    let mut guard = pool.lock().await;
    if let Some(pool_ref) = guard.as_mut() {
        pool_ref.abort_connecting();
    }
}

/// The drain coroutine hands each finished server to its callback exactly
/// once, and a server already recorded in `seen` is not repeated.
#[tokio::test]
async fn the_drain_reports_a_server_once_when_its_slot_becomes_ready() {
    let pool = Arc::new(tokio::sync::Mutex::new(None));
    let info = server_info("gh", Some("GitHub."));
    let tool = tool_on(&info, "issue_get", "Get an issue.");
    {
        let mut guard = pool.lock().await;
        *guard = Some(super::McpPool::new(
            11,
            &[super::ResolvedServer {
                settings: server_row("gh"),
                api_key: None,
            }],
            None,
        ));
        let pool_ref = guard.as_mut().expect("pool");
        let notify = Arc::clone(&pool_ref.notify);
        let task_pool = Arc::clone(&pool);
        let task_info = Arc::clone(&info);
        let task_tool = Arc::clone(&tool);
        pool_ref.slots[0] = McpSlot::Connecting {
            server: Arc::clone(&info),
            handle: tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let mut guard = task_pool.lock().await;
                if let Some(pool_ref) = guard.as_mut() {
                    pool_ref.slots[0] = McpSlot::Ready {
                        server: task_info,
                        tools: vec![task_tool],
                        broken: Arc::new(AtomicBool::new(false)),
                    };
                }
                drop(guard);
                notify.notify_waiters();
            }),
        };
    }

    let recorded: Arc<std::sync::Mutex<Vec<(String, usize)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        super::drain_mcp_ready(
            Arc::clone(&pool),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            None,
            move |server, tools| {
                sink.lock()
                    .unwrap()
                    .push((server.id().to_owned(), tools.len()));
            },
            |_, _| {},
        ),
    )
    .await
    .expect("the drain finishes once the handshake slot is ready");
    assert_eq!(
        *recorded.lock().unwrap(),
        [("gh".to_owned(), 1)],
        "one ready server, reported once"
    );

    // A server already in `seen` is not reported again (a second turn's
    // drain starts with the ready servers it already registered).
    let repeated = Arc::new(std::sync::Mutex::new(0_usize));
    let sink = Arc::clone(&repeated);
    super::drain_mcp_ready(
        Arc::clone(&pool),
        ["gh".to_owned()].into_iter().collect(),
        std::collections::HashSet::new(),
        None,
        move |_, _| *sink.lock().unwrap() += 1,
        |_, _| {},
    )
    .await;
    assert_eq!(*repeated.lock().unwrap(), 0);
}

/// A handshake that fails after the turn started is reported to the catalog:
/// the server stops reading as "still connecting".
#[tokio::test]
async fn the_drain_reports_a_handshake_failure() {
    let pool = Arc::new(tokio::sync::Mutex::new(None));
    let info = server_info("gh", None);
    {
        let mut guard = pool.lock().await;
        *guard = Some(super::McpPool::new(
            12,
            &[super::ResolvedServer {
                settings: server_row("gh"),
                api_key: None,
            }],
            None,
        ));
        let pool_ref = guard.as_mut().expect("pool");
        let notify = Arc::clone(&pool_ref.notify);
        let task_pool = Arc::clone(&pool);
        pool_ref.slots[0] = McpSlot::Connecting {
            server: Arc::clone(&info),
            handle: tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let mut guard = task_pool.lock().await;
                if let Some(pool_ref) = guard.as_mut() {
                    pool_ref.slots[0] = McpSlot::Failed {
                        server: info,
                        reason: "handshake timed out".to_owned(),
                        retry_at: std::time::Instant::now() + super::MCP_RETRY_COOLDOWN,
                    };
                }
                drop(guard);
                notify.notify_waiters();
            }),
        };
    }

    let reported: Arc<std::sync::Mutex<Vec<(String, String)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&reported);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        super::drain_mcp_ready(
            Arc::clone(&pool),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            None,
            |_, _| panic!("a failed server must not be reported as ready"),
            move |server, reason| {
                sink.lock()
                    .unwrap()
                    .push((server.id().to_owned(), reason.to_owned()));
            },
        ),
    )
    .await
    .expect("the drain finishes once the handshake slot failed");
    assert_eq!(
        *reported.lock().unwrap(),
        [("gh".to_owned(), "handshake timed out".to_owned())]
    );
}
