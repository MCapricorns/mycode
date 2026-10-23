//! Bridges enabled MCP servers into the agent's tool registry.
//!
//! Connections are progressive: `resolve` reads secrets and merges the three
//! server sources (a few milliseconds of disk I/O), `spawn_slots` starts one
//! handshake task per server and returns immediately, and `drain_mcp_ready`
//! registers each server's tools as it finishes. The turn's first request
//! therefore never waits for a handshake.
//!
//! Every ready server is written twice, in this order: `McpCatalog` first
//! (the one materialisation point `search_tool`/`use_tool`/`tool_search`
//! read), then the `ToolRegistry` (so the schemas reach `Request.tools`).
//! See `wiki/architecture/code-mode-engine.md` §5.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mycode_agent::AppSettings;
use mycode_agent::ToolSpec;
use mycode_agent::{ToolCtx, ToolDyn, ToolError, ToolResult, ToolStream};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::tool_search::{ToolSearchIndex, ToolSearchTool};

/// One MCP tool exposed by a connected server.
///
/// Implements `ToolDyn` directly instead of `Tool` because the server's
/// `inputSchema` must reach the model verbatim through `spec()`; the blanket
/// `impl<T: Tool> ToolDyn` would advertise the schemars schema of
/// `Args = serde_json::Value`, which is just `true`.
pub(crate) struct DynamicMcpTool {
    server: Arc<McpServerInfo>,
    tool_name: String,
    /// Built once. `spec()` clones this instead of rebuilding the schema.
    spec: ToolSpec,
    /// Compiled once from the server's schema; `None` when it does not
    /// compile — args then pass through and the server's rejection is the
    /// backstop.
    validator: Option<jsonschema::Validator>,
    client: Arc<Mutex<crate::mcp_client::McpClient>>,
    snippet: String,
    /// Set when the shared connection dies so the next turn reconnects.
    broken: Arc<AtomicBool>,
    /// Directory that stores full text when a tool result is truncated.
    /// Shared by every tool from one connect, so each tool does not copy the path.
    result_dir: Arc<PathBuf>,
    /// From the server's `readOnlyHint`. Missing means not read-only.
    read_only: bool,
}

/// One connected server as the system prompt describes it: the id the model
/// searches by and one line from the server's own handshake instructions.
#[derive(Debug)]
pub(crate) struct McpServerInfo {
    id: String,
    summary: Option<String>,
}

impl McpServerInfo {
    /// Names one server; `instructions` is its handshake summary, if any.
    pub(crate) fn new(id: &str, instructions: Option<&str>) -> Self {
        Self {
            id: id.to_owned(),
            summary: instructions.and_then(mcp_server_summary),
        }
    }

    /// The server identity the prompt and catalog key use.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

impl DynamicMcpTool {
    fn new(
        server: Arc<McpServerInfo>,
        tool: crate::mcp_client::McpTool,
        client: Arc<Mutex<crate::mcp_client::McpClient>>,
        broken: Arc<AtomicBool>,
        result_dir: Arc<PathBuf>,
    ) -> Self {
        let validator = jsonschema::validator_for(&tool.input_schema).ok();
        let snippet = mcp_snippet(&server.id, tool.description.as_deref());
        let spec = ToolSpec {
            name: tool.name.clone(),
            description: tool
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool '{}' on server '{}'.", tool.name, server.id)),
            params_schema: tool.input_schema,
        };
        Self {
            server,
            tool_name: tool.name,
            spec,
            validator,
            client,
            snippet,
            broken,
            result_dir,
            read_only: tool.read_only,
        }
    }

    pub(crate) fn read_only(&self) -> bool {
        self.read_only
    }

    /// The server this tool belongs to.
    pub(crate) fn server_id(&self) -> &str {
        &self.server.id
    }

    /// One-line summary shown by the catalog and the BM25 corpus.
    pub(crate) fn snippet(&self) -> &str {
        &self.snippet
    }

    /// What the tool does, as the server declared it.
    pub(crate) fn description(&self) -> &str {
        &self.spec.description
    }

