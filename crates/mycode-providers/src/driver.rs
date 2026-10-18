//! Shared streaming driver: transport bytes to bounded event stream.
//!
//! One spawned task owns the whole request lifetime. It feeds SSE frames to
//! a protocol-specific [`FrameReducer`], forwards every produced event through
//! the bounded sender, and stops at the first terminal, cancellation, or
//! transport failure. The consumer synthesizes the cancellation terminal.

use std::sync::Arc;
use std::time::Duration;

use mycode_core::{EventStreamSender, ProviderError, ProviderErrorKind, StreamEvent};

use crate::sse::FrameParser;
use crate::transport::TransportCall;

/// Protocol-specific state machine over SSE data payloads.
pub(crate) trait FrameReducer: Send + 'static {
    /// Consumes one data payload and produces zero or more events.
    ///
    /// Implementations may return a terminal event to end the stream.
    fn feed(&mut self, data: &str) -> Vec<StreamEvent>;

    /// Produces the terminal event for a stream that ended without one.
    fn finish(&mut self) -> StreamEvent;

    /// Ends the stream after a transport or protocol failure.
    ///
    /// Keeps thinking, text, and named tool calls already reduced, and marks
    /// the message with [`mycode_core::StopReason::Error`].
    fn interrupt(&mut self, detail: &str) -> StreamEvent;
}

/// Same-body POSTs per request, counting the first try.
const MAX_ATTEMPTS: usize = 4;

/// Backoff for a dropped connection, connect timeout, or generic 5xx.
#[cfg(not(test))]
const TRANSIENT_DELAYS_MS: [u64; 3] = [400, 1_200, 3_500];
#[cfg(test)]
const TRANSIENT_DELAYS_MS: [u64; 3] = [5, 5, 5];
/// Rate limits need a longer pause than a network blip.
#[cfg(not(test))]
const RATE_LIMIT_DELAYS_MS: [u64; 3] = [1_000, 4_000, 10_000];
#[cfg(test)]
const RATE_LIMIT_DELAYS_MS: [u64; 3] = [5, 5, 5];

/// The pause before retrying one failed `post` with the same body, or `None`
/// when the failure is final.
///
/// Network interruptions, connect timeouts, 408, 429, and the usual
/// transient 5xx answers are worth a bounded retry. A 404 (wrong route or
/// model) and the Cloudflare origin faults (520, 524, and family) describe
/// failures a retry cannot fix: the turn must stop and report them.
/// Mid-stream failures never reach this — by then partial output is already
/// reduced, so replaying the request would duplicate it.
fn retry_delay_for(error: &ProviderError, attempt: usize) -> Option<Duration> {
    if error.is_cancelled() {
        return None;
    }
    let delays = match error.status() {
        Some(429) => &RATE_LIMIT_DELAYS_MS,
        Some(404) | Some(520..=526) => return None,
        Some(408) | Some(500..=503) | Some(507) | Some(529) => &TRANSIENT_DELAYS_MS,
        Some(_) => return None,
        None => match error.kind() {
            ProviderErrorKind::Unavailable | ProviderErrorKind::Timeout => &TRANSIENT_DELAYS_MS,
            _ => return None,
        },
    };
    delays.get(attempt).copied().map(Duration::from_millis)
}

