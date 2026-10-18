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
///
/// Items arrive through a small bounded channel: one `Ok` per body chunk,
/// one final `Err` for a transport failure (idle timeout, connection
/// error, cancellation) when applicable, then `None` ends the body.
pub type ByteStream = tokio::sync::mpsc::Receiver<Result<Bytes, ProviderError>>;

/// Chunks buffered between the reader task and the consumer. Bounds
/// read-ahead without stalling a fast SSE peer.
const BYTE_STREAM_CAPACITY: usize = 4;

/// Opens SSE POST requests for provider adapters.
///
/// Implementations must honor cancellation by ending the returned stream
/// promptly once `cancel` fires.
///
/// The method returns a boxed future so the trait stays object-safe for
/// `dyn SseTransport` dispatch; implementers write `Box::pin(async move { … })`.
pub trait SseTransport: Send + Sync + 'static {
    /// Starts one POST and returns the raw response body.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ProviderError`] for connection, status, and timeout
    /// failures; body-level failures surface through the stream.
    fn post<'a>(
        &'a self,
        call: TransportCall,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ByteStream, ProviderError>> + Send + 'a>>;
}

/// reqwest-backed production transport.
///
/// Each request builds a client that does not follow redirects; a direct
/// hop pins the resolved addresses while a proxied one lets the proxy
/// resolve the host. [`ReqwestTransport::new`] checks once per process that
/// TLS can start. The stored value is the idle ceiling between
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
        // The TLS backend either starts in this process or it does not, so
        // the probe client is built once instead of on every turn.
        static TLS_PROBE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
        TLS_PROBE
            .get_or_init(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(CONNECT_TIMEOUT)
                    .build()
                    .map(drop)
                    .map_err(|error| format!("TLS backend failed to start: {error}"))
            })
            .clone()
            .map_err(|message| {
                ProviderError::with_message(ProviderErrorKind::Unavailable, message)
            })?;
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

impl SseTransport for ReqwestTransport {
    fn post<'a>(
        &'a self,
        call: TransportCall,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ByteStream, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
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
            let idle = self.read_timeout;
            if !status.is_success() {
                // A server that answers with an error status and then stalls
                // must not pin the turn: the idle ceiling bounds the
                // error-body read the same way it bounds body chunks. The
                // status still decides the error kind, so a stalled 400 is not
                // retried as a transient timeout.
                let body = tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        return Err(ProviderError::new(ProviderErrorKind::Cancelled));
                    }
                    text = read_error_body(response, idle) => match text {
                        Ok(body) => body,
                        Err(()) => "(the error body timed out)".to_owned(),
                    },
                };
                return Err(ProviderError::with_message(
                    status_error_kind(status),
                    format!("HTTP {status}: {}", body.as_str()),
                )
                .with_status(status.as_u16()));
            }
            let cancel_for_body = cancel;
            let (chunk_tx, chunk_rx) =
                tokio::sync::mpsc::channel::<Result<Bytes, ProviderError>>(BYTE_STREAM_CAPACITY);
            // Reads the body chunk by chunk on a dedicated task and forwards
            // each chunk through the bounded channel. Cancellation and a chunk
            // gap over the idle ceiling surface as one final `Err` item, then
            // the stream ends; a dropped consumer stops the task and releases
            // the response. Every delivered chunk starts the idle wait over.
            tokio::spawn(async move {
                let mut response = response;
                loop {
                    let chunk = tokio::select! {
                        biased;
                        () = cancel_for_body.cancelled() => {
                            let _ = chunk_tx
                                .send(Err(ProviderError::new(ProviderErrorKind::Cancelled)))
                                .await;
                            return;
                        }
                        chunk = tokio::time::timeout(idle, response.chunk()) => chunk,
                    };
                    match chunk {
                        Ok(Ok(Some(bytes))) => {
                            if chunk_tx.send(Ok(bytes)).await.is_err() {
                                return;
                            }
                        }
                        Ok(Ok(None)) => return,
                        Ok(Err(error)) => {
                            let _ = chunk_tx.send(Err(map_reqwest_error(error))).await;
                            return;
                        }
                        Err(_elapsed) => {
                            let _ = chunk_tx
                                .send(Err(ProviderError::new(ProviderErrorKind::Timeout)))
                                .await;
                            return;
                        }
                    }
                }
            });
            Ok(chunk_rx)
        })
    }
}