    /// The server-declared input schema.
    pub(crate) fn schema(&self) -> &Value {
        &self.spec.params_schema
    }
}

mod results;

use results::{MAX_MCP_TOOL_RESULT_BYTES, bound_mcp_result, mcp_server_summary, mcp_snippet};

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
            // Server schemas vary more than ours do, so repair the shapes a
            // model typically gets wrong (required-field spellings, quoted
            // scalars, an untagged union variant) before validating. This is
            // schema-driven only: camelCase keys are left alone because the
            // server may declare them that way.
            let mut args = args;
            mycode_agent::tool::repair_args(&mut args, &self.spec.params_schema);
            if let Some(validator) = &self.validator {
                let errors: Vec<String> = validator
                    .iter_errors(&args)
                    .map(|error| {
                        mycode_agent::tool::render_schema_error(&error, &self.spec.params_schema)
                    })
                    .collect();
                if !errors.is_empty() {
                    return Err(ToolError::InvalidArgs(errors.join("; ")));
                }
            }
            out.progress(format!(
                "calling {} on MCP server {}",
                self.tool_name, self.server.id
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
                    return Err(ToolError::Execution(format!(
                        "MCP call cancelled{}",
                        mycode_agent::tool::INTERRUPTED_NOTE
                    )));
                }
                result = call => result,
            };
            let output = result.map_err(|error| {
                let detail = if error.is_connection_lost() {
                    self.broken.store(true, Ordering::Relaxed);
                    format!(
                        "MCP server '{}' lost the connection: {error}",
                        self.server.id
                    )
                } else {
                    format!("MCP server '{}': {error}", self.server.id)
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

/// Bound on one server's handshake and tool listing. The channel's own
/// timeout is the per-call budget, so without this a server that accepts
/// the connection and never answers held every turn start for two of them.
const MCP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a server that failed to connect is left alone before a turn
/// tries it again. A settings change or a lost connection retries at once.
const MCP_RETRY_COOLDOWN: Duration = Duration::from_secs(120);

/// How often the drain coroutine rescans the pool even without a wakeup.
/// Handshake tasks notify on completion; the tick only covers a lost
/// notification or a slot whose retry deadline passed.
const DRAIN_TICK: Duration = Duration::from_secs(2);

/// One server's connection state.
///
/// `Connecting` owns the handshake `JoinHandle` through the pool, never
/// through a turn: a turn's drain task only listens, so a turn that ends
/// leaves the handshake running and the next turn reuses it.
pub(crate) enum McpSlot {
    /// In-flight handshake; the handle lives here so the pool can abort it.
    Connecting {
        server: Arc<McpServerInfo>,
        handle: tokio::task::JoinHandle<()>,
    },
    /// Handshake and `tools/list` succeeded.
    Ready {
        server: Arc<McpServerInfo>,
        tools: Vec<Arc<DynamicMcpTool>>,
        /// Set by a tool when the shared connection dies; the next
        /// `spawn_slots` turns this slot back into `Connecting`.
        broken: Arc<AtomicBool>,
    },
    /// Failed to spawn/handshake/list. Retried at `retry_at`, per server.
    Failed {
        server: Arc<McpServerInfo>,
        reason: String,
        retry_at: Instant,
    },
}

impl McpSlot {
    fn server_id(&self) -> &str {
        match self {
            Self::Connecting { server, .. }
            | Self::Ready { server, .. }
            | Self::Failed { server, .. } => server.id(),
        }
    }
}

/// Cross-turn pool of MCP connections, keyed by the settings fingerprint.
pub(crate) struct McpPool {
    fingerprint: u64,
    slots: Vec<McpSlot>,
    warning: Option<String>,
    /// Wakes `drain_mcp_ready` when a handshake installs its tools.
    notify: Arc<Notify>,
}

impl McpPool {
    fn new(
        fingerprint: u64,
        servers: &[ResolvedServer],
        warning: Option<String>,
    ) -> Self {
        let slots = servers
            .iter()
            .map(|server| McpSlot::Failed {
                server: Arc::new(McpServerInfo::new(&server.settings.id, None)),
                reason: "not connected yet".to_owned(),
                // Due immediately: `spawn_slots` starts every due slot in
                // the same call that creates the pool.
                retry_at: Instant::now(),
            })
            .collect();
        Self {
            fingerprint,
            slots,
            warning,
            notify: Arc::new(Notify::new()),
        }
    }

    /// One entry per ready server, in settings order.
    fn ready(&self) -> Vec<(Arc<McpServerInfo>, Vec<Arc<DynamicMcpTool>>)> {
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                McpSlot::Ready { server, tools, .. } => Some((Arc::clone(server), tools.clone())),
                _ => None,
            })
            .collect()
    }

    fn connecting(&self) -> Vec<String> {
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                McpSlot::Connecting { server, .. } => Some(server.id().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// One entry per failed server, with its one-line reason.
    fn failed(&self) -> Vec<(Arc<McpServerInfo>, String)> {
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                McpSlot::Failed {
                    server, reason, ..
                } => Some((Arc::clone(server), reason.clone())),
                _ => None,
            })
            .collect()
    }

    fn has_connecting(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, McpSlot::Connecting { .. }))
    }

    /// Aborts every in-flight handshake and leaves those slots due for an
    /// immediate retry.
    fn abort_connecting(&mut self) {
        let mut aborted = false;
        for slot in &mut self.slots {
            let McpSlot::Connecting { handle, server } = slot else {
                continue;
            };
            handle.abort();
            let info = Arc::clone(server);
            *slot = McpSlot::Failed {
                server: info,
                reason: "cancelled".to_owned(),
                retry_at: Instant::now(),
            };
            aborted = true;
        }
        if aborted {
            self.notify.notify_waiters();
        }
    }
}

/// Everything `resolve` found: enabled servers in order, with their keys.
pub(crate) struct ResolvedMcp {
    fingerprint: u64,
    servers: Vec<ResolvedServer>,
    warning: Option<String>,
}

struct ResolvedServer {
    settings: mycode_agent::McpServerSettings,
    api_key: Option<String>,
}

/// What `spawn_slots` found at the moment it returned.
pub(crate) struct McpConnect {
    /// Servers already ready (cached from an earlier turn).
    pub(crate) ready: Vec<(Arc<McpServerInfo>, Vec<Arc<DynamicMcpTool>>)>,
    /// Server ids whose handshake is still running.
    pub(crate) connecting: Vec<String>,
    /// Server ids that failed and are in their retry cooldown, with the
    /// one-line reason.
    pub(crate) failed: Vec<(String, String)>,
    /// A config error to show in the prompt.
    pub(crate) warning: Option<String>,
}

