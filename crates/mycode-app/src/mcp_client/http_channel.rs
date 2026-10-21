//! Streamable-HTTP transport for remote MCP endpoints.
//!
//! One JSON-RPC request per POST; the response body may be a single JSON
//! object or an SSE stream whose `data:` frames carry the reply. A returned
//! `Mcp-Session-Id` header is captured and replayed on later requests.
//! Credential headers come from the server binding; keys live in the secret
//! store and never travel through settings.

use std::time::Duration;

use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::{JsonRpcChannel, MAX_MESSAGE_BYTES, McpError};

/// Bytes of a non-2xx body kept for the error excerpt.
const ERROR_EXCERPT_BYTES: usize = 300;

/// Reads the response body up to `limit` bytes, rejecting anything longer.
///
/// The bound applies while reading so a misbehaving endpoint cannot buffer
/// an unbounded body before the size check runs.
async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, McpError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| McpError::transport(format!("response body failed: {error}")))?
    {
        if body.len() + chunk.len() > limit {
            return Err(McpError::Oversized);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Credential header styles accepted for HTTP MCP servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyHeader {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
}

impl KeyHeader {
    /// Parses the settings vocabulary.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("x-api-key") => Self::XApiKey,
            _ => Self::Bearer,
        }
    }
}

/// Options for one HTTP channel.
#[derive(Clone)]
pub struct HttpChannelOptions {
    /// Credential header style.
    pub key_header: KeyHeader,
    /// API key, when the server requires one.
    pub api_key: Option<String>,
    /// Extra headers (magpie's Library keeps vendor headers here).
    pub headers: Vec<(String, String)>,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// One HTTP MCP session bound to an endpoint.
pub struct HttpChannel {
    endpoint: String,
    options: HttpChannelOptions,
    session_id: RwLock<Option<String>>,
}

impl HttpChannel {
    /// Creates one channel over the endpoint. Every request goes through the
    /// pinned sender, which builds its own client per call.
    #[must_use]
    pub fn new(endpoint: &str, options: HttpChannelOptions) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            options,
            session_id: RwLock::new(None),
        }
    }

    /// Posts one envelope. `id` names the reply to pick out of an SSE body;
    /// a notification passes `None`.
    async fn post_envelope(
        &self,
        envelope: Value,
        id: Option<u64>,
    ) -> Result<Option<Value>, McpError> {
        let body =
            serde_json::to_vec(&envelope).map_err(|error| McpError::protocol(error.to_string()))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Oversized);
        }
        let mut headers = vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            (
                "Accept".to_owned(),
                "application/json, text/event-stream".to_owned(),
            ),
            (
                "MCP-Protocol-Version".to_owned(),
                super::PROTOCOL_VERSION.to_owned(),
            ),
        ];
        if let Some(session) = self.session_id.read().await.as_deref() {
            headers.push(("Mcp-Session-Id".to_owned(), session.to_owned()));
        }
        if let Some(key) = &self.options.api_key {
            let key = super::strip_bearer_prefix(key);
            match self.options.key_header {
                KeyHeader::Bearer => {
                    headers.push(("Authorization".to_owned(), format!("Bearer {key}")));
                }
                KeyHeader::XApiKey => headers.push(("x-api-key".to_owned(), key.to_owned())),
            }
        }
        headers.extend(self.options.headers.iter().cloned());
        let response = mycode_providers::send_pinned(mycode_providers::PinnedRequest {
            method: reqwest::Method::POST,
            url: self.endpoint.clone(),
            headers,
            body: Some(mycode_providers::PinnedBody::Bytes(body.into())),
            mode: mycode_providers::PinMode::CheckRedirect,
            timeout: Some(self.options.timeout),
            read_timeout: None,
            user_agent: None,
            follow_redirects: true,
            cancel: tokio_util::sync::CancellationToken::new(),
        })
        .await
        .map_err(|error| {
            if error.contains("timed out") || error.contains("timeout") {
                McpError::Timeout
            } else {
                McpError::transport(format!("request to {} failed: {error}", self.endpoint))
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            // The body usually names the cause (bad key, unknown session);
            // keep a short excerpt so the settings page can show it.
            let excerpt = bounded_body(response, ERROR_EXCERPT_BYTES)
                .await
                .unwrap_or_default();
            let excerpt = String::from_utf8_lossy(&excerpt).into_owned();
            let hint = match status.as_u16() {
                401 | 403 => " — check the API key stored for this server",
                404 => " — the session expired or the endpoint path is wrong",
                _ => "",
            };
            return Err(McpError::transport(format!(
                "HTTP {status}{hint}: {}",
                excerpt.trim()
            )));
        }
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            *self.session_id.write().await = Some(session.to_owned());
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let bytes = bounded_body(response, MAX_MESSAGE_BYTES).await?;
        if content_type.contains("text/event-stream") {
            return sse_reply(&bytes, id);
        }
        if bytes.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| McpError::protocol(format!("response is not JSON: {error}")))?;
        Ok(Some(value))
    }
}