/// Drives one provider request to exactly one terminal.
///
/// `retryable` says the body carries a field one of the retry rules can
/// strip (a prompt cache key or replayed thinking); only then is a copy kept.
/// `make_reducer` rebuilds the reducer for each attempt, so a retry starts
/// from a clean protocol state.
pub(crate) async fn drive(
    transport: Arc<dyn crate::transport::SseTransport>,
    call: TransportCall,
    retryable: bool,
    mut make_reducer: impl FnMut() -> Box<dyn FrameReducer + Send>,
    sender: EventStreamSender,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut attempts = 0;
    'attempt: loop {
        let body = loop {
            match transport.post(call.clone(), cancel.clone()).await {
                Ok(body) => break body,
                Err(error) => {
                    if let Some(delay) = retry_delay_for(&error, attempts)
                        && attempts + 1 < MAX_ATTEMPTS
                    {
                        if back_off(&sender, &cancel, delay).await {
                            return;
                        }
                        attempts += 1;
                        continue;
                    }
                    // Transient retries are spent; the body-rewrite rules
                    // still get their one shot at a final rejection they
                    // recognize.
                    let stripped = retryable
                        .then(|| {
                            crate::cache::retry_body_without_prompt_cache_key(
                                &call.endpoint,
                                &error,
                                &call.body,
                            )
                            .or_else(|| {
                                crate::anthropic_messages::retry_body_without_thinking(
                                    &error, &call.body,
                                )
                            })
                        })
                        .flatten();
                    let Some(stripped) = stripped else {
                        let _ = sender.send(StreamEvent::Error(error)).await;
                        return;
                    };
                    if cancel.is_cancelled() {
                        let _ = sender.send(StreamEvent::Error(error)).await;
                        return;
                    }
                    match transport
                        .post(
                            TransportCall {
                                endpoint: call.endpoint.clone(),
                                headers: call.headers.clone(),
                                body: stripped,
                            },
                            cancel.clone(),
                        )
                        .await
                    {
                        Ok(body) => break body,
                        Err(error) => {
                            let _ = sender.send(StreamEvent::Error(error)).await;
                            return;
                        }
                    }
                }
            }
        };

        let mut parser = FrameParser::new();
        let mut reducer = make_reducer();
        let mut body = body;
        // Nothing user-visible yet: an early stream drop is as retryable as
        // a refused POST. Once one event is out, replaying could duplicate
        // committed output, so later failures are terminal.
        let mut emitted_any = false;
        loop {
            let chunk = tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                chunk = body.recv() => chunk,
            };
            let chunk = match chunk {
                Some(Ok(chunk)) => chunk,
                Some(Err(error)) => {
                    if !emitted_any
                        && attempts + 1 < MAX_ATTEMPTS
                        && let Some(delay) = retry_delay_for(&error, attempts)
                    {
                        if back_off(&sender, &cancel, delay).await {
                            return;
                        }
                        attempts += 1;
                        continue 'attempt;
                    }
                    let _ = emit(&sender, reducer.interrupt(&error.to_string())).await;
                    return;
                }
                None => break,
            };
            let frames = match parser.feed(&chunk) {
                Ok(frames) => frames,
                Err(error) => {
                    let _ = emit(&sender, reducer.interrupt(&error.to_string())).await;
                    return;
                }
            };
            for frame in frames {
                let events = reducer.feed(&frame);
                // An error frame ahead of any output (Anthropic's mid-stream
                // `overloaded_error`) is the same failure as a refused POST.
                if !emitted_any
                    && attempts + 1 < MAX_ATTEMPTS
                    && let [StreamEvent::Error(error)] = events.as_slice()
                    && let Some(delay) = retry_delay_for(error, attempts)
                {
                    if back_off(&sender, &cancel, delay).await {
                        return;
                    }
                    attempts += 1;
                    continue 'attempt;
                }
                emitted_any |= !events.is_empty();
                let terminal = send_all(&sender, events).await;
                if terminal {
                    return;
                }
            }
        }

        match parser.finish() {
            Ok(Some(trailing)) => {
                let events = reducer.feed(&trailing);
                if send_all(&sender, events).await {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                let _ = emit(&sender, reducer.interrupt(&error.to_string())).await;
                return;
            }
        }
        let _ = emit(&sender, reducer.finish()).await;
        return;
    }
}

/// Sleeps for `delay` before a retry. Returns `true` when cancellation won
/// the race; the cancellation terminal has then been sent.
async fn back_off(
    sender: &EventStreamSender,
    cancel: &tokio_util::sync::CancellationToken,
    delay: Duration,
) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => {
            let _ = sender
                .send(StreamEvent::Error(ProviderError::new(ProviderErrorKind::Cancelled)))
                .await;
            true
        }
        () = tokio::time::sleep(delay) => false,
    }
}

/// Sends events in order.
///
/// Returns `true` when the driver should stop: a terminal event was accepted,
/// or the sender rejected the event.
async fn send_all(sender: &EventStreamSender, events: Vec<StreamEvent>) -> bool {
    for event in events {
        if emit(sender, event).await {
            return true;
        }
    }
    false
}

/// Forwards one event.
///
/// Returns `true` when the consumer is gone or the event is terminal.
async fn emit(sender: &EventStreamSender, event: StreamEvent) -> bool {
    let terminal = matches!(event, StreamEvent::Done { .. } | StreamEvent::Error(_));
    if !sender.send(event).await {
        return true;
    }
    terminal
}

