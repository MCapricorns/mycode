//! Injectable SSE transport seam.
//!
//! The runtime owns all egress. Adapters describe one HTTP call; the
//! [`SseTransport`] implementation opens it and returns the raw response byte
//! stream. Production uses [`ReqwestTransport`]; tests inject deterministic
//! byte streams without any network.

use std::future::Future;
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
///
/// Each request builds a client that does not follow redirects and pins the
/// resolved addresses. [`ReqwestTransport::new`] builds a throwaway client to
/// check that TLS can start. The stored value is the idle ceiling between
/// body chunks, not a total-request deadline.
#[derive(Clone)]
pub struct ReqwestTransport {
    read_timeout: Duration,
}

impl ReqwestTransport {
    /// Checks that the TLS backend can initialize.
    ///
    /// Uses [`READ_TIMEOUT`] as the idle ceiling between body chunks.
    ///
    /// # Errors
    ///
    /// Returns an unavailable error when the TLS backend cannot initialize.
    pub fn new() -> Result<Self, ProviderError> {
        Self::with_read_timeout(READ_TIMEOUT)
    }

    /// Checks that TLS can start, with `read_timeout` as the idle ceiling.
    ///
    /// # Errors
    ///
    /// Returns an unavailable error when the TLS backend cannot initialize.
    pub fn with_read_timeout(read_timeout: Duration) -> Result<Self, ProviderError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| ProviderError::new(ProviderErrorKind::Unavailable))?;
        Ok(Self { read_timeout })
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
        let mut headers = call.headers;
        headers.push((
            reqwest::header::CONTENT_TYPE.to_string(),
            "application/json".to_owned(),
        ));
        let response = crate::http_pin::send_pinned(crate::http_pin::PinnedRequest {
            method: reqwest::Method::POST,
            url: call.endpoint,
            headers,
            body: Some(crate::http_pin::PinnedBody::Bytes(call.body.into())),
            mode: crate::http_pin::PinMode::CheckRedirect,
            timeout: None,
            read_timeout: Some(self.read_timeout),
            user_agent: None,
            follow_redirects: true,
            cancel: cancel.clone(),
        })
        .await
        .map_err(|message| {
            if cancel.is_cancelled() || message == "request cancelled" {
                ProviderError::new(ProviderErrorKind::Cancelled)
            } else {
                ProviderError::with_message(ProviderErrorKind::Unavailable, message)
            }
        })?;
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
            idle: self.read_timeout,
            waiting: None,
        };
        Ok(Box::pin(guarded))
    }
}

/// Ends the stream once cancellation fires, and fails when a chunk gap
/// exceeds the idle ceiling. Each delivered chunk starts the wait over.
struct CancellableStream<S> {
    inner: S,
    cancel: CancellationToken,
    idle: Duration,
    waiting: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<S> Stream for CancellableStream<S>
where
    S: Stream<Item = Result<Bytes, ProviderError>> + Unpin,
{
    type Item = Result<Bytes, ProviderError>;

    fn poll_next(
        self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.cancel.is_cancelled() {
            return std::task::Poll::Ready(Some(Err(ProviderError::new(
                ProviderErrorKind::Cancelled,
            ))));
        }
        match Pin::new(&mut this.inner).poll_next(context) {
            std::task::Poll::Ready(item) => {
                this.waiting = None;
                std::task::Poll::Ready(item)
            }
            std::task::Poll::Pending => {
                if this.waiting.is_none() {
                    this.waiting = Some(Box::pin(tokio::time::sleep(this.idle)));
                }
                let waiting = this.waiting.as_mut().expect("idle wait");
                match waiting.as_mut().poll(context) {
                    std::task::Poll::Ready(()) => {
                        this.waiting = None;
                        std::task::Poll::Ready(Some(Err(ProviderError::new(
                            ProviderErrorKind::Timeout,
                        ))))
                    }
                    std::task::Poll::Pending => std::task::Poll::Pending,
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio_util::sync::CancellationToken;

    use super::{ReqwestTransport, SseTransport, TransportCall};

    fn spawn_drip_server(pieces: usize, gap: Duration) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut tmp = [0_u8; 1024];
            while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut tmp) {
                    Ok(0) | Err(_) => return,
                    Ok(count) => buf.extend_from_slice(&tmp[..count]),
                }
            }
            let body: String = (0..pieces).map(|index| format!("data:{index}\n")).collect();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if stream.write_all(header.as_bytes()).is_err() {
                return;
            }
            let _ = stream.flush();
            for piece in body.split_inclusive('\n') {
                std::thread::sleep(gap);
                if stream.write_all(piece.as_bytes()).is_err() {
                    return;
                }
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/v1/chat/completions")
    }

    #[tokio::test]
    async fn a_slow_but_alive_stream_outlives_the_idle_limit() {
        let idle = Duration::from_millis(200);
        let gap = Duration::from_millis(80);
        let pieces = 5;
        let url = spawn_drip_server(pieces, gap);
        let transport = ReqwestTransport::with_read_timeout(idle).expect("transport");
        let started = std::time::Instant::now();
        let mut stream = transport
            .post(
                TransportCall {
                    endpoint: url,
                    headers: Vec::new(),
                    body: b"{}".to_vec(),
                },
                CancellationToken::new(),
            )
            .await
            .expect("post");
        let mut collected = Vec::new();
        let outcome = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(bytes) => collected.extend_from_slice(&bytes),
                    Err(error) => panic!("stream failed: {error}"),
                }
            }
            collected
        })
        .await;
        let collected = outcome.expect("stream hung");
        assert!(
            started.elapsed() > idle,
            "stream finished in {:?}, under the idle limit",
            started.elapsed()
        );
        let text = String::from_utf8(collected).expect("utf8");
        let expected: String = (0..pieces).map(|index| format!("data:{index}\n")).collect();
        assert_eq!(text, expected);
    }
}
