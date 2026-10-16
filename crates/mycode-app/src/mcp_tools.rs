//! Bridges enabled MCP servers into the agent's tool registry.
//!
//! Servers are connected at turn start. A small server is registered on
//! the model tool list. Larger servers stay behind `search_tool` and
//! `use_tool`. A failed server is skipped.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
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
    /// From the server's `readOnlyHint`. Missing means not read-only.
    read_only: bool,
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
            read_only: tool.read_only,
        }
    }

    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }
}

impl ToolDyn for DynamicMcpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some(self.snippet.as_str())
    }

    /// A server that declares `readOnlyHint` may serve this call beside
    /// other reads; each connection still takes one request at a time.
    fn parallel_safe(&self) -> bool {
        self.read_only
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
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
            // The dispatcher does not select on the turn token while a tool
            // runs; bound the call here so Escape actually cancels an MCP call.
            // The client lock is inside the race too: another session's turn
            // can hold this shared connection for a full request timeout.
            let call = async {
                let mut client = self.client.lock().await;
                client.call_tool(&self.tool_name, args).await
            };
            let result = tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => {
                    return Err(ToolError::Execution("MCP call cancelled".to_owned()));
                }
                result = call => result,
            };
            let output = result.map_err(|error| {
                let detail = if error.is_connection_lost() {
                    self.broken.store(true, Ordering::Relaxed);
                    format!(
                        "MCP server '{}' lost the connection: {error}",
                        self.server_id
                    )
                } else {
                    format!("MCP server '{}': {error}", self.server_id)
                };
                ToolError::Execution(detail)
            })?;
            // A server that flags `isError` ran the tool and reported failure as
            // data. The model needs to read it to correct its arguments, so it
            // becomes an error *result* rather than a dispatcher failure.
            let text = if output.text.len() <= MAX_MCP_TOOL_RESULT_BYTES {
                output.text
            } else {
                let result_dir = Arc::clone(&self.result_dir);
                let output_text = output.text;
                tokio::task::spawn_blocking(move || bound_mcp_result(&result_dir, &output_text))
                    .await
                    .map_err(|error| {
                        ToolError::Execution(format!("MCP result task failed: {error}"))
                    })?
            };
            Ok(if output.is_error {
                ToolResult::error(text)
            } else {
                ToolResult::text(text)
            })
        })
    }
}

/// Bytes of an MCP tool result kept inline for the model.
const MAX_MCP_TOOL_RESULT_BYTES: usize = 20 * 1024;