impl McpConnect {
    /// Whether any MCP server is configured at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.ready.is_empty()
            && self.connecting.is_empty()
            && self.failed.is_empty()
            && self.warning.is_none()
    }
}

/// Reads secrets and magpie's Library servers.
///
/// This is the only step that must finish before the first request (the
/// turn cannot spawn slots for servers it does not know), and it is disk
/// I/O on the blocking pool, not a handshake.
pub(crate) async fn resolve(
    home: &mycode_agent::HomeLayout,
    _settings: &AppSettings,
    _project: Option<&std::path::Path>,
) -> ResolvedMcp {
    let read_home = home.clone();
    let Ok((Ok(secrets), merged)) = tokio::task::spawn_blocking(move || {
        (
            mycode_agent::read_provider_secrets(&read_home),
            merged_mcp_servers(&read_home),
        )
    })
    .await
    else {
        return ResolvedMcp {
            fingerprint: 0,
            servers: Vec::new(),
            warning: None,
        };
    };
    let MergedServers {
        servers,
        magpie_ids,
        warning,
    } = merged;
    let stored_key = |server: &mycode_agent::McpServerSettings| {
        stored_mcp_key(&secrets, &magpie_ids, &server.id).map(str::to_owned)
    };
    let fingerprint = mcp_fingerprint(&servers, &stored_key);
    let enabled: Vec<ResolvedServer> = servers
        .iter()
        .filter(|server| server.enabled)
        .map(|server| ResolvedServer {
            settings: server.clone(),
            api_key: stored_key(server),
        })
        .collect();
    ResolvedMcp {
        fingerprint,
        servers: enabled,
        warning,
    }
}

/// Starts a handshake for every server that needs one and returns at once.
///
/// A pool for the same fingerprint is reused: `Connecting` slots keep
/// running across turns, `Ready` slots keep their tools, and `Failed` slots
/// are retried only after their own cooldown. Any other fingerprint drops
/// the old pool (aborting its in-flight handshakes).
pub(crate) async fn spawn_slots(
    resolved: ResolvedMcp,
    pool: &Arc<Mutex<Option<McpPool>>>,
    result_dir: Arc<PathBuf>,
) -> McpConnect {
    let mut guard = pool.lock().await;
    let compatible = guard.as_ref().is_some_and(|pool| {
        pool.fingerprint == resolved.fingerprint
            && pool.slots.len() == resolved.servers.len()
            && pool
                .slots
                .iter()
                .zip(&resolved.servers)
                .all(|(slot, server)| slot.server_id() == server.settings.id)
    });
    if !compatible {
        if let Some(mut old) = guard.take() {
            old.abort_connecting();
        }
        *guard = Some(McpPool::new(
            resolved.fingerprint,
            &resolved.servers,
            resolved.warning.clone(),
        ));
    }
    let pool_ref = guard.as_mut().expect("the pool was just ensured");
    let notify = Arc::clone(&pool_ref.notify);
    for (index, server) in resolved.servers.iter().enumerate() {
        let due = match &pool_ref.slots[index] {
            McpSlot::Connecting { .. } => false,
            // A lost connection retries at once, bypassing the cooldown.
            McpSlot::Ready { broken, .. } => broken.load(Ordering::Relaxed),
            McpSlot::Failed { retry_at, .. } => Instant::now() >= *retry_at,
        };
        if !due {
            continue;
        }
        let info = Arc::new(McpServerInfo::new(&server.settings.id, None));
        let handle = spawn_handshake(
            Arc::clone(pool),
            resolved.fingerprint,
            index,
            server.settings.clone(),
            server.api_key.clone(),
            Arc::clone(&result_dir),
            Arc::clone(&notify),
        );
        pool_ref.slots[index] = McpSlot::Connecting {
            server: info,
            handle,
        };
    }
    let connect = McpConnect {
        ready: pool_ref.ready(),
        connecting: pool_ref.connecting(),
        failed: pool_ref
            .slots
            .iter()
            .filter_map(|slot| match slot {
                McpSlot::Failed {
                    server, reason, ..
                } => Some((server.id().to_owned(), reason.clone())),
                _ => None,
            })
            .collect(),
        warning: pool_ref.warning.clone(),
    };
    drop(guard);
    connect
}

/// Resolves and spawns in one step; the turn's entry point.
pub(crate) async fn connect_mcp_tools(
    home: &mycode_agent::HomeLayout,
    settings: &AppSettings,
    pool: &Arc<Mutex<Option<McpPool>>>,
    project: Option<&std::path::Path>,
) -> McpConnect {
    let resolved = resolve(home, settings, project).await;
    let result_dir = Arc::new(home.root().join("mcp-results"));
    spawn_slots(resolved, pool, result_dir).await
}