/// Builds a terminal protocol-error event.
pub(crate) fn protocol_error(message: &'static str) -> StreamEvent {
    StreamEvent::Error(ProviderError::with_message(
        mycode_core::ProviderErrorKind::Protocol,
        message,
    ))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use mycode_core::{ProviderError, ProviderErrorKind, StreamEvent};
    use tokio_util::sync::CancellationToken;

    use super::drive;
    use crate::openai_completions::CompletionsReducer;
    use crate::transport::{ByteStream, SseTransport, TransportCall};

    struct Scripted {
        bodies: Mutex<Vec<Vec<u8>>>,
        steps: Mutex<Vec<Result<ByteStream, ProviderError>>>,
    }

    impl SseTransport for Scripted {
        fn post<'a>(
            &'a self,
            call: TransportCall,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<ByteStream, ProviderError>> + Send + 'a>> {
            Box::pin(async move {
                self.bodies.lock().expect("bodies").push(call.body);
                self.steps.lock().expect("steps").remove(0)
            })
        }
    }

    fn scripted(steps: Vec<Result<&'static [u8], ProviderError>>) -> Arc<Scripted> {
        let steps: Vec<Result<ByteStream, ProviderError>> = steps
            .into_iter()
            .map(|step| {
                step.map(|bytes| {
                    let (sender, stream) = tokio::sync::mpsc::channel(1);
                    sender
                        .try_send(Ok(Bytes::from(bytes.to_vec())))
                        .expect("channel accepts one scripted chunk");
                    let _ = sender;
                    stream
                })
            })
            .collect();
        Arc::new(Scripted {
            bodies: Mutex::new(Vec::new()),
            steps: Mutex::new(steps),
        })
    }

    fn call(endpoint: &str) -> TransportCall {
        TransportCall {
            endpoint: endpoint.to_owned(),
            headers: Vec::new(),
            body: br#"{"model":"x","prompt_cache_key":"session-1"}"#.to_vec(),
        }
    }

    #[tokio::test]
    async fn a_named_prompt_cache_key_400_is_retried_once() {
        let endpoint = "https://retry.example.test/v1/chat/completions";
        let transport = scripted(vec![
            Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "HTTP 400: unknown parameter prompt_cache_key",
            )),
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"),
        ]);
        let cancel = CancellationToken::new();
        let (sender, mut stream) = mycode_core::EventStream::channel(cancel.clone());
        drive(
            transport.clone(),
            call(endpoint),
            true,
            || Box::new(CompletionsReducer::new()),
            sender,
            cancel,
        )
        .await;
        let bodies = transport.bodies.lock().expect("bodies").clone();
        assert_eq!(bodies.len(), 2);
        assert!(
            bodies[0]
                .windows(16)
                .any(|window| window == b"prompt_cache_key")
        );
        assert!(
            !bodies[1]
                .windows(16)
                .any(|window| window == b"prompt_cache_key")
        );
        let mut saw_done = false;
        while let Some(event) = stream.next().await {
            if matches!(event, StreamEvent::Done { .. }) {
                saw_done = true;
            }
        }
        assert!(saw_done);
        assert!(!crate::cache::wants_prompt_cache_key(endpoint));
    }

    #[tokio::test]
    async fn a_rejected_thinking_signature_is_retried_without_thinking() {
        let transport = scripted(vec![
            Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "HTTP 400: Invalid `signature` in `thinking` block",
            )),
            Ok(b"data: {\"type\":\"message_stop\"}\n\n"),
        ]);
        let cancel = CancellationToken::new();
        let (sender, _stream) = mycode_core::EventStream::channel(cancel.clone());
        let body = serde_json::json!({
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "s"},
                {"type": "text", "text": "hi"}
            ]}]
        });
        drive(
            transport.clone(),
            TransportCall {
                endpoint: "https://api.anthropic.com/v1/messages".to_owned(),
                headers: Vec::new(),
                body: serde_json::to_vec(&body).unwrap(),
            },
            crate::anthropic_messages::replays_thinking(&body),
            || Box::new(crate::anthropic_messages::MessagesReducer::new()),
            sender,
            cancel,
        )
        .await;
        let bodies = transport.bodies.lock().expect("bodies").clone();
        assert_eq!(bodies.len(), 2);
        let retried = String::from_utf8(bodies[1].clone()).unwrap();
        assert!(!retried.contains("thinking"), "{retried}");
        assert!(retried.contains("\"hi\""), "{retried}");
    }

    #[tokio::test]
    async fn an_unnamed_400_is_not_retried() {
        let endpoint = "https://noretry.example.test/v1/chat/completions";
        let transport = scripted(vec![Err(ProviderError::with_message(
            ProviderErrorKind::Rejected,
            "HTTP 400: invalid request",
        ))]);
        let cancel = CancellationToken::new();
        let (sender, mut stream) = mycode_core::EventStream::channel(cancel.clone());
        drive(
            transport.clone(),
            call(endpoint),
            true,
            || Box::new(CompletionsReducer::new()),
            sender,
            cancel,
        )
        .await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 1);
        let event = stream.next().await.expect("terminal");
        assert!(matches!(event, StreamEvent::Error(_)));
        assert!(crate::cache::wants_prompt_cache_key(endpoint));
    }

    const OK_CHUNK: &[u8] = b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

    async fn driven(transport: &Arc<Scripted>) -> Vec<StreamEvent> {
        let cancel = CancellationToken::new();
        let (sender, mut stream) = mycode_core::EventStream::channel(cancel.clone());
        drive(
            transport.clone(),
            call("https://retryable.example.test/v1/chat/completions"),
            false,
            || Box::new(CompletionsReducer::new()),
            sender,
            cancel,
        )
        .await;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_network_drop_is_retried_until_the_stream_opens() {
        let transport = scripted(vec![
            Err(ProviderError::new(ProviderErrorKind::Unavailable)),
            Err(ProviderError::with_message(
                ProviderErrorKind::Timeout,
                "timed out",
            )),
            Ok(OK_CHUNK),
        ]);
        let events = driven(&transport).await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 3);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::Done { .. }))
        );
    }

    #[tokio::test]
    async fn a_429_is_retried_then_succeeds() {
        let transport = scripted(vec![
            Err(ProviderError::new(ProviderErrorKind::Unavailable).with_status(429)),
            Ok(OK_CHUNK),
        ]);
        let events = driven(&transport).await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 2);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::Done { .. }))
        );
    }

    #[tokio::test]
    async fn transient_failures_give_up_after_the_attempt_cap() {
        let transport = scripted(vec![
            Err(ProviderError::new(ProviderErrorKind::Unavailable)),
            Err(ProviderError::new(ProviderErrorKind::Unavailable)),
            Err(ProviderError::new(ProviderErrorKind::Unavailable)),
            Err(ProviderError::new(ProviderErrorKind::Unavailable)),
            Ok(OK_CHUNK), // beyond the cap: never sent
        ]);
        let events = driven(&transport).await;
        assert_eq!(
            transport.bodies.lock().expect("bodies").len(),
            super::MAX_ATTEMPTS
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, StreamEvent::Error(_)))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, StreamEvent::Done { .. }))
        );
    }

    #[tokio::test]
    async fn a_520_origin_fault_stops_without_retry() {
        let transport = scripted(vec![
            Err(ProviderError::with_message(
                ProviderErrorKind::Unavailable,
                "HTTP 520: web server returned an unknown error",
            )
            .with_status(520)),
            Ok(OK_CHUNK),
        ]);
        let events = driven(&transport).await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 1);
        let terminal = events.last().expect("terminal event");
        match terminal {
            StreamEvent::Error(error) => {
                assert_eq!(error.status(), Some(520));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_404_stops_without_retry() {
        let transport = scripted(vec![
            Err(ProviderError::new(ProviderErrorKind::Rejected).with_status(404)),
            Ok(OK_CHUNK),
        ]);
        let events = driven(&transport).await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 1);
        assert!(matches!(events.last(), Some(StreamEvent::Error(_))));
    }

    #[tokio::test]
    async fn an_overloaded_frame_before_output_is_retried() {
        let transport = scripted(vec![
            Ok(b"event: error
data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}

"),
            Ok(b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}

data: {\"type\":\"message_stop\"}

"),
        ]);
        let cancel = CancellationToken::new();
        let (sender, mut stream) = mycode_core::EventStream::channel(cancel.clone());
        drive(
            transport.clone(),
            call("https://api.anthropic.com/v1/messages"),
            false,
            || Box::new(crate::anthropic_messages::MessagesReducer::new()),
            sender,
            cancel,
        )
        .await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 2);
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        let Some(StreamEvent::Done { message }) = events.last() else {
            panic!("expected done, got {events:?}");
        };
        assert_eq!(message.text(), "ok");
    }

    #[test]
    fn retry_classification() {
        let unavailable = ProviderError::new(ProviderErrorKind::Unavailable);
        let timeout = ProviderError::new(ProviderErrorKind::Timeout);
        let rejected = ProviderError::new(ProviderErrorKind::Rejected);
        let rate_limited = ProviderError::new(ProviderErrorKind::Unavailable).with_status(429);
        let gone = ProviderError::new(ProviderErrorKind::Rejected).with_status(404);
        let origin_down = ProviderError::new(ProviderErrorKind::Unavailable).with_status(524);
        let server = ProviderError::new(ProviderErrorKind::Unavailable).with_status(503);
        let cancelled = ProviderError::new(ProviderErrorKind::Cancelled);

        assert!(super::retry_delay_for(&unavailable, 0).is_some());
        assert!(super::retry_delay_for(&timeout, 0).is_some());
        assert!(super::retry_delay_for(&rate_limited, 0).is_some());
        assert!(super::retry_delay_for(&server, 0).is_some());
        assert!(super::retry_delay_for(&gone, 0).is_none());
        assert!(super::retry_delay_for(&origin_down, 0).is_none());
        assert!(super::retry_delay_for(&rejected, 0).is_none());
        assert!(super::retry_delay_for(&cancelled, 0).is_none());
        assert!(super::retry_delay_for(&unavailable, 2).is_some());
        assert!(super::retry_delay_for(&unavailable, 3).is_none());
    }
}
