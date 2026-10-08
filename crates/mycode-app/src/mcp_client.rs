//! First-party bounded MCP client.
//!
//! Speaks JSON-RPC 2.0 against configured servers over two transports:
//! newline-delimited stdio for local commands and the MCP Streamable-HTTP
//! wire for remote endpoints.
//!
//! Bounds: one MiB per message, 128 tools per server, per-request timeouts,
//! and strict https for HTTP endpoints. The [`JsonRpcChannel`] seam
//! abstracts the byte transport shared by both.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

pub mod http_channel;
pub mod stdio_channel;

pub use http_channel::{HttpChannel, HttpChannelOptions, KeyHeader};
pub use stdio_channel::StdioChannel;

/// One JSON-RPC request/response hop over a configured MCP server.
///
/// Implementations own the wire envelope: they wrap `id`, `method`, and
/// `params` into a JSON-RPC request, wait for the response carrying the same
/// id, map JSON-RPC error objects to [`McpError::Server`], and return the
/// bare `result` value.
#[async_trait::async_trait]
pub trait JsonRpcChannel: Send + Sync + 'static {
    /// Performs one request and returns the bare `result` value.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport, timeout, and protocol failures.
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError>;

    /// Sends one notification that expects no response.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport failures.
    async fn notify(&self, method: &str) -> Result<(), McpError>;

    /// Releases the transport gracefully; stdio closes the child's stdin
    /// and waits before killing it. Stateless transports do nothing.
    async fn shutdown(&self) {}
}

/// Maximum accepted response message bytes.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Maximum tools listed per server.
pub const MAX_TOOLS: usize = 128;
/// Per-request timeout every MCP channel is constructed with: it bounds each
/// JSON-RPC hop for the channel's whole lifetime — the handshake, tools/list,
/// and every tools/call a turn issues. Both the settings-page probe and the
/// turn session channel pass this constant.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Protocol revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Longest error detail kept from a server or a child's stderr.
const MAX_DETAIL_CHARS: usize = 512;

/// Users paste either the raw key or `Bearer <key>`; the header always
/// carries exactly one `Bearer ` prefix. Shared by the MCP HTTP channel and
/// the web client transport, which both send bearer credentials.
pub(crate) fn strip_bearer_prefix(key: &str) -> &str {
    let trimmed = key.trim();
    trimmed
        .split_once(char::is_whitespace)
        .and_then(|(scheme, rest)| scheme.eq_ignore_ascii_case("bearer").then_some(rest.trim()))
        .unwrap_or(trimmed)
}

/// Errors surfaced by the MCP client.
///
/// Every variant that can carry a cause does, bounded to
/// [`MAX_DETAIL_CHARS`]: a bare "transport failure" gives the user nothing
/// to act on when a stdio server fails to start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// The server violated the MCP wire contract.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// A request exceeded its deadline.
    #[error("request timed out")]
    Timeout,
    /// The transport failed before a response arrived.
    #[error("{0}")]
    Transport(String),
    /// The server returned a JSON-RPC error object.
    #[error("server error: {0}")]
    Server(String),
    /// The response exceeded the message bound.
    #[error("response exceeded the size bound")]
    Oversized,
}

impl McpError {
    /// Builds a [`McpError::Protocol`] with a bounded detail.
    #[must_use]
    pub fn protocol(detail: impl Into<String>) -> Self {
        Self::Protocol(bound_detail(detail.into()))
    }

    /// Builds a [`McpError::Transport`] with a bounded detail.
    #[must_use]
    pub fn transport(detail: impl Into<String>) -> Self {
        Self::Transport(bound_detail(detail.into()))
    }

    /// Builds a [`McpError::Server`] from a JSON-RPC error object.
    #[must_use]
    pub fn server(error: &Value) -> Self {
        let code = error["code"].as_i64();
        let message = error["message"].as_str().unwrap_or("unspecified error");
        Self::Server(bound_detail(match code {
            Some(code) => format!("{message} (code {code})"),
            None => message.to_owned(),
        }))
    }

    /// True when the underlying connection is unusable and the caller
    /// should reconnect rather than retry on the same channel.
    #[must_use]
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Timeout | Self::Oversized)
    }
}