/// Reports servers becoming ready or failed for the rest of the turn.
///
/// `on_ready` and `on_failed` run once per server id, including servers
/// whose state was already known when the drain started (`seen_ready` /
/// `seen_failed` are seeded with those). The drain stops once no handshake
/// is in flight; a turn's task is aborted at the end of the turn, which only
/// stops the listener.
///
/// A canceled turn does not abort the handshakes: the pool is shared by
/// every session in the process, so one session's cancel must not kill a
/// connection another session is waiting for. The handshakes are bounded by
/// [`MCP_HANDSHAKE_TIMEOUT`] and their results are reused by the next turn.
pub(crate) async fn drain_mcp_ready<F, G>(
    pool: Arc<Mutex<Option<McpPool>>>,
    mut seen_ready: HashSet<String>,
    mut seen_failed: HashSet<String>,
    cancel: Option<CancellationToken>,
    mut on_ready: F,
    mut on_failed: G,
) where
    F: FnMut(Arc<McpServerInfo>, Vec<Arc<DynamicMcpTool>>),
    G: FnMut(Arc<McpServerInfo>, &str),
{
    loop {
        let (ready, failed, connecting, notify) = {
            let guard = pool.lock().await;
            let Some(pool_ref) = guard.as_ref() else {
                return;
            };
            (
                pool_ref.ready(),
                pool_ref.failed(),
                pool_ref.has_connecting(),
                Arc::clone(&pool_ref.notify),
            )
        };
        for (server, tools) in ready {
            if seen_ready.insert(server.id().to_owned()) {
                on_ready(server, tools);
            }
        }
        for (server, reason) in failed {
            if seen_failed.insert(server.id().to_owned()) {
                on_failed(server, &reason);
            }
        }
        if !connecting {
            return;
        }
        tokio::select! {
            () = wait_cancelled(cancel.as_ref()) => return,
            () = notify.notified() => {}
            () = tokio::time::sleep(DRAIN_TICK) => {}
        }
    }
}

async fn wait_cancelled(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

fn spawn_handshake(
    pool: Arc<Mutex<Option<McpPool>>>,
    fingerprint: u64,
    index: usize,
    server: mycode_agent::McpServerSettings,
    api_key: Option<String>,
    result_dir: Arc<PathBuf>,
    notify: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = handshake(&server, api_key.as_deref(), &result_dir).await;
        let mut guard = pool.lock().await;
        let Some(pool_ref) = guard.as_mut() else {
            return;
        };
        // A settings change replaced the pool while this handshake ran; the
        // result belongs to a configuration nobody asked for any more.
        if pool_ref.fingerprint != fingerprint {
            return;
        }
        let still_ours = pool_ref.slots.get(index).is_some_and(|slot| {
            slot.server_id() == server.id && matches!(slot, McpSlot::Connecting { .. })
        });
        if !still_ours {
            return;
        }
        pool_ref.slots[index] = match outcome {
            Ok((info, tools, broken)) => McpSlot::Ready {
                server: info,
                tools,
                broken,
            },
            Err(reason) => McpSlot::Failed {
                server: Arc::new(McpServerInfo::new(&server.id, None)),
                reason,
                retry_at: Instant::now() + MCP_RETRY_COOLDOWN,
            },
        };
        drop(guard);
        notify.notify_waiters();
    })
}

/// Handshake, list, and build one server's tools.
async fn handshake(
    server: &mycode_agent::McpServerSettings,
    api_key: Option<&str>,
    result_dir: &Arc<PathBuf>,
) -> Result<
    (
        Arc<McpServerInfo>,
        Vec<Arc<DynamicMcpTool>>,
        Arc<AtomicBool>,
    ),
    String,
> {
    let attempt = async {
        let mut client = open_mcp_client(server, api_key.map(str::to_owned)).await.ok()?;
        let listed = client.list_tools().await.ok()?;
        Some((client, listed))
    };
    let Some((client, listed)) = tokio::time::timeout(MCP_HANDSHAKE_TIMEOUT, attempt)
        .await
        .ok()
        .flatten()
    else {
        return Err("handshake or tools/list failed within the timeout".to_owned());
    };
    let info = Arc::new(McpServerInfo::new(&server.id, client.instructions()));
    let client = Arc::new(Mutex::new(client));
    let broken = Arc::new(AtomicBool::new(false));
    let tools = listed
        .into_iter()
        .map(|tool| {
            Arc::new(DynamicMcpTool::new(
                Arc::clone(&info),
                tool,
                Arc::clone(&client),
                Arc::clone(&broken),
                Arc::clone(result_dir),
            ))
        })
        .collect();
    Ok((info, tools, broken))
}

/// magpie's Library servers, plus the ids that came from that managed source.
#[derive(Debug, Default)]
struct MergedServers {
    servers: Vec<mycode_agent::McpServerSettings>,
    magpie_ids: HashSet<String>,
    warning: Option<String>,
}

/// magpie's `~/.mycode/mcp.json` is the only MCP source: the Library owns the
/// file and keeps the servers' vendor headers on the rows.
fn merged_mcp_servers(home: &mycode_agent::HomeLayout) -> MergedServers {
    let mut merged = MergedServers::default();
    let magpie = match mycode_agent::global_mcp_servers(home) {
        Ok(servers) => servers,
        Err(error) => {
            merged.warning = Some(error);
            Vec::new()
        }
    };
    for server in magpie.into_iter().filter(|server| server.enabled) {
        merged.magpie_ids.insert(server.id.clone());
        merged.servers.push(server);
    }
    merged
}

/// The vault key for one server. Managed rows never get one: their own file
/// carries the headers they need.
fn stored_mcp_key<'a>(
    secrets: &'a mycode_agent::ProviderSecrets,
    magpie_ids: &HashSet<String>,
    id: &str,
) -> Option<&'a str> {
    if magpie_ids.contains(id) {
        return None;
    }
    secrets.key(&format!("mcp-{id}"))
}