/// Age after which a stored full-text MCP result is deleted. Every oversized
/// result used to stay in `mcp-results` forever.
const MCP_RESULT_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Removes stored results older than [`MCP_RESULT_RETENTION`]. Best-effort:
/// a file that cannot be inspected or removed stays for the next sweep.
fn prune_stale_mcp_results(dir: &std::path::Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("mcp-") || !name.ends_with(".txt") {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > MCP_RESULT_RETENTION);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Truncates oversized tool text and stores the full body beside the home.
pub(crate) fn bound_mcp_result(dir: &std::path::Path, text: &str) -> String {
    if text.len() <= MAX_MCP_TOOL_RESULT_BYTES {
        return text.to_owned();
    }
    prune_stale_mcp_results(dir, std::time::SystemTime::now());
    let id = format!(
        "mcp-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    );
    let path = dir.join(format!("{id}.txt"));
    let stored = match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, text)) {
        Ok(()) => format!("full text stored at {}", path.display()),
        Err(error) => format!("full text could not be stored: {error}"),
    };
    let head_end = text.floor_char_boundary(MAX_MCP_TOOL_RESULT_BYTES / 2);
    let tail_start =
        text.ceil_char_boundary(text.len().saturating_sub(MAX_MCP_TOOL_RESULT_BYTES / 2));
    format!(
        "{}\n\n[truncated; {stored}]\n\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
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
    /// One slot per enabled server, in settings order. `None` is a server
    /// that did not connect; the next turn retries only those.
    servers: Vec<Option<Vec<Arc<DynamicMcpTool>>>>,
    warning: Option<String>,
}

impl McpPool {
    fn tools(&self) -> Vec<Arc<DynamicMcpTool>> {
        self.servers.iter().flatten().flatten().cloned().collect()
    }
}

/// Connects every enabled MCP server and flattens its tools.
///
/// Servers that connected stay live across turns while the settings
/// fingerprint holds and no connection broke. A server that fails to spawn,
/// handshake, or list tools is skipped for this turn and retried on the
/// next one, without restarting the servers that are already up. Runs
/// inside the spawned turn task on the single-threaded core runtime —
/// plain `.await` only, never `block_on`.
pub(crate) async fn connect_mcp_tools(
    home: &mycode_config::HomeLayout,
    settings: &AppSettings,
    pool: &tokio::sync::Mutex<Option<McpPool>>,
    project: Option<&std::path::Path>,
) -> (Vec<Arc<DynamicMcpTool>>, Option<String>) {
    let read_home = home.clone();
    let read_settings = settings.clone();
    let read_project = project.map(std::path::Path::to_path_buf);
    let Ok((Ok(secrets), merged)) = tokio::task::spawn_blocking(move || {
        (
            mycode_config::read_provider_secrets(&read_home),
            merged_mcp_servers(&read_settings, read_project.as_deref()),
        )
    })
    .await
    else {
        return (Vec::new(), None);
    };
    let MergedServers {
        servers,
        project_ids,
        warning: project_warning,
    } = merged;
    let stored_key = |server: &mycode_config::McpServerSettings| {
        stored_mcp_key(&secrets, &project_ids, &server.id).map(str::to_owned)
    };
    let fingerprint = mcp_fingerprint(&servers, &stored_key);
    let enabled: Vec<_> = servers.iter().filter(|server| server.enabled).collect();
    let (broken, mut slots) = {
        let guard = pool.lock().await;
        match guard.as_ref() {
            Some(cached)
                if cached.fingerprint == fingerprint
                    && !cached.broken.load(Ordering::Relaxed)
                    && cached.servers.len() == enabled.len() =>
            {
                if cached.servers.iter().all(Option::is_some) {
                    return (cached.tools(), cached.warning.clone());
                }
                (Arc::clone(&cached.broken), cached.servers.clone())
            }
            _ => (Arc::new(AtomicBool::new(false)), vec![None; enabled.len()]),
        }
    };
    let result_dir = Arc::new(home.root().join("mcp-results"));
    // Missing servers start together: a turn waits for the slowest
    // handshake, not the sum of them. The futures stay owned by this one, so
    // a cancelled turn still drops (and kills) every half-started server.
    // Slots keep the settings order, so the tool list is the same bytes
    // every turn.
    let handshakes = enabled
        .iter()
        .zip(&slots)
        .filter(|(_, slot)| slot.is_none())
        .map(|(server, _)| {
            let api_key = stored_key(server);
            async move {
                // The channel timeout bounds every request over the channel's
                // lifetime — including each tools/call during the turn — so
                // it must be the full request budget, not a connect-phase
                // bound. A 10s value here made every tool call that outlasted
                // it fail with Timeout and drop the connection mid-turn.
                let mut client = open_mcp_client(server, api_key).await.ok()?;
                let listed = client.list_tools().await.ok()?;
                Some((client, listed))
            }
        })
        .collect::<Vec<_>>();
    let mut opened = futures_util::future::join_all(handshakes).await.into_iter();
    for (server, slot) in enabled.iter().zip(slots.iter_mut()) {
        if slot.is_some() {
            continue;
        }
        let Some((client, listed)) = opened.next().flatten() else {
            continue;
        };
        let direct = mcp_server_is_direct(&listed);
        let client = Arc::new(tokio::sync::Mutex::new(client));
        *slot = Some(
            listed
                .into_iter()
                .map(|tool| {
                    Arc::new(DynamicMcpTool::new(
                        server.id.clone(),
                        tool,
                        client.clone(),
                        Arc::clone(&broken),
                        direct,
                        Arc::clone(&result_dir),
                    ))
                })
                .collect(),
        );
    }
    let live = McpPool {
        fingerprint,
        broken,
        servers: slots,
        warning: project_warning.clone(),
    };
    let tools = live.tools();
    *pool.lock().await = Some(live);
    (tools, project_warning)
}

/// User servers plus the project's own, with the ids that came from the
/// project.
#[derive(Debug, Default)]
struct MergedServers {
    servers: Vec<mycode_config::McpServerSettings>,
    project_ids: std::collections::HashSet<String>,
    warning: Option<String>,
}

/// A project's `.mycode/mcp.json` ships with the repository, so it adds
/// servers but never replaces one the user configured, enabled or not. A
/// project row reusing a user id would otherwise inherit that server's
/// stored key and could send it to an endpoint the repository chose.
fn merged_mcp_servers(settings: &AppSettings, project: Option<&std::path::Path>) -> MergedServers {
    let mut merged = MergedServers {
        servers: settings
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .cloned()
            .collect(),
        ..MergedServers::default()
    };
    let Some(project) = project else {
        return merged;
    };
    let extra = match mycode_config::project_mcp_servers(project) {
        Ok(extra) => extra,
        Err(error) => {
            merged.warning = Some(error);
            return merged;
        }
    };
    let mut shadowed = Vec::new();
    for server in extra.into_iter().filter(|server| server.enabled) {
        let taken = settings
            .mcp_servers
            .iter()
            .any(|existing| existing.id == server.id)
            || merged.project_ids.contains(&server.id);
        if taken {
            shadowed.push(server.id);
            continue;
        }
        merged.project_ids.insert(server.id.clone());
        merged.servers.push(server);
    }
    if !shadowed.is_empty() {
        merged.warning = Some(format!(
            "project servers {} reuse a configured id and were skipped",
            shadowed.join(", ")
        ));
    }
    merged
}

/// The vault key for one server. Project servers never get one: the user
/// stored those keys for their own servers.
fn stored_mcp_key<'a>(
    secrets: &'a mycode_config::ProviderSecrets,
    project_ids: &std::collections::HashSet<String>,
    id: &str,
) -> Option<&'a str> {
    if project_ids.contains(id) {
        return None;
    }
    secrets.key(&format!("mcp-{id}"))
}

