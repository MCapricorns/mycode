//! Bridges enabled MCP servers into the agent's tool registry.
//!
//! Servers are connected at turn start. A small server is registered on
//! the model tool list. Larger servers stay behind `search_tool` and
//! `use_tool`. A failed server is skipped.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mycode_config::AppSettings;
use mycode_core::ToolSpec;
use mycode_tools::{ToolCtx, ToolDyn, ToolError, ToolResult, ToolStream};
use serde_json::Value;

/// One MCP tool exposed by a connected server, registered as a raw-JSON tool.
///
/// Implements `ToolDyn` directly instead of `Tool` because the server's
/// `inputSchema` must reach the model verbatim through `spec()`; the blanket
/// `impl<T: Tool> ToolDyn` would advertise the schemars schema of
/// `Args = serde_json::Value`, which is just `true`.
pub(crate) struct DynamicMcpTool {
    server_id: String,
    tool_name: String,
    /// Built once. `spec()` clones this instead of rebuilding the schema.
    spec: ToolSpec,
    /// Compiled once from the server's schema; `None` when it does not
    /// compile — args then pass through and the server's rejection is the
    /// backstop.
    validator: Option<jsonschema::Validator>,
    client: Arc<tokio::sync::Mutex<crate::mcp_client::McpClient>>,
    snippet: String,
    /// Set when the shared connection dies so the next turn reconnects.
    broken: Arc<AtomicBool>,
    /// Small servers are registered as ordinary tools. Larger ones stay
    /// reachable through `search_tool`.
    direct: bool,
    /// Directory that stores full text when a tool result is truncated.
    /// Shared by every tool from one connect, so each tool does not copy the path.
    result_dir: Arc<std::path::PathBuf>,
}

impl DynamicMcpTool {
    fn new(
        server_id: String,
        tool: crate::mcp_client::McpTool,
        client: Arc<tokio::sync::Mutex<crate::mcp_client::McpClient>>,
        broken: Arc<AtomicBool>,
        direct: bool,
        result_dir: Arc<std::path::PathBuf>,
    ) -> Self {
        let validator = jsonschema::validator_for(&tool.input_schema).ok();
        let snippet = format!(
            "MCP tool on server '{server_id}'. Call it when the task matches; do not wait to be asked."
        );
        let spec = ToolSpec {
            name: tool.name.clone(),
            description: tool
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool '{}' on server '{server_id}'.", tool.name)),
            params_schema: tool.input_schema,
        };
        Self {
            server_id,
            tool_name: tool.name,
            spec,
            validator,
            client,
            snippet,
            broken,
            direct,
            result_dir,
        }
    }
}

#[async_trait::async_trait]
impl ToolDyn for DynamicMcpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some(self.snippet.as_str())
    }

    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if let Some(validator) = &self.validator {
            let errors: Vec<String> = validator
                .iter_errors(&args)
                .map(|error| format!("{error} (at {})", error.instance_path()))
                .collect();
            if !errors.is_empty() {
                return Err(ToolError::InvalidArgs(errors.join("; ")));
            }
        }
        out.progress(format!(
            "calling {} on MCP server {}",
            self.tool_name, self.server_id
        ));
        let mut client = self.client.lock().await;
        // The dispatcher does not select on the turn token while a tool
        // runs; bound the call here so Escape actually cancels an MCP call.
        let call = client.call_tool(&self.tool_name, args);
        let output = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => {
                return Err(ToolError::Execution("MCP call cancelled".to_owned()));
            }
            result = call => result.map_err(|error| {
                if error.is_connection_lost() {
                    self.broken.store(true, Ordering::Relaxed);
                }
                let detail = if error.is_connection_lost() {
                    format!(
                        "MCP server '{}' lost the connection: {error}",
                        self.server_id
                    )
                } else {
                    format!("MCP server '{}': {error}", self.server_id)
                };
                ToolError::Execution(detail)
            })?,
        };
        // A server that flags `isError` ran the tool and reported failure as
        // data. The model needs to read it to correct its arguments, so it
        // becomes an error *result* rather than a dispatcher failure.
        let text = bound_mcp_result(&self.result_dir, &output.text);
        Ok(if output.is_error {
            ToolResult::error(text)
        } else {
            ToolResult::text(text)
        })
    }
}