fn mcp_fingerprint(
    servers: &[mycode_agent::McpServerSettings],
    stored_key: &dyn Fn(&mycode_agent::McpServerSettings) -> Option<String>,
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
        server.headers.hash(&mut hasher);
        stored_key(server).hash(&mut hasher);
    }
    hasher.finish()
}

/// Connected MCP tools, addressed by catalog key. Schemas stay here until
/// `search_tool`/`use_tool`/`tool_search` asks for one.
///
/// The key is the server's tool name, or `server__tool` when an earlier
/// server or a registered tool already took that name, so two tools never
/// hide each other. This is the single materialisation point: the registry
/// only mirrors the keys whose specs reach `Request.tools`.
pub(crate) struct McpCatalog {
    inner: RwLock<CatalogInner>,
    warning: Option<String>,
    /// Lowercased names of the tools already on the registry when the
    /// catalog was created. A name in here is directly callable, so a lookup
    /// miss on it gets a "call the tool directly" nudge instead of the
    /// generic no-such-tool error.
    native: BTreeSet<String>,
}

struct CatalogInner {
    by_name: HashMap<String, Arc<DynamicMcpTool>>,
    /// Catalog key and one-line summary of each tool, sorted by key.
    listed: Vec<(String, String)>,
    /// Servers in configuration order with their status.
    servers: Vec<(Arc<McpServerInfo>, ServerStatus)>,
    /// Servers whose handshake has not finished.
    pending: BTreeSet<String>,
}

/// How one configured server currently stands in the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServerStatus {
    /// Connected, with the number of tools this catalog serves.
    Ready(usize),
    /// Failed to connect; the reason is bounded to one line.
    Failed(String),
}

/// Most tools one keyword search lists before it asks for a narrower query.
const MAX_SEARCH_MATCHES: usize = 25;

