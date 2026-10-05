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
            .map_err(|_| WebError::Unavailable)?;
        Ok(Self)
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
                body: Some(mycode_providers::PinnedBody::Bytes(body.to_vec())),
                mode: mycode_providers::PinMode::PublicHttps,
                timeout: Some(timeout),
                user_agent: None,
                cancel: cancel.clone(),
            }) => sent.map_err(|_| WebError::Unavailable)?,
        };
        if !response.status().is_success() {
            return Err(WebError::Unavailable);
        }
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            bytes = response.bytes() => bytes.map_err(|_| WebError::Protocol)?,
        };
        Ok(bytes.to_vec())
    }
}