/// Maximum bytes read from an error response body.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Reads at most [`MAX_ERROR_BODY_BYTES`] from a non-2xx response with `idle`
/// bounding each chunk wait.
async fn read_error_body(mut response: reqwest::Response, idle: Duration) -> Result<String, ()> {
    let mut buf = Vec::new();
    loop {
        match tokio::time::timeout(idle, response.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(buf.len());
                buf.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                if buf.len() >= MAX_ERROR_BODY_BYTES {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(_)) => break,
            Err(_elapsed) => return Err(()),
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
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

    use tokio_util::sync::CancellationToken;

    use super::{ReqwestTransport, SseTransport, TransportCall};

    /// Reads the request head. The reply starts right away: waiting for the
    /// two-byte body can stall on Nagle against a delayed ACK (up to 200ms on
    /// Windows), longer than the idle limits these tests use.
    fn read_request_head(stream: &mut std::net::TcpStream) -> bool {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => return false,
                Ok(count) => buf.extend_from_slice(&tmp[..count]),
            }
        }
        true
    }

    /// Ends a reply without a reset. Closing with the request body still
    /// unread in the receive buffer sends RST instead of FIN, and the client
    /// then loses the tail of the response ("transport failed"). Half-close,
    /// then drain until the client closes its side.
    fn finish_response(mut stream: std::net::TcpStream) {
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut sink = [0_u8; 1024];
        while matches!(stream.read(&mut sink), Ok(count) if count > 0) {}
    }

    fn spawn_drip_server(pieces: usize, gap: Duration) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            if !read_request_head(&mut stream) {
                return;
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
            finish_response(stream);
        });
        format!("http://{addr}/v1/chat/completions")
    }

    fn spawn_oversized_error_server(chunks: usize, chunk_bytes: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            if !read_request_head(&mut stream) {
                return;
            }
            let total = chunks * chunk_bytes;
            let header = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {total}\r\n\r\n"
            );
            if stream.write_all(header.as_bytes()).is_err() {
                return;
            }
            let _ = stream.flush();
            let piece = vec![b'x'; chunk_bytes];
            for _ in 0..chunks {
                if stream.write_all(&piece).is_err() {
                    return;
                }
                let _ = stream.flush();
            }
            // Hold open after the cap is crossed to prove the client stops
            // reading at MAX_ERROR_BODY_BYTES instead of waiting for EOF.
            std::thread::sleep(Duration::from_secs(10));
        });
        format!("http://{addr}/v1/chat/completions")
    }

    /// Sends error-status headers with a promised body it never delivers.
    fn spawn_stalled_error_server() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            if !read_request_head(&mut stream) {
                return;
            }
            let header = "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 256\r\n\r\n";
            if stream.write_all(header.as_bytes()).is_err() {
                return;
            }
            let _ = stream.flush();
            // Never send the promised body; hold the socket open.
            std::thread::sleep(Duration::from_secs(10));
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
            while let Some(item) = stream.recv().await {
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

    #[tokio::test]
    async fn a_stalled_error_body_keeps_its_status_kind() {
        let url = spawn_stalled_error_server();
        let transport =
            ReqwestTransport::with_read_timeout(Duration::from_millis(200)).expect("transport");
        let posted = transport.post(
            TransportCall {
                endpoint: url,
                headers: Vec::new(),
                body: b"{}".to_vec(),
            },
            CancellationToken::new(),
        );
        let error = tokio::time::timeout(Duration::from_secs(3), posted)
            .await
            .expect("post settled within the outer bound")
            .expect_err("stalled error body must fail");
        assert_eq!(error.kind(), mycode_core::ProviderErrorKind::Rejected);
        assert_eq!(error.status(), Some(400));
    }

    #[tokio::test]
    async fn an_oversized_error_body_stops_reading_at_the_cap() {
        let url = spawn_oversized_error_server(12, 8192);
        let transport =
            ReqwestTransport::with_read_timeout(Duration::from_secs(2)).expect("transport");
        let posted = transport.post(
            TransportCall {
                endpoint: url,
                headers: Vec::new(),
                body: b"{}".to_vec(),
            },
            CancellationToken::new(),
        );
        let error = tokio::time::timeout(Duration::from_secs(2), posted)
            .await
            .expect("error body settled at the cap without waiting for the 10s hold")
            .expect_err("400 must fail");
        assert_eq!(error.kind(), mycode_core::ProviderErrorKind::Rejected);
    }
}
