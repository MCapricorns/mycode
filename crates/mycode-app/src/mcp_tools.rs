//! Bridges enabled MCP servers into the agent's tool registry.
//!
//! Servers are connected at turn start. Their tools are not inlined into
//! the model tool list. The model calls `search_tool` for one schema, then
//! `use_tool` with arguments that match it. A failed server is skipped.

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
}

impl DynamicMcpTool {
    fn new(
        server_id: String,
        tool: crate::mcp_client::McpTool,
        client: Arc<tokio::sync::Mutex<crate::mcp_client::McpClient>>,
        broken: Arc<AtomicBool>,
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
        Ok(if output.is_error {
            ToolResult::error(output.text)
        } else {
            ToolResult::text(output.text)
        })
    }
}

/// Live MCP clients reused across turns until settings change or a
/// connection dies.
pub(crate) struct McpPool {
    fingerprint: u64,
    broken: Arc<AtomicBool>,
    tools: Vec<Arc<DynamicMcpTool>>,
}

/// Connects every enabled MCP server and flattens its tools.
///
/// A matching live pool is reused. Any per-server failure (spawn, handshake,
/// listing) skips that server. Runs inside the spawned turn task on the
/// single-threaded core runtime — plain `.await` only, never `block_on`.
pub(crate) async fn connect_mcp_tools(
    home: &mycode_config::HomeLayout,
    settings: &AppSettings,
    pool: &tokio::sync::Mutex<Option<McpPool>>,
) -> Vec<Arc<DynamicMcpTool>> {
    let Ok(secrets) = mycode_config::read_provider_secrets(home) else {
        return Vec::new();
    };
    let fingerprint = mcp_fingerprint(settings, &secrets);
    {
        let guard = pool.lock().await;
        if let Some(cached) = guard.as_ref()
            && cached.fingerprint == fingerprint
            && !cached.broken.load(Ordering::Relaxed)
        {
            return cached.tools.clone();
        }
    }
    let mut tools = Vec::new();
    let broken = Arc::new(AtomicBool::new(false));
    for server in settings.mcp_servers.iter().filter(|server| server.enabled) {
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
        let client = Arc::new(tokio::sync::Mutex::new(client));
        for tool in listed {
            tools.push(Arc::new(DynamicMcpTool::new(
                server.id.clone(),
                tool,
                client.clone(),
                Arc::clone(&broken),
            )));
        }
    }
    *pool.lock().await = Some(McpPool {
        fingerprint,
        broken,
        tools: tools.clone(),
    });
    tools
}

fn mcp_fingerprint(settings: &AppSettings, secrets: &mycode_config::ProviderSecrets) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for server in settings.mcp_servers.iter().filter(|server| server.enabled) {
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
    names: String,
}

impl McpCatalog {
    pub(crate) fn from_tools(tools: Vec<Arc<DynamicMcpTool>>) -> Option<Arc<Self>> {
        if tools.is_empty() {
            return None;
        }
        let mut by_name = HashMap::new();
        let mut names = Vec::new();
        for tool in tools {
            names.push(tool.tool_name.clone());
            by_name.insert(tool.tool_name.clone(), tool);
        }
        names.sort();
        names.dedup();
        let extra = names.len().saturating_sub(40);
        let mut listed = names.into_iter().take(40).collect::<Vec<_>>().join(", ");
        if extra > 0 {
            listed.push_str(&format!(", and {extra} more"));
        }
        Some(Arc::new(Self {
            by_name,
            names: listed,
        }))
    }

    /// Comma-separated names injected into the system prompt.
    pub(crate) fn index(&self) -> &str {
        &self.names
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

/// Grok-style schema lookup. The model must call this before `use_tool`.
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
            description: format!(
                "Retrieve one connected MCP tool's description and inputSchema before calling use_tool. Never guess parameter names. Connected: {}.",
                self.catalog.names
            ),
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
        let Some(tool) = self.catalog.get(&name) else {
            return Err(ToolError::InvalidArgs(format!(
                "no MCP tool named {name}. Connected: {}",
                self.catalog.names
            )));
        };
        let spec = tool.spec();
        Ok(ToolResult::text(format!(
            "name: {}\nserver: {}\ndescription: {}\ninputSchema: {}",
            spec.name, tool.server_id, spec.description, spec.params_schema
        )))
    }
}

/// Calls one MCP tool whose schema was retrieved with `search_tool`.
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
                "no MCP tool named {name}. Connected: {}",
                self.catalog.names
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