/// Extracts the JSON-RPC response from an SSE body.
///
/// Events follow the SSE rules: consecutive `data:` lines join with `\n`
/// and a blank line ends the event, so a reply split across lines parses.
/// Server-initiated requests and notifications on the stream carry a
/// `method` and are skipped; with `id` set, only that request's reply (or an
/// id-less error) is returned.
fn sse_reply(bytes: &[u8], id: Option<u64>) -> Result<Option<Value>, McpError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| McpError::protocol("SSE body is not UTF-8"))?;
    let mut data = String::new();
    let mut has_data = false;
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if has_data
                && let Some(value) = sse_event(&data)?
                && is_reply(&value, id)
            {
                return Ok(Some(value));
            }
            data.clear();
            has_data = false;
            continue;
        }
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        if has_data {
            data.push('\n');
        }
        data.push_str(payload.strip_prefix(' ').unwrap_or(payload));
        has_data = true;
    }
    Ok(None)
}

/// Parses one SSE event's data; `[DONE]` and empty data carry nothing.
fn sse_event(data: &str) -> Result<Option<Value>, McpError> {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data)
        .map(Some)
        .map_err(|error| McpError::protocol(format!("SSE frame is not JSON: {error}")))
}

/// Whether one stream message answers the request.
fn is_reply(value: &Value, id: Option<u64>) -> bool {
    if value.get("method").is_some() {
        return false;
    }
    let has_error = value["error"].is_object();
    match id {
        Some(id) => value["id"].as_u64() == Some(id) || (has_error && value["id"].is_null()),
        None => value["id"].is_u64() || has_error,
    }
}

/// Completes one JSON-RPC request's reply value.
///
/// Unlike notifications, a request requires a response object carrying `id`
/// or an `error` object; an empty body is a protocol violation.
fn finish_reply(value: Option<Value>, id: u64) -> Result<Value, McpError> {
    let Some(value) = value else {
        return Err(McpError::protocol(
            "server returned an empty response to a request",
        ));
    };
    if let Some(error) = value.get("error").filter(|error| error.is_object()) {
        return Err(McpError::server(error));
    }
    if value["id"].as_u64() != Some(id) {
        return Err(McpError::protocol(format!(
            "reply id {} does not match request id {id}",
            value["id"]
        )));
    }
    Ok(value["result"].clone())
}

impl JsonRpcChannel for HttpChannel {
    fn request<'a>(
        &'a self,
        id: u64,
        method: &'a str,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send + 'a>> {
        Box::pin(async move {
            let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
            });
            let reply = self.post_envelope(envelope, Some(id)).await?;
            finish_reply(reply, id)
        })
    }

    fn notify<'a>(
        &'a self,
        method: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
        Box::pin(async move {
            let envelope = json!({
            "jsonrpc": "2.0",
            "method": method,
            });
            // 202/empty responses are success; transport failures propagate.
            self.post_envelope(envelope, None).await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::sse_reply;

    #[test]
    fn sse_reply_joins_multi_line_data_and_skips_server_messages() {
        let body = concat!(
            ": keep-alive\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n",
            "\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"ping\"}\n",
            "\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":3,\n",
            "data: \"result\":{\"ok\":true}}\n",
            "\n",
        );
        let reply = sse_reply(body.as_bytes(), Some(3)).unwrap().expect("reply");
        assert_eq!(reply["id"], 3);
        assert_eq!(reply["result"]["ok"], true);
    }

    #[test]
    fn sse_reply_ignores_other_ids_and_keeps_bare_errors() {
        let other = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert!(sse_reply(other.as_bytes(), Some(2)).unwrap().is_none());
        let error = "data: {\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32600}}";
        let reply = sse_reply(error.as_bytes(), Some(2))
            .unwrap()
            .expect("error");
        assert!(reply["error"].is_object());
        assert!(sse_reply(b"data: [DONE]\n\n", Some(2)).unwrap().is_none());
    }

    #[test]
    fn finish_reply_rejects_missing_response_object() {
        let err = super::finish_reply(None, 7).expect_err("empty request response must fail");
        assert!(
            matches!(err, super::McpError::Protocol(ref msg) if msg.contains("empty response")),
            "{err:?}"
        );
    }
}
