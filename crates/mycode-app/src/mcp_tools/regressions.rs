//! Regression tests for catalog identity, discovery and execution contracts.

use std::collections::BTreeSet;
use std::sync::Arc;

use mycode_agent::{ToolCtx, ToolDyn, ToolRegistry, ToolStream};
use serde_json::json;

use super::{
    McpCatalog, McpServerInfo, register_meta_tools, register_ready_server, stub_catalog_tool,
};
use crate::tool_search::ToolSearchIndex;

#[test]
fn mcp_names_cannot_replace_bridge_tools_or_prefixed_native_tools() {
    let registry = ToolRegistry::new();
    registry.register(stub_catalog_tool("native", "files__read", "Native plugin."));
    let catalog = McpCatalog::new(
        None,
        BTreeSet::from(["read".to_owned(), "files__read".to_owned()]),
        [],
    );
    let index = ToolSearchIndex::new();
    register_meta_tools(&registry, &catalog, Arc::clone(&index));
    let meta = registry.get("search_tool").unwrap();
    let native = registry.get("files__read").unwrap();
    register_ready_server(
        &registry,
        &catalog,
        &index,
        Arc::new(McpServerInfo::new("files", None)),
        vec![
            stub_catalog_tool("files", "search_tool", "Remote search."),
            stub_catalog_tool("files", "read", "Remote read."),
        ],
    );
    assert!(
        Arc::ptr_eq(&meta, &registry.get("search_tool").unwrap()),
        "discovery bridge must remain reachable"
    );
    assert!(
        Arc::ptr_eq(&native, &registry.get("files__read").unwrap()),
        "a prefixed collision must not replace a native tool"
    );
    assert_eq!(index.len(), 2, "colliding remote tools remain discoverable");
    assert_eq!(
        catalog.get("files__search_tool").unwrap().1.server_id(),
        "files"
    );
    let (key, _) = catalog
        .get("files.read")
        .expect("qualified lookup finds the collision-safe key");
    assert_ne!(key, "files__read");
    assert!(registry.get(&key).unwrap().schema_on_demand());
    assert!(
        !meta.schema_on_demand(),
        "bridge schemas remain in the code-mode prompt"
    );
}

#[test]
fn a_qualified_name_never_falls_back_to_a_different_server() {
    let catalog = McpCatalog::new(None, BTreeSet::new(), ["beta".to_owned()]);
    catalog.insert_ready(
        Arc::new(McpServerInfo::new("alpha", None)),
        vec![stub_catalog_tool("alpha", "search", "Search alpha.")],
    );
    for name in [
        "beta.search",
        "beta:search",
        "beta/search",
        "beta__search",
        "mcp__beta__search",
    ] {
        assert!(
            catalog.get(name).is_none(),
            "{name} must not execute alpha.search"
        );
    }
    assert_eq!(catalog.get("ALPHA.SEARCH").unwrap().1.server_id(), "alpha");
    // An exact raw tool name containing punctuation stays valid.
    catalog.insert_ready(
        Arc::new(McpServerInfo::new("literal", None)),
        vec![stub_catalog_tool(
            "literal",
            "api.search",
            "A literal name.",
        )],
    );
    assert_eq!(catalog.get("api.search").unwrap().1.server_id(), "literal");
}

#[test]
fn an_exact_case_insensitive_key_wins_over_a_bare_name_alias() {
    let catalog = McpCatalog::new(None, BTreeSet::new(), []);
    catalog.insert_ready(
        Arc::new(McpServerInfo::new("alpha", None)),
        vec![stub_catalog_tool("alpha", "search", "Search alpha.")],
    );
    catalog.insert_ready(
        Arc::new(McpServerInfo::new("beta", None)),
        vec![stub_catalog_tool("beta", "search", "Search beta.")],
    );
    assert_eq!(catalog.get("SEARCH").unwrap().1.server_id(), "alpha");
}

#[test]
fn listing_failed_servers_includes_the_reason() {
    let catalog = McpCatalog::new(None, BTreeSet::new(), ["docs".to_owned()]);
    catalog.mark_failed(
        Arc::new(McpServerInfo::new("docs", None)),
        "missing command",
    );
    let listed = catalog.list_text();
    assert!(
        listed.contains("docs") && listed.contains("missing command"),
        "{listed}"
    );
}

#[tokio::test]
async fn handshake_errors_keep_the_actual_failure() {
    let server = super::tests::server_row("broken");
    let error = super::handshake(&server, None, &Arc::new(std::env::temp_dir()))
        .await
        .err()
        .expect("missing command fails");
    assert!(error.contains("missing its command"), "{error}");
}

#[tokio::test]
async fn code_mode_discovery_notes_teach_the_registered_call_contract() {
    let registry = ToolRegistry::new();
    registry.register(Arc::new(mycode_agent::builtin::ReadTool));
    // The app exposes only shell/run_code/wait_code in this mode.
    registry.expose_only(&["run_code", "wait_code"]);
    let catalog = McpCatalog::new(
        None,
        BTreeSet::from(["read".to_owned()]),
        ["other".to_owned()],
    );
    let index = ToolSearchIndex::new();
    register_meta_tools(&registry, &catalog, Arc::clone(&index));
    register_ready_server(
        &registry,
        &catalog,
        &index,
        Arc::new(McpServerInfo::new("docs", None)),
        vec![stub_catalog_tool("docs", "lookup", "Look up docs.")],
    );
    let error = super::UseTool::new(Arc::clone(&catalog))
        .execute_dyn(
            json!({"name": "read", "arguments": {}}),
            &ToolCtx::new(std::env::temp_dir()),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("call(\"read\""), "{error}");
    let note = catalog.prompt_note();
    assert!(note.contains("run_code"), "{note}");
    assert!(!note.contains("Their tools are on the tool list"), "{note}");
    assert!(!note.contains("once connected they appear there"), "{note}");
}

#[tokio::test]
async fn discovery_and_use_tool_work_inside_a_javascript_script() {
    let registry = Arc::new(ToolRegistry::new());
    let catalog = McpCatalog::new(None, BTreeSet::new(), []);
    let index = ToolSearchIndex::new();
    register_meta_tools(&registry, &catalog, Arc::clone(&index));
    register_ready_server(
        &registry,
        &catalog,
        &index,
        Arc::new(McpServerInfo::new("docs", None)),
        vec![super::tests::catalog_tool(
            "docs",
            "lookup",
            "Look up docs.",
        )],
    );
    let tool = mycode_agent::RunCodeTool::new(
        Arc::clone(&registry),
        Arc::new(mycode_agent::CodeCells::new()),
        "s".to_owned(),
    );
    let result = tool
        .execute_dyn(
            json!({"script": r#"
let schema = call("search_tool", { name: "lookup" });
if (schema.is_error) { throw "discovery failed"; }
let found = call("tool_search", { query: "docs lookup" });
if (found.is_error) { throw "ranking failed"; }
let result = call("use_tool", { name: "lookup", arguments: {} });
complete(result);
"#}),
            &ToolCtx::new(std::env::temp_dir()),
            &mut ToolStream::closed(),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_agent::message::ContentBlock::Text(block) => Some(block.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let output: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(output["is_error"], false);
    assert!(text.contains("pong"), "{text}");
    assert_eq!(result.details.unwrap()["calls"], 3);
}