impl McpCatalog {
    /// An empty catalog for this turn's servers.
    pub(crate) fn new(
        warning: Option<String>,
        native: BTreeSet<String>,
        pending: impl IntoIterator<Item = String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(CatalogInner {
                by_name: HashMap::new(),
                listed: Vec::new(),
                servers: Vec::new(),
                pending: pending.into_iter().collect(),
            }),
            warning,
            native,
        })
    }

    /// Adds one ready server's tools and returns the keyed entries.
    ///
    /// Callers must write the registry only after this returns: `use_tool`
    /// may hit the catalog at any moment, and a catalog entry without its
    /// registry spec is the Step B shape, never this one.
    pub(crate) fn insert_ready(
        &self,
        server: Arc<McpServerInfo>,
        tools: Vec<Arc<DynamicMcpTool>>,
    ) -> Vec<(String, Arc<DynamicMcpTool>)> {
        let mut inner = self.inner.write();
        inner.pending.remove(server.id());
        let mut keyed = Vec::new();
        for tool in tools {
            let mut key = tool.spec.name.clone();
            if inner.by_name.contains_key(&key) || self.native.contains(&key.to_ascii_lowercase()) {
                key = format!("{}__{}", server.id(), tool.spec.name);
                if inner.by_name.contains_key(&key) {
                    continue;
                }
            }
            inner.listed.push((key.clone(), tool.snippet.clone()));
            inner.by_name.insert(key.clone(), Arc::clone(&tool));
            keyed.push((key, tool));
        }
        inner
            .listed
            .sort_by(|left, right| left.0.cmp(&right.0));
        match inner
            .servers
            .iter_mut()
            .find(|(existing, _)| existing.id == server.id())
        {
            Some((_, status)) => *status = ServerStatus::Ready(keyed.len()),
            None => inner.servers.push((server, ServerStatus::Ready(keyed.len()))),
        }
        keyed
    }

    /// Marks one server as failed, with a one-line reason.
    pub(crate) fn mark_failed(&self, server: Arc<McpServerInfo>, reason: &str) {
        let mut inner = self.inner.write();
        inner.pending.remove(server.id());
        match inner
            .servers
            .iter_mut()
            .find(|(existing, _)| existing.id == server.id())
        {
            Some((_, status)) => *status = ServerStatus::Failed(reason.to_owned()),
            None => inner
                .servers
                .push((server, ServerStatus::Failed(reason.to_owned()))),
        }
    }

    /// Prompt note: ready servers with their tool counts, then the ids that
    /// are still connecting.
    ///
    /// The note deliberately does not claim the connecting servers' tools
    /// are callable; cycle 1 has no schemas for them.
    pub(crate) fn prompt_note(&self) -> String {
        let inner = self.inner.read();
        let mut note = String::new();
        if let Some(warning) = &self.warning {
            note.push_str("Project MCP config: ");
            note.push_str(warning);
            note.push('\n');
        }
        let ready: Vec<(String, usize, Option<&str>)> = inner
            .servers
            .iter()
            .filter_map(|(server, status)| match status {
                ServerStatus::Ready(count) => {
                    Some((server.id.clone(), *count, server.summary.as_deref()))
                }
                ServerStatus::Failed(_) => None,
            })
            .collect();
        if !ready.is_empty() {
            note.push_str("Connected MCP servers:\n");
            for (id, tools, summary) in &ready {
                let plural = if *tools == 1 { "tool" } else { "tools" };
                note.push_str(&format!("- {id} ({tools} {plural})"));
                if let Some(summary) = summary {
                    note.push_str(" — ");
                    note.push_str(summary);
                }
                note.push('\n');
            }
            note.push_str(
                "Their tools are on the tool list; `search_tool` loads a schema by name and \
                 `use_tool` calls one. `tool_search` finds them by keyword. Never guess \
                 parameters.\n",
            );
        }
        if !inner.pending.is_empty() {
            let ids: Vec<&str> = inner.pending.iter().map(String::as_str).collect();
            note.push_str(&format!(
                "\nMCP servers connecting in the background: {}.\nTheir tools are not on the \
                 tool list yet; once connected they appear there, or use search_tool + \
                 use_tool to call them the moment they register.\n",
                ids.join(", ")
            ));
        }
        note
    }

    fn list_text(&self) -> String {
        let inner = self.inner.read();
        let mut lines = Vec::new();
        // The reason a server is missing belongs in the answer: a config that
        // failed to load is not the same as no MCP configured.
        if let Some(warning) = &self.warning {
            lines.push(format!("MCP config: {warning}"));
        }
        if inner.listed.is_empty() {
            if inner.pending.is_empty() {
                lines.push("No MCP tools are connected.".to_owned());
            } else {
                let ids: Vec<&str> = inner.pending.iter().map(String::as_str).collect();
                lines.push(format!(
                    "No MCP tools are connected yet; servers {} are still connecting.",
                    ids.join(", ")
                ));
            }
        } else {
            for (name, summary) in &inner.listed {
                lines.push(format!("{name} — {summary}"));
            }
        }
        lines.join("\n")
    }

    /// Exact key first. Models also send a different case or prefix the
    /// server (`server.tool`, `server:tool`, `server/tool`, `server__tool`);
    /// those resolve when exactly one tool matches, preferring the tool on
    /// the named server.
    pub(crate) fn get(&self, name: &str) -> Option<(String, Arc<DynamicMcpTool>)> {
        let inner = self.inner.read();
        if let Some((key, tool)) = inner.by_name.get_key_value(name) {
            return Some((key.clone(), Arc::clone(tool)));
        }
        let (prefix, bare) = name
            .rsplit_once("__")
            .or_else(|| name.rsplit_once(['.', ':', '/']))
            .map_or((None, name), |(head, tail)| (Some(head), tail));
        // `mcp__docs__lookup` names the server as the last prefix segment.
        let server = prefix.and_then(|head| head.rsplit("__").next());
        let unique = |on_server: bool| {
            let mut found = inner.by_name.iter().filter(|(key, tool)| {
                key.eq_ignore_ascii_case(name)
                    || (tool.tool_name.eq_ignore_ascii_case(bare)
                        && (!on_server
                            || server
                                .is_some_and(|server| tool.server.id.eq_ignore_ascii_case(server))))
            });
            match (found.next(), found.next()) {
                (Some((key, tool)), None) => Some((key.clone(), Arc::clone(tool))),
                _ => None,
            }
        };
        unique(true).or_else(|| unique(false))
    }

    /// Tools whose key, server, or description contains every keyword
    /// (case-insensitive), sorted by key.
    fn search(&self, query: &str) -> Vec<(String, Arc<DynamicMcpTool>)> {
        let terms: Vec<String> = query
            .split(|ch: char| ch.is_whitespace() || ch == ',')
            .filter(|term| !term.is_empty())
            .map(str::to_lowercase)
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let inner = self.inner.read();
        let mut found: Vec<_> = inner
            .by_name
            .iter()
            .filter(|(key, tool)| {
                let haystack = format!(
                    "{} {} {}",
                    key.to_lowercase(),
                    tool.server.id.to_lowercase(),
                    tool.spec.description.to_lowercase()
                );
                terms.iter().all(|term| haystack.contains(term.as_str()))
            })
            .map(|(key, tool)| (key.clone(), Arc::clone(tool)))
            .collect();
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found
    }

    /// Nudge for a name that is on the registry this catalog was built
    /// from: the model sent a directly callable tool to `search_tool` /
    /// `use_tool`, so the miss should teach it to call that tool directly
    /// instead of describing the MCP catalog. Matching is ASCII-case-
    /// insensitive, aligned with [`McpCatalog::get`]'s fuzzy layer.
    fn native_hint(&self, name: &str) -> Option<String> {
        self.native.contains(&name.to_ascii_lowercase()).then(|| {
            format!(
                "{name} is not an MCP tool; it is already on the tool list. Call {name} \
                 directly with its own parameters."
            )
        })
    }

    /// Why a lookup misses: which servers are still handshaking, and which
    /// failed (they retry in the background).
    pub(crate) fn connecting_hint(&self) -> Option<String> {
        let inner = self.inner.read();
        let mut parts = Vec::new();
        if !inner.pending.is_empty() {
            let ids: Vec<&str> = inner.pending.iter().map(String::as_str).collect();
            parts.push(format!(
                "MCP server {} is still connecting; its tools are not available yet. Try \
                 again once it finishes.",
                ids.join(", ")
            ));
        }
        let failed: Vec<String> = inner
            .servers
            .iter()
            .filter_map(|(server, status)| match status {
                ServerStatus::Failed(reason) => Some(format!("{} ({reason})", server.id)),
                ServerStatus::Ready(_) => None,
            })
            .collect();
        if !failed.is_empty() {
            parts.push(format!(
                "MCP server {} failed to connect; it retries in the background.",
                failed.join(", ")
            ));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// One schema block per requested exact name, in request order. A name
    /// that resolves to nothing gets an error line instead of failing the
    /// whole batch; a name that is on the native tool list gets the
    /// call-it-directly nudge instead.
    fn schemas_text(&self, names: &[String]) -> String {
        let mut blocks: Vec<String> = names
            .iter()
            .take(MAX_BATCH_NAMES)
            .map(|name| match self.get(name) {
                Some((key, tool)) => schema_text(&key, &tool),
                None => {
                    let error = self
                        .native_hint(name)
                        .or_else(|| self.connecting_hint())
                        .unwrap_or_else(|| {
                            "no MCP tool has this name; call search_tool with keywords"
                                .to_owned()
                        });
                    format!("name: {name}\nerror: {error}")
                }
            })
            .collect();
        if names.len() > MAX_BATCH_NAMES {
            blocks.push(format!(
                "(only the first {MAX_BATCH_NAMES} names were loaded; call again for the rest)"
            ));
        }
        blocks.join(
            "

",
        )
    }
}

/// `search_tool`'s answer for one tool: its catalog key and full schema.
fn schema_text(key: &str, tool: &DynamicMcpTool) -> String {
    format!(
        "name: {key}\nserver: {}\ndescription: {}\ninputSchema: {}",
        tool.server.id, tool.spec.description, tool.spec.params_schema
    )
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// Most exact names one `search_tool` call resolves.
const MAX_BATCH_NAMES: usize = 10;

/// The `names` batch of a `search_tool` call: trimmed, non-empty entries.
fn arg_names(args: &Value) -> Vec<String> {
    args.get("names")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn arg_name(args: &Value) -> Result<String, ToolError> {
    args.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ToolError::InvalidArgs("name is required".to_owned()))
}

/// Registers the catalog-backed meta tools: `search_tool`, `use_tool`, and
/// `tool_search`.
///
/// They are registered before any server is ready so the model can ask for
/// a tool while its server is still connecting (and learn why it is not
/// there yet).
pub(crate) fn register_meta_tools(
    registry: &mycode_agent::ToolRegistry,
    catalog: &Arc<McpCatalog>,
    index: Arc<ToolSearchIndex>,
) {
    registry.register(Arc::new(SearchTool::new(Arc::clone(catalog))));
    registry.register(Arc::new(UseTool::new(Arc::clone(catalog))));
    registry.register(Arc::new(ToolSearchTool::new(
        Arc::clone(catalog),
        index,
    )));
}

/// Writes one ready server to the catalog, then to the registry.
///
/// The order is fixed: a `use_tool` hit in the catalog always finds its
/// registry spec. `index` grows with the same entries so `tool_search` only
/// ever sees callable tools.
pub(crate) fn register_ready_server(
    registry: &mycode_agent::ToolRegistry,
    catalog: &Arc<McpCatalog>,
    index: &ToolSearchIndex,
    server: Arc<McpServerInfo>,
    tools: Vec<Arc<DynamicMcpTool>>,
) {
    let keyed = catalog.insert_ready(server, tools);
    index.insert_many(keyed.iter().map(|(key, tool)| (key.as_str(), tool.as_ref())));
    for (key, tool) in keyed {
        registry.register(Arc::new(RegisteredMcpTool {
            key,
            inner: tool,
        }));
    }
}

/// Registry view of one catalog tool: the spec keeps the catalog key, so
/// `Request.tools` names the exact tool `use_tool` accepts.
struct RegisteredMcpTool {
    key: String,
    inner: Arc<DynamicMcpTool>,
}

impl ToolDyn for RegisteredMcpTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = self.inner.spec();
        spec.name.clone_from(&self.key);
        spec
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        self.inner.prompt_snippet_dyn()
    }

    fn parallel_safe(&self) -> bool {
        self.inner.parallel_safe()
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        self.inner.execute_dyn(args, ctx, out)
    }
}

