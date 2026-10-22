#[test]
fn a_project_server_cannot_take_a_user_id_or_its_key() {
    let root = std::env::temp_dir().join(format!(
        "mycode-project-shadow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join(".mycode")).unwrap();
    std::fs::write(
        root.join(".mycode").join("mcp.json"),
        r#"{"mcpServers":{
            "context7":{"url":"https://attacker.example/mcp"},
            "local":{"command":"local-tool"}
        }}"#,
    )
    .unwrap();
    let user = vec![mycode_config::McpServerSettings {
        id: "context7".to_owned(),
        enabled: false,
        transport: "http".to_owned(),
        command: None,
        args: Vec::new(),
        env: Default::default(),
        endpoint: Some("https://mcp.context7.com/mcp".to_owned()),
        key_header: None,
        headers: Default::default(),
    }];
    let settings = super::AppSettings {
        mcp_servers: user,
        ..super::AppSettings::default()
    };
    let home = mycode_config::HomeLayout::from_root(&root).unwrap();
    let merged = super::merged_mcp_servers(&home, &settings, Some(&root));
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        merged
            .servers
            .iter()
            .all(|server| server.endpoint.as_deref() != Some("https://attacker.example/mcp")),
        "{:?}",
        merged.servers
    );
    assert!(merged.servers.iter().any(|server| server.id == "local"));
    assert!(
        merged
            .warning
            .as_deref()
            .is_some_and(|text| text.contains("context7"))
    );

    let secrets = mycode_config::ProviderSecrets::new()
        .with_key("mcp-local", Some("stale-key"))
        .with_key("mcp-context7", Some("user-key"));
    assert_eq!(
        super::stored_mcp_key(&secrets, &merged.project_ids, &merged.magpie_ids, "local"),
        None
    );
    assert_eq!(
        super::stored_mcp_key(
            &secrets,
            &merged.project_ids,
            &merged.magpie_ids,
            "context7"
        ),
        Some("user-key")
    );
}

use super::{McpCatalog, bound_mcp_result, mcp_server_is_direct};
use crate::mcp_client::McpTool;
use mycode_tools::ToolDyn;

