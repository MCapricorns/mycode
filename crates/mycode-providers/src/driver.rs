//! Shared streaming driver: transport bytes to bounded event stream.
//!
//! One spawned task owns the whole request lifetime. It feeds SSE frames to
//! a protocol-specific [`FrameReducer`], forwards every produced event through
//! the bounded sender, and stops at the first terminal, cancellation, or
//! transport failure. The consumer synthesizes the cancellation terminal.

use std::sync::Arc;

use futures_util::StreamExt as _;

use mycode_core::{EventStreamSender, ProviderError, StreamEvent};

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

/// Drives one provider request to exactly one terminal.
pub(crate) async fn drive(
    transport: Arc<dyn crate::transport::SseTransport>,
    call: TransportCall,
    reducer: Box<dyn FrameReducer + Send>,
    sender: EventStreamSender,
    cancel: tokio_util::sync::CancellationToken,
    provider_id: String,
    model: String,
) {
    let prepared = crate::cache::body_has_prompt_cache_key(&call.body).then(|| {
        (
            call.endpoint.clone(),
            call.headers.clone(),
            call.body.clone(),
        )
    });
    let body = match transport.post(call, cancel.clone()).await {
        Ok(body) => body,
        Err(error) => {
            let Some((endpoint, headers, original)) = prepared else {
                let _ = sender.send(StreamEvent::Error(error)).await;
                return;
            };
            let Some(stripped) =
                crate::cache::retry_body_without_prompt_cache_key(&endpoint, &error, &original)
            else {
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
                        endpoint,
                        headers,
                        body: stripped,
                    },
                    cancel.clone(),
                )
                .await
            {
                Ok(body) => body,
                Err(error) => {
                    let _ = sender.send(StreamEvent::Error(error)).await;
                    return;
                }
            }
        }
    };

    let mut parser = FrameParser::new();
    let mut reducer = reducer;
    let mut body = body;
    loop {
        let chunk = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            chunk = body.next() => chunk,
        };
        let chunk = match chunk {
            Some(Ok(chunk)) => chunk,
            Some(Err(error)) => {
                let _ = emit(
                    &sender,
                    &provider_id,
                    &model,
                    reducer.interrupt(&error.to_string()),
                )
                .await;
                return;
            }
            None => break,
        };
        let frames = match parser.feed(&chunk) {
            Ok(frames) => frames,
            Err(error) => {
                let _ = emit(
                    &sender,
                    &provider_id,
                    &model,
                    reducer.interrupt(&error.to_string()),
                )
                .await;
                return;
            }
        };
        for frame in frames {
            let terminal = send_all(&sender, &provider_id, &model, reducer.feed(&frame)).await;
            if terminal {
                return;
            }
        }
    }

    match parser.finish() {
        Ok(Some(trailing)) => {
            if send_all(&sender, &provider_id, &model, reducer.feed(&trailing)).await {
                return;
            }
        }
        Ok(None) => {}
        Err(error) => {
            let _ = emit(
                &sender,
                &provider_id,
                &model,
                reducer.interrupt(&error.to_string()),
            )
            .await;
            return;
        }
    }
    let _ = emit(&sender, &provider_id, &model, reducer.finish()).await;
}

/// Sends events in order; returns `true` when a terminal was sent.
async fn send_all(
    sender: &EventStreamSender,
    provider_id: &str,
    model: &str,
    events: Vec<StreamEvent>,
) -> bool {
    for event in events {
        if emit(sender, provider_id, model, event).await {
            return true;
        }
    }
    false
}

/// Logs usage for a completed response, then forwards the event.
///
/// Returns `true` when the consumer is gone or the event is terminal.
async fn emit(
    sender: &EventStreamSender,
    provider_id: &str,
    model: &str,
    event: StreamEvent,
) -> bool {
    crate::cache::log_done_usage(provider_id, model, &event);
    let terminal = matches!(event, StreamEvent::Done { .. } | StreamEvent::Error(_));
    if !sender.send(event).await {
        return true;
    }
    terminal
}

/// Converts a parser failure into a terminal error event.
pub(crate) fn protocol_error(message: &'static str) -> StreamEvent {
    StreamEvent::Error(ProviderError::with_message(
        mycode_core::ProviderErrorKind::Protocol,
        message,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use mycode_core::{ProviderError, ProviderErrorKind, StreamEvent};
    use tokio_util::sync::CancellationToken;

    use super::drive;
    use crate::openai_completions::CompletionsReducer;
    use crate::transport::{ByteStream, SseTransport, TransportCall};

    struct Scripted {
        bodies: Mutex<Vec<Vec<u8>>>,
        steps: Mutex<Vec<Result<&'static [u8], &'static str>>>,
    }

    #[async_trait::async_trait]
    impl SseTransport for Scripted {
        async fn post(
            &self,
            call: TransportCall,
            _cancel: CancellationToken,
        ) -> Result<ByteStream, ProviderError> {
            self.bodies.lock().expect("bodies").push(call.body);
            let step = self.steps.lock().expect("steps").remove(0);
            match step {
                Ok(bytes) => {
                    let stream = futures_util::stream::iter(vec![Ok(Bytes::from(bytes.to_vec()))]);
                    Ok(Box::pin(stream))
                }
                Err(message) => Err(ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    message,
                )),
            }
        }
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
        let transport = Arc::new(Scripted {
            bodies: Mutex::new(Vec::new()),
            steps: Mutex::new(vec![
                Err("HTTP 400: unknown parameter prompt_cache_key"),
                Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"),
            ]),
        });
        let cancel = CancellationToken::new();
        let (sender, mut stream) = mycode_core::EventStream::channel(cancel.clone());
        drive(
            transport.clone(),
            call(endpoint),
            Box::new(CompletionsReducer::new()),
            sender,
            cancel,
            "probe".to_owned(),
            "x".to_owned(),
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
    async fn an_unnamed_400_is_not_retried() {
        let endpoint = "https://noretry.example.test/v1/chat/completions";
        let transport = Arc::new(Scripted {
            bodies: Mutex::new(Vec::new()),
            steps: Mutex::new(vec![Err("HTTP 400: invalid request")]),
        });
        let cancel = CancellationToken::new();
        let (sender, mut stream) = mycode_core::EventStream::channel(cancel.clone());
        drive(
            transport.clone(),
            call(endpoint),
            Box::new(CompletionsReducer::new()),
            sender,
            cancel,
            "probe".to_owned(),
            "x".to_owned(),
        )
        .await;
        assert_eq!(transport.bodies.lock().expect("bodies").len(), 1);
        let event = stream.next().await.expect("terminal");
        assert!(matches!(event, StreamEvent::Error(_)));
        assert!(crate::cache::wants_prompt_cache_key(endpoint));
    }
}
