//! reqwest-backed [`WebTransport`] production implementation.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{WebError, WebTransport};

/// Production transport for the bounded web client.
#[derive(Clone, Default)]
pub struct ReqwestWebTransport;

impl ReqwestWebTransport {
    /// Checks that the TLS backend starts. Requests pin addresses themselves.
    ///
    /// # Errors
    ///
    /// Returns [`WebError::Unavailable`] when the TLS backend fails.
    pub fn new() -> Result<Self, WebError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| WebError::unavailable(format!("http client unavailable: {error}")))?;
        Ok(Self)
    }
}

/// Bytes of a non-2xx body inspected for an AnySearch error object.
const ERROR_BODY_LIMIT: usize = 8 * 1024;

/// `HTTP {status}` plus AnySearch `error_code` and `message` when the body has them.
///
/// A non-JSON body, or JSON without those fields, stays the status alone.
pub(super) fn http_error_reason(status: reqwest::StatusCode, body: &[u8]) -> String {
    let status_text = format!("HTTP {status}");
    let detail = serde_json::from_slice::<serde_json::Value>(body_prefix(body))
        .ok()
        .and_then(|value| super::anysearch_error_detail(&value));
    match detail {
        Some(detail) => format!("{status_text} ({detail})"),
        None => status_text,
    }
}

fn body_prefix(body: &[u8]) -> &[u8] {
    let mut end = body.len().min(ERROR_BODY_LIMIT);
    // A cut mid-codepoint would make the JSON parser reject an otherwise valid body.
    if end < body.len() {
        while end > 0 && body[end] & 0b1100_0000 == 0b1000_0000 {
            end -= 1;
        }
    }
    &body[..end]
}

/// Maps a [`mycode_providers::send_pinned`] failure onto the web client error.
///
/// Cancellation stays a distinct variant. Every other failure keeps its
/// reason so `web_search` and `fetch_content` can show it.
pub(super) fn pinned_transport_error(message: &str) -> WebError {
    if message == "request cancelled" {
        WebError::Cancelled
    } else {
        WebError::unavailable(message)
    }
}

#[async_trait::async_trait]
impl WebTransport for ReqwestWebTransport {
    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: &[u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError> {
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "application/json".to_owned()),
        ];
        if let Some(key) = bearer {
            // Shared sanitizer: strip a pasted `Bearer <scheme> <key>` down
            // to the raw key before it rides bearer_auth.
            let key = crate::mcp_client::strip_bearer_prefix(key);
            headers.push(("authorization".to_owned(), format!("Bearer {key}")));
        }
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            sent = mycode_providers::send_pinned(mycode_providers::PinnedRequest {
                method: reqwest::Method::POST,
                url: endpoint.to_owned(),
                headers,
                body: Some(mycode_providers::PinnedBody::Bytes(body.to_vec().into())),
                mode: mycode_providers::PinMode::PublicHttps,
                timeout: Some(timeout),
                user_agent: None,
                cancel: cancel.clone(),
            }) => sent.map_err(|message| pinned_transport_error(&message))?,
        };
        if !response.status().is_success() {
            let status = response.status();
            let bytes = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(WebError::Cancelled),
                bytes = response.bytes() => bytes,
            };
            let body = bytes.unwrap_or_default();
            return Err(WebError::unavailable(http_error_reason(status, &body)));
        }
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            bytes = response.bytes() => bytes.map_err(|_| WebError::Protocol)?,
        };
        Ok(bytes.to_vec())
    }
}