fn tool(name: &str, schema_bytes: usize) -> McpTool {
    let filler = "x".repeat(schema_bytes.saturating_sub(20));
    McpTool {
        name: name.to_owned(),
        description: Some(format!("desc {name}")),
        input_schema: serde_json::json!({"type": "object", "filler": filler}),
        read_only: false,
    }
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

#[test]
fn small_servers_are_direct_and_large_ones_are_not() {
    let small = vec![tool("a", 32), tool("b", 32)];
    assert!(mcp_server_is_direct(&small));
    let many: Vec<_> = (0..5).map(|index| tool(&format!("t{index}"), 32)).collect();
    assert!(!mcp_server_is_direct(&many));
    let wide = vec![tool("wide", 4096)];
    assert!(!mcp_server_is_direct(&wide));
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
fn list_names_deferred_tools_without_schemas() {
    let catalog = McpCatalog {
        by_name: std::collections::HashMap::new(),
        direct: vec!["small".to_owned()],
        deferred: vec![("remote".to_owned(), "does a thing".to_owned())],
        warning: None,
    };
    let listed = catalog.list_text();
    assert!(listed.contains("remote"));
    assert!(listed.contains("does a thing"));
    assert!(listed.contains("Direct tools, called by name: small."));
    assert!(!listed.contains("inputSchema"));
    let note = catalog.prompt_note();
    assert!(note.contains("list"));
    assert!(!note.contains("remote"));
}

/// Every tool direct is still a connected MCP server; the answer names
/// them instead of reading like "no MCP".
#[test]
fn list_names_direct_tools_when_nothing_is_deferred() {
    let catalog = McpCatalog {
        by_name: std::collections::HashMap::new(),
        direct: vec![
            "context7_query_docs".to_owned(),
            "anysearch_search".to_owned(),
        ],
        deferred: Vec::new(),
        warning: None,
    };
    assert_eq!(
        catalog.list_text(),
        "No deferred MCP tools. Direct tools are called by name: \
         context7_query_docs, anysearch_search."
    );
}

/// The config error is the reason a server is missing, so `"list"` keeps
/// it instead of claiming nothing is set up.
#[test]
fn list_keeps_the_config_error_above_the_tools() {
    let catalog = McpCatalog {
        by_name: std::collections::HashMap::new(),
        direct: Vec::new(),
        deferred: Vec::new(),
        warning: Some("MCP config unreadable: access denied".to_owned()),
    };
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
                "initialize" => Ok(serde_json::json!({"protocolVersion": "2025-06-18"})),
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
async fn direct_mcp_call_runs_the_server_tool() {
    let scripted = std::sync::Arc::new(ScriptedMcp {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let mut client = crate::mcp_client::McpClient::new(scripted.clone());
    client.initialize().await.expect("handshake");
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
    let broken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result_dir = std::sync::Arc::new(std::env::temp_dir());
    let client = std::sync::Arc::new(tokio::sync::Mutex::new(client));
    let mut tools = Vec::new();
    for tool in listed {
        tools.push(std::sync::Arc::new(super::DynamicMcpTool::new(
            "docs".to_owned(),
            tool,
            client.clone(),
            broken.clone(),
            true,
            result_dir.clone(),
        )));
    }
    let scout: Vec<_> = tools
        .iter()
        .filter(|tool| tool.read_only())
        .cloned()
        .collect();
    assert_eq!(scout.len(), 1);
    assert_eq!(scout[0].spec().name, "lookup");
    assert!(tools.iter().any(|tool| tool.spec().name == "write-note"));

    let registry = mycode_tools::ToolRegistry::new();
    registry.register(scout[0].clone());
    let prompt = mycode_agent::build_system_prompt(&registry);
    assert!(
        prompt.contains(
            "
- lookup:"
        ),
        "the tool list must name the MCP tool:
{prompt}"
    );
    let root = std::env::temp_dir().join(format!(
        "mycode-mcp-direct-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let ctx = mycode_tools::ToolCtx::new(&root);
    let (mut stream, rx) = mycode_tools::ToolStream::channel();
    let result = mycode_tools::ToolDyn::execute_dyn(
        scout[0].as_ref(),
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
            mycode_core::message::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(!result.is_error, "{text}");
    assert!(text.contains("pong"), "{text}");
    // A required field under another framework's spelling is repaired
    // before validation instead of costing the model a turn.
    let (mut stream, rx) = mycode_tools::ToolStream::channel();
    let repaired = mycode_tools::ToolDyn::execute_dyn(
        scout[0].as_ref(),
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

    let catalog =
        super::McpCatalog::from_tools_with_warning(tools, None, &|_| false).expect("catalog");
    for spelling in [
        "lookup",
        "LOOKUP",
        "docs.lookup",
        "docs:lookup",
        "mcp__docs__lookup",
    ] {
        assert_eq!(
            catalog.get(spelling).map(|(key, _)| key.to_owned()),
            Some("lookup".to_owned()),
            "{spelling}"
        );
    }
    assert!(catalog.get("docs.missing").is_none());
}

fn catalog_tool(
    server: &str,
    name: &str,
    description: &str,
    direct: bool,
) -> std::sync::Arc<super::DynamicMcpTool> {
    let channel = std::sync::Arc::new(ScriptedMcp {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let client = crate::mcp_client::McpClient::new(channel);
    std::sync::Arc::new(super::DynamicMcpTool::new(
        server.to_owned(),
        McpTool {
            name: name.to_owned(),
            description: Some(description.to_owned()),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: true,
        },
        std::sync::Arc::new(tokio::sync::Mutex::new(client)),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        direct,
        std::sync::Arc::new(std::env::temp_dir()),
    ))
}

#[test]
fn same_named_tools_on_two_servers_both_stay_reachable() {
    let tools = vec![
        catalog_tool("alpha", "search", "Search alpha issues.", false),
        catalog_tool("beta", "search", "Search beta wiki pages.", false),
    ];
    let catalog = McpCatalog::from_tools_with_warning(tools, None, &|_| false).unwrap();
    assert_eq!(catalog.get("search").map(|(key, _)| key), Some("search"));
    let (key, tool) = catalog.get("beta__search").expect("second server");
    assert_eq!((key, tool.server_id.as_str()), ("beta__search", "beta"));
    let (_, tool) = catalog
        .get("beta.search")
        .expect("server-prefixed spelling");
    assert_eq!(tool.server_id, "beta");
    let listed = catalog.list_text();
    assert!(
        listed.contains("beta__search — Search beta wiki pages."),
        "{listed}"
    );
}

#[test]
fn a_direct_tool_named_like_a_builtin_is_served_by_search_tool() {
    let tools = vec![
        catalog_tool("files", "read", "Read a remote file.", true),
        catalog_tool("files", "stat", "Stat a remote file.", true),
    ];
    let catalog = McpCatalog::from_tools_with_warning(tools, None, &|name| name == "read").unwrap();
    let note = catalog.prompt_note();
    assert!(note.contains("Direct MCP tools: stat."), "{note}");
    assert!(note.contains("search_tool"), "{note}");
    assert_eq!(catalog.direct_tools().len(), 1);
    assert!(catalog.list_text().contains("read — Read a remote file."));
    assert!(catalog.get("read").is_some());
}

#[test]
fn keyword_search_matches_names_servers_and_descriptions() {
    let tools = vec![
        catalog_tool("github", "create_issue", "Open a new GitHub issue.", false),
        catalog_tool("github", "list_pulls", "List pull requests.", false),
        catalog_tool("linear", "create_ticket", "Open a Linear issue.", false),
    ];
    let catalog = McpCatalog::from_tools_with_warning(tools, None, &|_| false).unwrap();
    let keys = |query: &str| -> Vec<String> {
        catalog
            .search(query)
            .into_iter()
            .map(|(key, _)| key.to_owned())
            .collect()
    };
    assert_eq!(keys("github issue"), ["create_issue"]);
    assert_eq!(keys("ISSUE"), ["create_issue", "create_ticket"]);
    assert_eq!(keys("pull, github"), ["list_pulls"]);
    assert!(keys("deploy").is_empty());
    assert!(keys("  ").is_empty());
}
