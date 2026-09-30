//! Injectable SSE transport seam.
//!
//! The runtime owns all egress. Adapters describe one HTTP call; the
//! [`SseTransport`] implementation opens it and returns the raw response byte
//! stream. Production uses [`ReqwestTransport`]; tests inject deterministic
//! byte streams without any network.

use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use tokio_util::sync::CancellationToken;

use mycode_core::{ProviderError, ProviderErrorKind};

/// Default ceiling for establishing a connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Idle ceiling between response body chunks.
///
/// A peer that connects and then stops sending must not pin a turn until
/// the user notices. Models that keep streaming reset the timer on every
/// chunk, so a long generation is unaffected.
pub const READ_TIMEOUT: Duration = Duration::from_secs(180);

/// One outbound POST described by an adapter.
#[derive(Debug, Clone)]
pub struct TransportCall {
    /// Full HTTPS endpoint URL.
    pub endpoint: String,
    /// Extra request headers (auth, protocol version, user agent).
    pub headers: Vec<(String, String)>,
    /// Serialized JSON request body.
    pub body: Vec<u8>,
}

/// A live response body stream.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>;

/// Opens SSE POST requests for provider adapters.
///
/// Implementations must honor cancellation by ending the returned stream
/// promptly once `cancel` fires.
#[async_trait::async_trait]
pub trait SseTransport: Send + Sync + 'static {
    /// Starts one POST and returns the raw response body.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ProviderError`] for connection, status, and timeout
    /// failures; body-level failures surface through the stream.
    async fn post(
        &self,
        call: TransportCall,
        cancel: CancellationToken,
    ) -> Result<ByteStream, ProviderError>;
}

/// reqwest-backed production transport.
#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Builds the shared client with bounded connect and header deadlines.
    ///
    /// # Errors
    ///
    /// Returns an unavailable error when the TLS backend cannot initialize.
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|_| ProviderError::new(ProviderErrorKind::Unavailable))?;
        Ok(Self { client })
    }
}

impl Default for ReqwestTransport {
    /// # Panics
    ///
    /// Panics when the TLS backend cannot initialize; callers preferring
    /// graceful failure use [`ReqwestTransport::new`].
    fn default() -> Self {
        Self::new().expect("reqwest client must initialize")
    }
}

#[async_trait::async_trait]
impl SseTransport for ReqwestTransport {
    async fn post(
        &self,
        call: TransportCall,
        cancel: CancellationToken,
    ) -> Result<ByteStream, ProviderError> {
        let mut request = self
            .client
            .post(&call.endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(call.body.clone());
        for (name, value) in &call.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let mut fetch = std::pin::pin!(request.send());
        let response = tokio::select! {
            sent = &mut fetch => sent.map_err(map_reqwest_error)?,
            () = cancel.cancelled() => return Err(ProviderError::new(ProviderErrorKind::Cancelled)),
        };
        let response = match cancel.is_cancelled() {
            true => return Err(ProviderError::new(ProviderErrorKind::Cancelled)),
            false => response,
        };
        let status = response.status();
        if !status.is_success() {
            let body = tokio::select! {
                text = response.text() => text.unwrap_or_default(),
                () = cancel.cancelled() => return Err(ProviderError::new(ProviderErrorKind::Cancelled)),
            };
            return Err(ProviderError::with_message(
                status_error_kind(status),
                format!("HTTP {status}: {}", body.as_str()),
            ));
        }
        let stream = response.bytes_stream();
        let cancel_for_body = cancel.clone();
        let mapped = futures_util::stream::StreamExt::map(stream, move |chunk| {
            chunk.map_err(map_reqwest_error)
        });
        let guarded = CancellableStream {
            inner: mapped,
            cancel: cancel_for_body,
        };
        Ok(Box::pin(guarded))
    }
}

/// Ends the stream once cancellation fires without polling the body.
struct CancellableStream<S> {
    inner: S,
    cancel: CancellationToken,
}

impl<S> Stream for CancellableStream<S>
where
    S: Stream<Item = Result<Bytes, ProviderError>> + Unpin,
{
    type Item = Result<Bytes, ProviderError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.cancel.is_cancelled() {
            return std::task::Poll::Ready(Some(Err(ProviderError::new(
                ProviderErrorKind::Cancelled,
            ))));
        }
        Pin::new(&mut self.inner).poll_next(context)
    }
}

fn map_reqwest_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError::new(ProviderErrorKind::Timeout)
    } else if error.is_connect() || error.is_request() {
        ProviderError::new(ProviderErrorKind::Unavailable)
    } else {
        ProviderError::with_message(ProviderErrorKind::Unavailable, "transport failed")
    }
}

fn status_error_kind(status: reqwest::StatusCode) -> ProviderErrorKind {
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        ProviderErrorKind::Unavailable
    } else if status.is_client_error() {
        ProviderErrorKind::Rejected
    } else {
        ProviderErrorKind::Protocol
    }
}
