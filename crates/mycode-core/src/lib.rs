//! `mycode-core` — the shared vocabulary every other crate speaks.
//!
//! Two layers live here, and nothing else:
//!
//! - **Conversation data**: messages, content blocks, usage, events, ids, and
//!   tool specs. Plain values with serde support, so they flow through agent
//!   event broadcasts and provider payloads unchanged.
//! - **The provider port** ([`provider`]): the request, error, and bounded
//!   stream contracts between the agent loop and a wire adapter. Contracts
//!   only — no wire protocol, transport, or selection policy.
//!
//! This is a leaf crate: it has no dependency on any other MYCode crate, which
//! is what lets the agent, the tools, and the desktop all agree on one set of
//! types. See `docs/core.md`.

pub mod error;
pub mod events;
pub mod ids;
pub mod message;
pub mod provider;
pub mod tool;

pub use error::MycodeError;
pub use events::{AgentEvent, MessageDelta, TurnOutcome};
pub use ids::CallId;
pub use message::{
    AssistantMessage, BinaryData, ContentBlock, CustomMessage, Message, SharedMessage, StopReason,
    TextBlock, ThinkingBlock, ToolCall, ToolResultMessage, Usage, UserMessage,
    interrupted_response_text, tool_label, tool_target,
};
#[doc(inline)]
pub use provider::{
    EventStream, EventStreamSender, MAX_REQUEST_ENCODED_BYTES, Provider, ProviderError,
    ProviderErrorKind, ReasoningLevel, Request, StreamEvent,
};
pub use tool::{ToolSpec, inline_schema_refs};
