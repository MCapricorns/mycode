//! Implements the bounded, cancellation-aware provider event stream.

use std::fmt;
use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::task::Poll;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::{ProviderError, ProviderErrorKind, StreamEvent};

/// Number of events buffered between a provider and the agent.
///
/// Sixteen permits short bursts without allowing an unbounded producer to
/// consume memory. Producers asynchronously backpressure beyond this point.
pub const EVENT_STREAM_CAPACITY: usize = 16;

/// Maximum JSON-encoded size of one stream event.
///
/// One MiB bounds every individual Host-to-Agent handoff independently of the
/// channel capacity.
pub const MAX_EVENT_ENCODED_BYTES: usize = 1_024 * 1_024;

#[derive(Debug, Default)]
struct SenderState {
    terminal_claimed: bool,
}

/// Producer half of an [`EventStream`].
///
/// Clones share one asynchronous critical section covering validation,
/// terminal claim, and bounded send. Therefore concurrent sends preserve a
/// single terminal and no delta can be enqueued after it.
#[derive(Clone)]
pub struct EventStreamSender {
    tx: mpsc::Sender<StreamEvent>,
    state: Arc<Mutex<SenderState>>,
}

impl fmt::Debug for EventStreamSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventStreamSender")
            .field("closed", &self.tx.is_closed())
            .finish_non_exhaustive()
    }
}

impl EventStreamSender {
    /// Sends one event with bounded backpressure.
    ///
    /// Oversized or unencodable events are replaced by one protocol terminal.
    /// Returns `false` when another terminal already won or the receiver closed.
    ///
    /// This operation is cancellation-safe while waiting for capacity: dropping
    /// its future before reservation completes does not claim or lose a terminal.
    pub async fn send(&self, event: StreamEvent) -> bool {
        let mut state = self.state.lock().await;
        if state.terminal_claimed || self.tx.is_closed() {
            return false;
        }
        let event = match serde_json::to_vec(&event) {
            Ok(encoded) if encoded.len() <= MAX_EVENT_ENCODED_BYTES => event,
            Ok(_) => protocol_terminal("provider event exceeds the encoded size limit"),
            Err(_) => protocol_terminal("provider event could not be encoded"),
        };

        let Ok(permit) = self.tx.reserve().await else {
            return false;
        };
        if event.is_terminal() {
            state.terminal_claimed = true;
        }
        permit.send(event);
        true
    }

    /// Resolves when the receiving stream is closed or dropped.
    pub fn closed(&self) -> impl Future<Output = ()> + '_ {
        self.tx.closed()
    }
}

fn protocol_terminal(message: &'static str) -> StreamEvent {
    StreamEvent::Error(ProviderError::with_message(
        ProviderErrorKind::Protocol,
        message,
    ))
}

/// Consumer half of the provider event stream.
///
/// Queued events drain before cancellation is observed. Cancellation and an
/// all-senders-dropped condition each synthesize exactly one terminal error.
/// After any terminal, [`Self::next`] returns `None`. Dropping the receiver
/// cancels the request and closes the channel, waking blocked producers.
#[derive(Debug)]
pub struct EventStream {
    rx: mpsc::Receiver<StreamEvent>,
    cancel: CancellationToken,
    terminal_seen: bool,
}

impl EventStream {
    /// Creates a bounded stream tied to `cancel`.
    #[must_use]
    pub fn channel(cancel: CancellationToken) -> (EventStreamSender, Self) {
        let (tx, rx) = mpsc::channel(EVENT_STREAM_CAPACITY);
        (
            EventStreamSender {
                tx,
                state: Arc::new(Mutex::new(SenderState::default())),
            },
            Self {
                rx,
                cancel,
                terminal_seen: false,
            },
        )
    }

    /// Receives the next event.
    ///
    /// Queued events take precedence over cancellation. A closed producer side
    /// without a terminal becomes one protocol error rather than silent EOF.
    pub async fn next(&mut self) -> Option<StreamEvent> {
        if self.terminal_seen {
            return None;
        }

        match self.rx.try_recv() {
            Ok(event) => return self.accept(event),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                return self.synthesize(self.closed_terminal());
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
        }

        let received = {
            let receiver = &mut self.rx;
            let mut cancelled = std::pin::pin!(self.cancel.cancelled());
            poll_fn(|context| {
                if let Poll::Ready(event) = receiver.poll_recv(context) {
                    return Poll::Ready(Ok(event));
                }
                if cancelled.as_mut().poll(context).is_ready() {
                    return Poll::Ready(Err(()));
                }
                Poll::Pending
            })
            .await
        };
        match received {
            Ok(Some(event)) => self.accept(event),
            Ok(None) => {
                let terminal = self.closed_terminal();
                self.synthesize(terminal)
            }
            Err(()) => match self.rx.try_recv() {
                Ok(event) => self.accept(event),
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    self.synthesize(StreamEvent::Error(ProviderError::new(
                        ProviderErrorKind::Cancelled,
                    )))
                }
            },
        }
    }

    fn closed_terminal(&self) -> StreamEvent {
        if self.cancel.is_cancelled() {
            StreamEvent::Error(ProviderError::new(ProviderErrorKind::Cancelled))
        } else {
            protocol_terminal("provider producer ended without a terminal event")
        }
    }

    fn accept(&mut self, event: StreamEvent) -> Option<StreamEvent> {
        if event.is_terminal() {
            self.finish();
        }
        Some(event)
    }

    fn synthesize(&mut self, event: StreamEvent) -> Option<StreamEvent> {
        self.finish();
        Some(event)
    }

    fn finish(&mut self) {
        self.terminal_seen = true;
        self.rx.close();
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.rx.close();
    }
}