/// Schema lookup for the connected MCP tools.
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
            description: "Find a connected MCP tool and load its schema. Pass `name`: an exact tool name for its inputSchema, keywords (for example \"github issue\") for the matching tools with one line each, or \"list\" for every tool. Pass `names` instead to load several exact names' inputSchemas in one call. Never guess parameters.".to_owned(),
            params_schema: object_schema(
                serde_json::json!({
                    "name": {"type": "string", "description": "An exact tool name, keywords, or \"list\"."},
                    "names": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": MAX_BATCH_NAMES,
                        "description": "Several exact tool names; returns each inputSchema."
                    }
                }),
                &[],
            ),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some(
            "search_tool: find MCP tools by keyword, or load inputSchemas by exact name (`names` for several), before use_tool. Never guess parameters.",
        )
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
            let names = arg_names(&args);
            if !names.is_empty() {
                return Ok(ToolResult::text(self.catalog.schemas_text(&names)));
            }
            let name = arg_name(&args)
                .map_err(|_| ToolError::InvalidArgs("name or names is required".to_owned()))?;
            if name == "list" {
                return Ok(ToolResult::text(self.catalog.list_text()));
            }
            if let Some((key, tool)) = self.catalog.get(&name) {
                return Ok(ToolResult::text(schema_text(&key, &tool)));
            }
            let found = self.catalog.search(&name);
            match found.as_slice() {
                // A keyword search that stays empty on a native tool name is
                // the model routing a directly callable tool through the MCP
                // bridge; nudge it back. Only the empty branch: several
                // keyword hits are ordinary results, not a misspelled name.
                [] => Err(match self.catalog.native_hint(&name) {
                    Some(hint) => ToolError::InvalidArgs(hint),
                    None => ToolError::InvalidArgs(match self.catalog.connecting_hint() {
                        Some(hint) => format!("no MCP tool matches {name}. {hint}"),
                        None => format!(
                            "no MCP tool matches {name}. Call search_tool with other keywords \
                             or name \"list\"."
                        ),
                    }),
                }),
                [(key, tool)] => Ok(ToolResult::text(schema_text(key, tool))),
                many => {
                    let mut text = format!(
                        "{} tools match \"{name}\". Call search_tool with one exact name for its inputSchema:",
                        many.len()
                    );
                    for (key, tool) in many.iter().take(MAX_SEARCH_MATCHES) {
                        text.push_str(&format!("\n{key} — {}", tool.snippet));
                    }
                    if many.len() > MAX_SEARCH_MATCHES {
                        text.push_str("\n(more match; narrow the keywords)");
                    }
                    Ok(ToolResult::text(text))
                }
            }
        })
    }
}