/// Trims one detail string to [`MAX_DETAIL_CHARS`] and a single line.
fn bound_detail(detail: String) -> String {
    let collapsed: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_DETAIL_CHARS {
        collapsed
    } else {
        let head: String = collapsed.chars().take(MAX_DETAIL_CHARS).collect();
        format!("{head}\u{2026}")
    }
}

/// One tool exposed by a server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// Tool name, unique per server.
    pub name: String,
    /// Human-readable description, when provided.
    pub description: Option<String>,
    /// JSON Schema for the tool arguments.
    pub input_schema: Value,
}

/// The outcome of one `tools/call`.
///
/// `is_error` mirrors the MCP `isError` flag: the tool ran and reported a
/// failure *as data*, which the model should see verbatim so it can correct
/// its arguments. Wire and server faults are [`McpError`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpToolOutput {
    /// Concatenated text content blocks.
    pub text: String,
    /// The server flagged the result as a tool-level error.
    pub is_error: bool,
}

/// A connected, initialized MCP client bound to one channel.
pub struct McpClient {
    channel: Arc<dyn JsonRpcChannel>,
    next_id: u64,
}

impl McpClient {
    /// Creates one client over a transport channel.
    #[must_use]
    pub fn new(channel: Arc<dyn JsonRpcChannel>) -> Self {
        Self {
            channel,
            next_id: 1,
        }
    }

    /// Performs the MCP initialize handshake and sends the initialized
    /// notification.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport and contract failures.
    pub async fn initialize(&mut self) -> Result<Value, McpError> {
        let result = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "mycode",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
            .await?;
        self.channel.notify("notifications/initialized").await?;
        Ok(result)
    }

    /// Lists the tools the server exposes, bounded by [`MAX_TOOLS`].
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport and contract failures.
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.call("tools/list", json!({})).await?;
        let Some(tools) = result["tools"].as_array() else {
            return Err(McpError::protocol("tools/list result has no tools array"));
        };
        if tools.len() > MAX_TOOLS {
            return Err(McpError::Oversized);
        }
        Ok(tools
            .iter()
            .filter_map(|tool| {
                // A nameless tool cannot be called; skip it instead of
                // advertising an empty name to the model.
                let name = tool["name"].as_str()?;
                if name.is_empty() {
                    return None;
                }
                Some(McpTool {
                    name: name.to_owned(),
                    description: tool["description"].as_str().map(str::to_owned),
                    input_schema: match &tool["inputSchema"] {
                        Value::Null => json!({"type": "object"}),
                        schema => schema.clone(),
                    },
                })
            })
            .collect())
    }

    /// Calls one tool and returns its concatenated text content together
    /// with the server's `isError` flag.
    ///
    /// # Errors
    ///
    /// Returns [`McpError`] for transport and contract failures.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<McpToolOutput, McpError> {
        let result = self
            .call("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        let mut text = String::new();
        if let Some(blocks) = result["content"].as_array() {
            for block in blocks {
                let part = match block["type"].as_str() {
                    Some("text") => block["text"].as_str().map(str::to_owned),
                    // Non-text blocks are summarized so the model learns the
                    // tool produced something it cannot read inline.
                    Some("image") | Some("audio") => Some(format!(
                        "[{} content omitted: {}]",
                        block["type"].as_str().unwrap_or("binary"),
                        block["mimeType"].as_str().unwrap_or("unknown type")
                    )),
                    Some("resource") => block["resource"]["text"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| {
                            block["resource"]["uri"]
                                .as_str()
                                .map(|uri| format!("[resource {uri}]"))
                        }),
                    _ => None,
                };
                if let Some(part) = part {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&part);
                }
            }
        }
        // Servers that return structuredContent only still owe the model a
        // readable body.
        if text.is_empty() && !result["structuredContent"].is_null() {
            text = result["structuredContent"].to_string();
        }
        Ok(McpToolOutput {
            text,
            is_error: result["isError"].as_bool() == Some(true),
        })
    }

    /// Releases the underlying transport (see [`JsonRpcChannel::shutdown`]).
    pub async fn shutdown(&self) {
        self.channel.shutdown().await;
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.channel.request(id, method, params).await
    }
}
