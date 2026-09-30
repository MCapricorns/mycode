//! `ToolStream` — the per-call progress/terminal channel tools write into
//! while executing. See `docs/tools.md`.
//!
//! Stream invariant: any number of [`ToolStreamItem::Progress`] items
//! followed by **exactly one** [`ToolStreamItem::Terminal`]. The producer
//! side enforces "at most one terminal" atomically across clones: the
//! check→claim→send sequence is one critical section, so two clones racing
//! `terminal()` cannot both deliver and a `progress()` that passed the check
//! can never land after a `Terminal`. Once a terminal item has been sent,
//! every further item is *silently ignored* (returns `false`). This follows
//! the general single-terminal stream principle, so a tool that already
//! finished can never corrupt the stream.
//!
//! Builtin tools return their final result from `Tool::execute`; the
//! dispatcher sends that result as the terminal item unless the tool already
//! sent one. The internal tool channel is currently unbounded. `ToolStream`
//! remains the tool-dispatch stream and is not the provider event API.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::tool::ToolResult;

/// Incremental progress update from a running tool, rendered live by the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProgress {
    pub message: String,
}

impl ToolProgress {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// One item on a tool's output stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolStreamItem {
    /// Incremental progress (zero or more, before the terminal).
    Progress(ToolProgress),
    /// The final result of the call (exactly one; ends the stream).
    Terminal(ToolResult),
}

/// Producer half of a [`ToolStream`]. Cloneable and shareable between
/// tasks; enforces the single-terminal invariant.
#[derive(Clone)]
pub struct ToolStream {
    tx: UnboundedSender<ToolStreamItem>,
    /// `true` once a [`ToolStreamItem::Terminal`] has been sent. Guarded
    /// by a mutex (not a bare atomic) so check→claim→send is one
    /// critical section shared by every clone: exactly one `terminal()`
    /// wins, and no in-flight `progress()` can be enqueued after the
    /// terminal — an atomic claim alone cannot order channel sends.
    state: Arc<Mutex<bool>>,
}

impl ToolStream {
    /// Create a channel pair: producer + consumer.
    pub fn channel() -> (Self, ToolStreamReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                state: Arc::new(Mutex::new(false)),
            },
            ToolStreamReceiver { rx },
        )
    }

    /// A stream nobody listens on — progress and terminal items are
    /// dropped. Useful for fire-and-forget dispatch and tests.
    pub fn closed() -> Self {
        let (stream, rx) = Self::channel();
        drop(rx);
        stream
    }

    /// Send a progress item. Returns `false` (and drops the item) once
    /// the stream has terminated or the receiver is gone.
    pub fn progress(&self, message: impl Into<String>) -> bool {
        self.send(ToolStreamItem::Progress(ToolProgress::new(message)))
    }

    /// Send the terminal result. Only the **first** terminal wins; later
    /// calls are ignored and return `false`.
    pub fn terminal(&self, result: ToolResult) -> bool {
        self.send(ToolStreamItem::Terminal(result))
    }

    /// Whether the stream has terminated (or has no receiver): further
    /// sends would be dropped.
    pub fn is_done(&self) -> bool {
        *self.state.lock().expect("tool stream state lock poisoned") || self.tx.is_closed()
    }

    fn send(&self, item: ToolStreamItem) -> bool {
        // One critical section across all clones: the check, the
        // terminal claim, and the channel send are indivisible, so a
        // racing terminal() can't double-deliver and a progress() that
        // passed the check can't be enqueued after a Terminal.
        let mut terminated = self.state.lock().expect("tool stream state lock poisoned");
        if *terminated {
            return false;
        }
        if matches!(item, ToolStreamItem::Terminal(_)) {
            *terminated = true;
        }
        self.tx.send(item).is_ok()
    }
}

/// Consumer half of a [`ToolStream`].
#[derive(Debug)]
pub struct ToolStreamReceiver {
    rx: UnboundedReceiver<ToolStreamItem>,
}

impl ToolStreamReceiver {
    /// Await the next item; `None` once all senders are dropped.
    pub async fn recv(&mut self) -> Option<ToolStreamItem> {
        self.rx.recv().await
    }

    /// Collect items until the terminal item arrives (or the producer is
    /// dropped without one — an early-exiting tool must not hang its
    /// consumer). The terminal item, when present, is included.
    pub async fn drain(&mut self) -> Vec<ToolStreamItem> {
        let mut items = Vec::new();
        while let Some(item) = self.recv().await {
            let is_terminal = matches!(item, ToolStreamItem::Terminal(_));
            items.push(item);
            if is_terminal {
                break;
            }
        }
        items
    }
}
