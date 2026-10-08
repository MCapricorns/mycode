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
) {
    let body = match transport.post(call, cancel.clone()).await {
        Ok(body) => body,
        Err(error) => {
            let _ = sender.send(StreamEvent::Error(error)).await;
            return;
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
                let _ = sender.send(reducer.interrupt(&error.to_string())).await;
                return;
            }
            None => break,
        };
        let frames = match parser.feed(&chunk) {
            Ok(frames) => frames,
            Err(error) => {
                let _ = sender.send(reducer.interrupt(&error.to_string())).await;
                return;
            }
        };
        for frame in frames {
            let terminal = send_all(&sender, reducer.feed(&frame)).await;
            if terminal {
                return;
            }
        }
    }

    match parser.finish() {
        Ok(Some(trailing)) => {
            if send_all(&sender, reducer.feed(&trailing)).await {
                return;
            }
        }
        Ok(None) => {}
        Err(error) => {
            let _ = sender.send(reducer.interrupt(&error.to_string())).await;
            return;
        }
    }
    let _ = sender.send(reducer.finish()).await;
}

/// Sends events in order; returns `true` when a terminal was sent.
async fn send_all(sender: &EventStreamSender, events: Vec<StreamEvent>) -> bool {
    for event in events {
        let terminal = matches!(event, StreamEvent::Done { .. } | StreamEvent::Error(_));
        if !sender.send(event).await {
            return true;
        }
        if terminal {
            return true;
        }
    }
    false
}

/// Converts a parser failure into a terminal error event.
pub(crate) fn protocol_error(message: &'static str) -> StreamEvent {
    StreamEvent::Error(ProviderError::with_message(
        mycode_core::ProviderErrorKind::Protocol,
        message,
    ))
}