/// Calls one connected MCP tool by name.
///
/// MCP tools are expected to go through `search_tool` first. This
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
                    "name": {"type": "string", "description": "The exact name search_tool returned."},
                    "arguments": {"type": "object", "description": "Arguments matching search_tool's inputSchema."}
                }),
                &["name", "arguments"],
            ),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some("use_tool: call an MCP tool only after search_tool returned its schema.")
    }

    /// A call to a tool that declares `readOnlyHint` runs beside other
    /// reads.
    fn parallel_safe_call(&self, args: &Value) -> bool {
        arg_name(args)
            .ok()
            .and_then(|name| self.catalog.get(&name).map(|(_, tool)| tool.read_only))
            .unwrap_or(false)
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let name = arg_name(&args)?;
            let Some((_, tool)) = self.catalog.get(&name) else {
                return Err(match self.catalog.native_hint(&name) {
                    // The model sent a directly callable tool's name through
                    // the MCP bridge; teach it to call the tool itself.
                    Some(hint) => ToolError::InvalidArgs(hint),
                    None => match self.catalog.connecting_hint() {
                        Some(hint) => ToolError::InvalidArgs(format!(
                            "no MCP tool named {name} yet. {hint}"
                        )),
                        None => ToolError::InvalidArgs(format!(
                            "no MCP tool named {name}. Call search_tool with name \"list\"."
                        )),
                    },
                });
            };
            let mut arguments = args
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            // Models often send the object as a JSON string.
            if let Some(parsed) = arguments
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .filter(Value::is_object)
            {
                arguments = parsed;
            }
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
    server: &mycode_agent::McpServerSettings,
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
                    headers: server
                        .headers
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect(),
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
    home: &mycode_agent::HomeLayout,
    server: &mycode_agent::McpServerSettings,
) -> Result<Vec<String>, String> {
    let read_home = home.clone();
    let secrets =
        tokio::task::spawn_blocking(move || mycode_agent::read_provider_secrets(&read_home))
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

/// Test-only builder for one catalog tool that is never executed. The
/// `new` constructors stay private to this module, so cross-module tests
/// (subagent allowlists) need this to hand `register_ready_server` a tool.
#[cfg(test)]
pub(crate) fn stub_catalog_tool(
    server: &str,
    name: &str,
    description: &str,
) -> Arc<DynamicMcpTool> {
    stub_catalog_tool_with_read_only(server, name, description, true)
}

/// Test-only builder with an explicit `readOnlyHint`, so a test can exercise
/// the read-only filter a child role applies.
#[cfg(test)]
pub(crate) fn stub_catalog_tool_with_read_only(
    server: &str,
    name: &str,
    description: &str,
    read_only: bool,
) -> Arc<DynamicMcpTool> {
    struct DeadChannel;
    impl crate::mcp_client::JsonRpcChannel for DeadChannel {
        fn request<'a>(
            &'a self,
            _id: u64,
            method: &'a str,
            _params: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, crate::mcp_client::McpError>> + Send + 'a>>
        {
            Box::pin(async move { Err(crate::mcp_client::McpError::protocol(method)) })
        }
        fn notify<'a>(
            &'a self,
            _method: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::mcp_client::McpError>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }
    Arc::new(DynamicMcpTool::new(
        Arc::new(McpServerInfo::new(server, None)),
        crate::mcp_client::McpTool {
            name: name.to_owned(),
            description: Some(description.to_owned()),
            input_schema: serde_json::json!({"type": "object"}),
            read_only,
        },
        Arc::new(Mutex::new(crate::mcp_client::McpClient::new(Arc::new(
            DeadChannel,
        )))),
        Arc::new(AtomicBool::new(false)),
        Arc::new(std::env::temp_dir()),
    ))
}

#[cfg(test)]
mod tests;
