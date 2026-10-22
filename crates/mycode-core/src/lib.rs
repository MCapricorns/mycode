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
//! types. See `wiki/index.html`.

pub mod attachments;
pub mod error;
pub mod events;
pub mod ids;
pub mod message;
pub mod provider;
pub mod tool;

/// Global allocator for every binary that links this leaf crate.
///
/// Declared once here — the only crate no workspace member conflicts with —
/// so the desktop binary and every test target share mimalloc's page-aligned
/// segments and per-thread free lists instead of the default allocator's
/// retained RSS. A second `#[global_allocator]` anywhere in the graph is a
/// compile error, which is the intended guard.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub use attachments::{
    AttachError, Attachments, IMAGE_MEDIA_TYPES, ImageAttachment, MAX_ATTACHED_FILES,
    MAX_ATTACHED_IMAGES, MAX_FILE_BYTES, MAX_FILES_BYTES, MAX_IMAGE_BASE64_BYTES, MAX_IMAGE_BYTES,
    MAX_MESSAGE_IMAGE_BASE64_BYTES, StoredUserMessage, TextAttachment, image_media_type,
};
pub use error::MycodeError;
pub use events::{AgentEvent, MessageDelta, TurnOutcome};
pub use ids::CallId;
pub use message::{
    AssistantMessage, BinaryData, ContentBlock, INTERRUPTED_RESPONSE_PREFIX, Message,
    SharedMessage, StopReason, TextBlock, ThinkingBlock, ToolCall, ToolResultMessage,
    USER_INTERRUPT_DETAIL, Usage, UserMessage, interrupted_response_text, pair_tool_results,
    strip_user_interruption, tool_label, tool_target,
};
#[doc(inline)]
pub use provider::{
    EventStream, EventStreamSender, JsonBoundError, MAX_REQUEST_ENCODED_BYTES, Provider,
    ProviderError, ProviderErrorKind, ReasoningLevel, Request, StreamEvent,
    check_encoded_json_bound, encoded_json_len,
};
pub use tool::{ToolSpec, inline_schema_refs};

/// Returns the wall-clock duration since the Unix epoch, saturating at zero
/// when the system clock predates the epoch.
#[must_use]
pub fn unix_now() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}
