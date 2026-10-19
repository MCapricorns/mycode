//! The provider port: what the agent loop asks of a wire adapter.
//!
//! [`Provider`] is the single seam between [`crate`] and any model backend.
//! The request shape, the bounded [`EventStream`] a response arrives on, and
//! the [`ProviderError`] taxonomy are contracts; no wire protocol, transport,
//! credential handling, or model-selection policy lives here.

mod error;
mod stream;

#[doc(inline)]
pub use error::{ProviderError, ProviderErrorKind};
#[doc(inline)]
pub use stream::{EVENT_STREAM_CAPACITY, EventStream, EventStreamSender, MAX_EVENT_ENCODED_BYTES};

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::{AssistantMessage, Message, SharedMessage, ToolSpec};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Maximum JSON-encoded size accepted for one provider request.
///
/// Eight MiB bounds one encoded provider request while leaving room for
/// normal conversation history and tool schemas. The agent validates this
/// limit after hook transformation and before invoking a provider.
pub const MAX_REQUEST_ENCODED_BYTES: usize = 8 * 1_024 * 1_024;

/// Requested reasoning effort for models that support it.
///
/// The spellings match models.dev `reasoning_options` effort values plus
/// `on` / `off` for catalog toggle rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    /// Disable reasoning when the vendor accepts an off switch.
    Off,
    /// Shortest advertised effort.
    Minimal,
    /// Brief reasoning; fastest discrete rung.
    #[default]
    Low,
    /// Balanced reasoning.
    Medium,
    /// Deep reasoning.
    High,
    /// Above high, when the catalog advertises `xhigh`.
    Xhigh,
    /// Vendor maximum effort.
    Max,
    /// Enable reasoning on toggle-only models.
    On,
}

impl ReasoningLevel {
    /// Parses a catalog or settings spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" | "none" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            "on" => Some(Self::On),
            _ => None,
        }
    }

    /// `reasoning_effort` wire token; `None` for toggle-on (no effort field).
    #[must_use]
    pub const fn effort_token(self) -> Option<&'static str> {
        match self {
            Self::Off => Some("none"),
            Self::On => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }

    /// Returns whether `token` is a syntactically valid effort token (`1..=32`
    /// ASCII alphanumeric, `-`, or `_` bytes).
    #[must_use]
    pub fn is_valid_token(token: &str) -> bool {
        (1..=32).contains(&token.len())
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }
}

/// Failure modes for [`check_encoded_json_bound`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonBoundError {
    /// Serialization failed before reaching the byte bound.
    Encode,
    /// Serialized output exceeded the caller-supplied byte limit.
    Oversized,
}

struct BoundedJsonCounter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedJsonCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.bytes.checked_add(buf.len()) {
            Some(next) if next <= self.limit => {
                self.bytes = next;
                Ok(buf.len())
            }
            _ => {
                self.exceeded = true;
                Err(std::io::Error::other("encoded JSON exceeds byte limit"))
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Counts the exact UTF-8 byte length of `value` when serialized as JSON
/// without allocating a buffer, stopping immediately if `limit` is exceeded.
///
/// # Errors
///
/// Returns [`JsonBoundError::Oversized`] when the encoded length exceeds
/// `limit`, or [`JsonBoundError::Encode`] when serialization fails.
pub fn check_encoded_json_bound<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<usize, JsonBoundError> {
    let mut counter = BoundedJsonCounter {
        bytes: 0,
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(counter.bytes),
        Err(_) if counter.exceeded => Err(JsonBoundError::Oversized),
        Err(_) => Err(JsonBoundError::Encode),
    }
}

/// Counts the exact UTF-8 byte length of `value` when serialized as JSON
/// without allocating a buffer.
#[must_use]
pub fn encoded_json_len<T: Serialize + ?Sized>(value: &T) -> Option<usize> {
    check_encoded_json_bound(value, usize::MAX).ok()
}

/// A provider-neutral completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Ordered system prompt parts. Shared so each LLM round does not copy
    /// the session prompt.
    pub system_prompt: Arc<[String]>,
    /// Conversation history. Entries are shared with the agent so a request
    /// round does not deep-copy tool output.
    pub messages: Vec<SharedMessage>,
    /// Tools available for the response. Shared so each LLM round does not
    /// deep-copy every tool schema.
    pub tools: Arc<[ToolSpec]>,
    /// Reasoning effort; `None` leaves the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
    /// Output token cap from models.dev `limit.output`. `None` when the
    /// catalog does not publish one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Effort token published by models.dev that is not one of the built-in
    /// levels. Sent as-is; no budget is invented for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_token: Option<String>,
    /// Stable session key for providers that accept `prompt_cache_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            system_prompt: Arc::from([]),
            messages: Vec::new(),
            tools: Arc::from([]),
            reasoning: None,
            max_output_tokens: None,
            reasoning_token: None,
            prompt_cache_key: None,
        }
    }
}

