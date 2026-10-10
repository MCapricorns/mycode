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

use std::sync::Arc;

use crate::{AssistantMessage, Message, SharedMessage, ToolSpec};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Maximum JSON-encoded size accepted for one provider request.
///
/// Eight MiB bounds the Agent-to-Host handoff while leaving room for normal
/// conversation history and tool schemas. The agent validates this limit after
/// hook transformation and before invoking a provider.
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
}

/// A provider-neutral completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Ordered system prompt parts.
    pub system_prompt: Vec<String>,
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
            system_prompt: Vec::new(),
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
        self.system_prompt.push(prompt.into());
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
        let encoded = serde_json::to_vec(self).map_err(|_| {
            ProviderError::with_message(
                ProviderErrorKind::Protocol,
                "provider request could not be encoded",
            )
        })?;
        if encoded.len() > MAX_REQUEST_ENCODED_BYTES {
            return Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "provider request exceeds the encoded size limit",
            ));
        }
        Ok(())
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
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Starts one completion stream.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`ProviderError`] when setup fails before a stream is
    /// available. Streaming failures are terminal [`StreamEvent::Error`] items.
    async fn stream(
        &self,
        request: &Request,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;
}