/// Bytes of an MCP tool result kept inline for the model.
const MAX_MCP_TOOL_RESULT_BYTES: usize = 20 * 1024;

/// Truncates oversized tool text and stores the full body beside the home.
pub(crate) fn bound_mcp_result(dir: &std::path::Path, text: &str) -> String {
    if text.len() <= MAX_MCP_TOOL_RESULT_BYTES {
        return text.to_owned();
    }
    let id = format!(
        "mcp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    let path = dir.join(format!("{id}.txt"));
    let stored = match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, text)) {
        Ok(()) => format!("full text stored at {}", path.display()),
        Err(error) => format!("full text could not be stored: {error}"),
    };
    let head_end = char_floor(text, MAX_MCP_TOOL_RESULT_BYTES / 2);
    let tail_start = char_ceil_from_end(text, MAX_MCP_TOOL_RESULT_BYTES / 2);
    format!(
        "{}\n\n[truncated; {stored}]\n\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

fn char_floor(text: &str, index: usize) -> usize {
    let mut end = index.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn char_ceil_from_end(text: &str, keep: usize) -> usize {
    let mut start = text.len().saturating_sub(keep);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// A server is exposed directly when it is small and its schemas are small.
pub(crate) fn mcp_server_is_direct(tools: &[crate::mcp_client::McpTool]) -> bool {
    const MAX_DIRECT_TOOLS: usize = 4;
    const MAX_DIRECT_SCHEMA_BYTES: usize = 2048;
    tools.len() <= MAX_DIRECT_TOOLS
        && tools.iter().all(|tool| {
            serde_json::to_vec(&tool.input_schema)
                .map(|bytes| bytes.len() <= MAX_DIRECT_SCHEMA_BYTES)
                .unwrap_or(false)
        })
}

/// Live MCP clients reused across turns until settings change or a
/// connection dies.
pub(crate) struct McpPool {
    fingerprint: u64,
    broken: Arc<AtomicBool>,
    tools: Vec<Arc<DynamicMcpTool>>,
    warning: Option<String>,
}

/// Connects every enabled MCP server and flattens its tools.
///
/// A matching live pool is reused only when every enabled server connected.
/// A server that fails to spawn, handshake, or list tools is skipped for
/// this turn and is not cached, so the next turn tries it again. Runs
/// inside the spawned turn task on the single-threaded core runtime —
/// plain `.await` only, never `block_on`.
pub(crate) async fn connect_mcp_tools(
    home: &mycode_config::HomeLayout,
    settings: &AppSettings,
    pool: &tokio::sync::Mutex<Option<McpPool>>,
    project: Option<&std::path::Path>,
) -> (Vec<Arc<DynamicMcpTool>>, Option<String>) {
    let Ok(secrets) = mycode_config::read_provider_secrets(home) else {
        return (Vec::new(), None);
    };
    let (servers, project_warning) = merged_mcp_servers(home, settings, project);
    let fingerprint = mcp_fingerprint(&servers, &secrets);
    {
        let guard = pool.lock().await;
        if let Some(cached) = guard.as_ref()
            && cached.fingerprint == fingerprint
            && !cached.broken.load(Ordering::Relaxed)
        {
            return (cached.tools.clone(), cached.warning.clone());
        }
    }
    let enabled: Vec<_> = servers.iter().filter(|server| server.enabled).collect();
    let mut tools = Vec::new();
    let mut connected = 0usize;
    let broken = Arc::new(AtomicBool::new(false));
    let result_dir = Arc::new(home.root().join("mcp-results"));
    for server in &enabled {
        let api_key = secrets
            .key(&format!("mcp-{}", server.id))
            .map(str::to_owned);
        // The channel timeout bounds every request over the channel's
        // lifetime — including each tools/call during the turn — so it must
        // be the full request budget, not a connect-phase bound. A 10s value
        // here made every tool call that outlasted it fail with Timeout and
        // drop the connection mid-turn.
        let Ok(mut client) = open_mcp_client(server, api_key).await else {
            continue;
        };
        let Ok(listed) = client.list_tools().await else {
            continue;
        };
        connected += 1;
        let direct = mcp_server_is_direct(&listed);
        let client = Arc::new(tokio::sync::Mutex::new(client));
        for tool in listed {
            tools.push(Arc::new(DynamicMcpTool::new(
                server.id.clone(),
                tool,
                client.clone(),
                Arc::clone(&broken),
                direct,
                Arc::clone(&result_dir),
            )));
        }
    }
    if connected == enabled.len() {
        *pool.lock().await = Some(McpPool {
            fingerprint,
            broken,
            tools: tools.clone(),
            warning: project_warning.clone(),
        });
    }
    (tools, project_warning)
}

fn merged_mcp_servers(
    home: &mycode_config::HomeLayout,
    settings: &AppSettings,
    project: Option<&std::path::Path>,
) -> (Vec<mycode_config::McpServerSettings>, Option<String>) {
    let mut servers: Vec<_> = settings
        .mcp_servers
        .iter()
        .filter(|server| server.enabled)
        .cloned()
        .collect();
    let Some(project) = project else {
        return (servers, None);
    };
    let trusted = mycode_config::read_ui_state(home)
        .ok()
        .is_some_and(|state| {
            let text = project.to_string_lossy();
            state
                .trusted_projects
                .iter()
                .any(|item| mycode_config::same_project_path(item, text.as_ref()))
        });
    match mycode_config::project_mcp_servers(project, trusted) {
        Ok(extra) => {
            for server in extra.into_iter().filter(|server| server.enabled) {
                if let Some(slot) = servers.iter_mut().find(|existing| existing.id == server.id) {
                    *slot = server;
                } else {
                    servers.push(server);
                }
            }
            (servers, None)
        }
        Err(error) => (servers, Some(error)),
    }
}

fn mcp_fingerprint(
    servers: &[mycode_config::McpServerSettings],
    secrets: &mycode_config::ProviderSecrets,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for server in servers.iter().filter(|server| server.enabled) {
        server.id.hash(&mut hasher);
        server.transport.hash(&mut hasher);
        server.command.hash(&mut hasher);
        server.args.hash(&mut hasher);
        server.env.hash(&mut hasher);
        server.endpoint.hash(&mut hasher);
        server.key_header.hash(&mut hasher);
        secrets.key(&format!("mcp-{}", server.id)).hash(&mut hasher);
    }
    hasher.finish()
}

/// Connected MCP tools, addressed by name. Schemas stay here until
/// `search_tool` is called.
pub(crate) struct McpCatalog {
    by_name: HashMap<String, Arc<DynamicMcpTool>>,
    direct: Vec<String>,
    deferred: Vec<(String, String)>,
    warning: Option<String>,
}

impl McpCatalog {
    pub(crate) fn from_tools_with_warning(
        tools: Vec<Arc<DynamicMcpTool>>,
        warning: Option<String>,
    ) -> Option<Arc<Self>> {
        if tools.is_empty() && warning.is_none() {
            return None;
        }
        let mut by_name = HashMap::new();
        let mut direct = Vec::new();
        let mut deferred = Vec::new();
        for tool in tools {
            if tool.direct {
                direct.push(tool.tool_name.clone());
            } else {
                deferred.push((tool.tool_name.clone(), tool.spec.description.clone()));
            }
            by_name.insert(tool.tool_name.clone(), tool);
        }
        direct.sort();
        direct.dedup();
        deferred.sort_by(|left, right| left.0.cmp(&right.0));
        deferred.dedup_by(|left, right| left.0 == right.0);
        Some(Arc::new(Self {
            by_name,
            direct,
            deferred,
            warning,
        }))
    }

    /// Tools small enough to register on the model tool list.
    pub(crate) fn direct_tools(&self) -> Vec<Arc<DynamicMcpTool>> {
        self.direct
            .iter()
            .filter_map(|name| self.by_name.get(name).cloned())
            .collect()
    }

    /// Prompt note that does not preload every deferred tool name.
    pub(crate) fn prompt_note(&self) -> String {
        let mut note = String::new();
        if let Some(warning) = &self.warning {
            note.push_str("Project MCP config was not loaded: ");
            note.push_str(warning);
            note.push('\n');
        }
        if !self.direct.is_empty() {
            note.push_str("Direct MCP tools: ");
            note.push_str(&self.direct.join(", "));
            note.push_str(". Call them by name.\n");
        }
        if !self.deferred.is_empty() {
            note.push_str(
                "Other MCP tools are connected but not preloaded. Call `search_tool` with name \
\"list\" for names and descriptions, then with an exact name for the inputSchema, then \
`use_tool`. Do not guess parameters.\n",
            );
        }
        note
    }

    fn list_text(&self) -> String {
        if self.deferred.is_empty() {
            return "No deferred MCP tools. Direct tools are called by name.".to_owned();
        }
        self.deferred
            .iter()
            .map(|(name, description)| format!("{name} — {description}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn get(&self, name: &str) -> Option<&Arc<DynamicMcpTool>> {
        self.by_name.get(name)
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn arg_name(args: &Value) -> Result<String, ToolError> {
    args.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ToolError::InvalidArgs("name is required".to_owned()))
}

/// Schema lookup for MCP tools that are not registered directly.
pub(crate) struct SearchTool {
    catalog: Arc<McpCatalog>,
}

impl SearchTool {
    pub(crate) fn new(catalog: Arc<McpCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait::async_trait]
impl ToolDyn for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_tool".to_owned(),
            description: "Look up MCP tools that are not registered directly. Pass name \"list\" for names and descriptions, or an exact tool name for its inputSchema. Never guess parameters.".to_owned(),
            params_schema: object_schema(
                serde_json::json!({
                    "name": {"type": "string", "description": "Exact tool name from the connected list."}
                }),
                &["name"],
            ),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some("search_tool: fetch one MCP inputSchema before use_tool. Never guess parameters.")
    }

    async fn execute_dyn(
        &self,
        args: Value,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let name = arg_name(&args)?;
        if name == "list" {
            return Ok(ToolResult::text(self.catalog.list_text()));
        }
        let Some(tool) = self.catalog.get(&name) else {
            return Err(ToolError::InvalidArgs(format!(
                "no MCP tool named {name}. Call search_tool with name \"list\"."
            )));
        };
        let spec = tool.spec();
        Ok(ToolResult::text(format!(
            "name: {}\nserver: {}\ndescription: {}\ninputSchema: {}",
            spec.name, tool.server_id, spec.description, spec.params_schema
        )))
    }
}

/// Calls one connected MCP tool by name.
///
/// Deferred tools are expected to go through `search_tool` first. This
/// call does not check that the schema was fetched.
pub(crate) struct UseTool {
    catalog: Arc<McpCatalog>,
}

impl UseTool {
    pub(crate) fn new(catalog: Arc<McpCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait::async_trait]
impl ToolDyn for UseTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "use_tool".to_owned(),
            description: "Call a connected MCP tool. Arguments must match the inputSchema returned by search_tool for that name. Do not call this before search_tool.".to_owned(),
            params_schema: object_schema(
                serde_json::json!({
                    "name": {"type": "string"},
                    "arguments": {"type": "object", "description": "Arguments matching search_tool's inputSchema."}
                }),
                &["name", "arguments"],
            ),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some("use_tool: call an MCP tool only after search_tool returned its schema.")
    }

    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let name = arg_name(&args)?;
        let Some(tool) = self.catalog.get(&name) else {
            return Err(ToolError::InvalidArgs(format!(
                "no MCP tool named {name}. Call search_tool with name \"list\"."
            )));
        };
        let arguments = args
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !arguments.is_object() {
            return Err(ToolError::InvalidArgs(
                "arguments must be an object".to_owned(),
            ));
        }
        tool.execute_dyn(arguments, ctx, out).await
    }
}

/// Builds and initializes one MCP client for a server row over stdio or
/// HTTP. The channel is constructed with
/// [`crate::mcp_client::DEFAULT_REQUEST_TIMEOUT`], which bounds every
/// request for the channel's lifetime — handshake, tools/list, and each
/// tools/call during a turn.
async fn open_mcp_client(
    server: &mycode_config::McpServerSettings,
    api_key: Option<String>,
) -> Result<crate::mcp_client::McpClient, String> {
    let timeout = crate::mcp_client::DEFAULT_REQUEST_TIMEOUT;
    let channel: Arc<dyn crate::mcp_client::JsonRpcChannel> = match server.transport.as_str() {
        "stdio" => {
            let command = server
                .command
                .as_deref()
                .ok_or("stdio server is missing its command")?;
            let env: Vec<(String, String)> = server
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            Arc::new(
                crate::mcp_client::StdioChannel::spawn(command, &server.args, &env, timeout)
                    .await
                    .map_err(|error| format!("MCP spawn failed: {error}"))?,
            )
        }
        "http" => {
            let endpoint = server
                .endpoint
                .as_deref()
                .ok_or("http server is missing its endpoint")?;
            Arc::new(
                crate::mcp_client::HttpChannel::new(
                    endpoint,
                    crate::mcp_client::HttpChannelOptions {
                        key_header: crate::mcp_client::KeyHeader::parse(
                            server.key_header.as_deref(),
                        ),
                        api_key,
                        timeout,
                    },
                )
                .map_err(|error| format!("MCP channel failed: {error}"))?,
            )
        }
        _ => return Err("unknown MCP transport".to_owned()),
    };
    let mut client = crate::mcp_client::McpClient::new(channel);
    client
        .initialize()
        .await
        .map_err(|error| format!("MCP handshake failed: {error}"))?;
    Ok(client)
}

/// Lists the tools of one MCP server binding over stdio or HTTP.
///
/// The caller supplies the row, so a settings form can test a binding it has
/// not saved yet. Only the API key is read from the vault, because a key is
/// never carried in a command.
///
/// Runs on the caller's runtime; never builds a nested one (a nested
/// `Runtime::block_on` panics and takes the core thread down with it).
pub(crate) async fn mcp_list_tools(
    home: &mycode_config::HomeLayout,
    server: &mycode_config::McpServerSettings,
) -> Result<Vec<String>, String> {
    let secrets = mycode_config::read_provider_secrets(home)
        .map_err(|error| crate::settings_io::render_config_error(&error))?;
    let api_key = secrets
        .key(&format!("mcp-{}", server.id))
        .map(str::to_owned);
    let mut client = open_mcp_client(server, api_key).await?;
    let tools = client
        .list_tools()
        .await
        .map_err(|error| format!("MCP tools listing failed: {error}"))?;
    client.shutdown().await;
    Ok(tools.iter().map(|tool| tool.name.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::{McpCatalog, bound_mcp_result, mcp_server_is_direct};
    use crate::mcp_client::McpTool;

    fn tool(name: &str, schema_bytes: usize) -> McpTool {
        let filler = "x".repeat(schema_bytes.saturating_sub(20));
        McpTool {
            name: name.to_owned(),
            description: Some(format!("desc {name}")),
            input_schema: serde_json::json!({"type": "object", "filler": filler}),
        }
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
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
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
        assert!(!listed.contains("inputSchema"));
        let note = catalog.prompt_note();
        assert!(note.contains("list"));
        assert!(!note.contains("remote"));
    }
}