impl Request {
    /// Creates an empty request.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one system prompt part.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        // Builders run once per part, so the copy stays off the request path.
        self.system_prompt = self
            .system_prompt
            .iter()
            .cloned()
            .chain([prompt.into()])
            .collect();
        self
    }

    /// Appends one conversation message.
    #[must_use]
    pub fn with_message(mut self, message: Message) -> Self {
        self.messages.push(Arc::new(message));
        self
    }

    /// Appends one tool specification.
    #[must_use]
    pub fn with_tool(mut self, tool: ToolSpec) -> Self {
        let mut tools = Vec::with_capacity(self.tools.len() + 1);
        tools.extend(self.tools.iter().cloned());
        tools.push(tool);
        self.tools = Arc::from(tools);
        self
    }

    /// Sets the requested reasoning effort.
    #[must_use]
    pub fn with_reasoning(mut self, level: ReasoningLevel) -> Self {
        self.reasoning = Some(level);
        self
    }

    /// Validates the encoded request size.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when JSON encoding fails, or a rejected error
    /// when the encoded request exceeds [`MAX_REQUEST_ENCODED_BYTES`].
    pub fn validate(&self) -> Result<(), ProviderError> {
        match check_encoded_json_bound(self, MAX_REQUEST_ENCODED_BYTES) {
            Ok(_) => Ok(()),
            Err(JsonBoundError::Oversized) => Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "provider request exceeds the encoded size limit",
            )),
            Err(JsonBoundError::Encode) => Err(ProviderError::with_message(
                ProviderErrorKind::Protocol,
                "provider request could not be encoded",
            )),
        }
    }
}

/// One event emitted while an assistant message streams.
///
/// A stream ends with exactly one [`Self::Done`] or [`Self::Error`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StreamEvent {
    /// Incremental user-visible text.
    TextDelta(String),
    /// Incremental model reasoning text.
    ThinkingDelta(String),
    /// Incremental tool-call arguments.
    ToolCallDelta {
        /// Opaque tool-call identifier.
        id: String,
        /// A JSON fragment that need not be valid by itself.
        partial_json: String,
    },
    /// The complete assistant message.
    Done {
        /// Fully assembled message, including complete tool calls.
        message: AssistantMessage,
    },
    /// The terminal provider failure.
    Error(ProviderError),
}

impl StreamEvent {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Error(_))
    }
}

/// Streams provider-neutral completions for the agent.
///
/// Adapters live outside this crate. Tests may inject their own
/// implementations. Producers must honor `cancel`, stop when
/// [`EventStreamSender::send`] returns `false`, and release upstream
/// resources.
///
/// The method returns a boxed future so the trait stays object-safe for
/// `dyn Provider` dispatch; implementers write `Box::pin(async move { … })`.
pub trait Provider: Send + Sync {
    /// Starts one completion stream.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`ProviderError`] when setup fails before a stream is
    /// available. Streaming failures are terminal [`StreamEvent::Error`] items.
    fn stream<'a>(
        &'a self,
        request: &'a Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send + 'a>>;
}