fn mcp_fingerprint(
    servers: &[mycode_config::McpServerSettings],
    stored_key: &dyn Fn(&mycode_config::McpServerSettings) -> Option<String>,
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
        stored_key(server).hash(&mut hasher);
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
            let name = tool.spec.name.clone();
            if tool.direct {
                direct.push(name.clone());
            } else {
                deferred.push((name.clone(), tool.spec.description.clone()));
            }
            by_name.insert(name, tool);
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
            note.push_str("Project MCP config: ");
            note.push_str(warning);
            note.push('\n');
        }
        if !self.direct.is_empty() {
            note.push_str("Direct MCP tools: ");
            note.push_str(&self.direct.join(", "));
            note.push_str(". Call them directly by name.\n");
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

/// Builds the tool catalog from one connect and registers its bridge tools
/// on `registry`: direct tools first (never shadowing an existing name),
/// then `search_tool` and `use_tool`. Returns the catalog so the caller can
/// render its prompt note.
pub(crate) fn register_mcp_tools(
    registry: &mycode_tools::ToolRegistry,
    tools: Vec<Arc<DynamicMcpTool>>,
    warning: Option<String>,
) -> Option<Arc<McpCatalog>> {
    let catalog = McpCatalog::from_tools_with_warning(tools, warning)?;
    for tool in catalog.direct_tools() {
        if registry.get(tool.spec().name.as_str()).is_none() {
            registry.register(tool);
        }
    }
    registry.register(Arc::new(SearchTool::new(Arc::clone(&catalog))));
    registry.register(Arc::new(UseTool::new(Arc::clone(&catalog))));
    Some(catalog)
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

    fn parallel_safe(&self) -> bool {
        true
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
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
        })
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

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
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
        })
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
            Arc::new(crate::mcp_client::HttpChannel::new(
                endpoint,
                crate::mcp_client::HttpChannelOptions {
                    key_header: crate::mcp_client::KeyHeader::parse(server.key_header.as_deref()),
                    api_key,
                    timeout,
                },
            ))
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
    let read_home = home.clone();
    let secrets =
        tokio::task::spawn_blocking(move || mycode_config::read_provider_secrets(&read_home))
            .await
            .map_err(|error| format!("secrets task failed: {error}"))?
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
        let mut user = mycode_config::builtin_mcp_servers();
        user[0].enabled = false;
        let settings = super::AppSettings {
            mcp_servers: user,
            ..super::AppSettings::default()
        };
        let merged = super::merged_mcp_servers(&settings, Some(&root));
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
            super::stored_mcp_key(&secrets, &merged.project_ids, "local"),
            None
        );
        assert_eq!(
            super::stored_mcp_key(&secrets, &merged.project_ids, "context7"),
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
        assert!(!listed.contains("inputSchema"));
        let note = catalog.prompt_note();
        assert!(note.contains("list"));
        assert!(!note.contains("remote"));
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
                dyn std::future::Future<
                        Output = Result<serde_json::Value, crate::mcp_client::McpError>,
                    > + Send
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
            Box<
                dyn std::future::Future<Output = Result<(), crate::mcp_client::McpError>>
                    + Send
                    + 'a,
            >,
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
        let calls = scripted.calls.lock().expect("calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "lookup");
        assert_eq!(calls[0]["arguments"]["query"], "direct");
        let _ = std::fs::remove_dir_all(&root);
    }
}
