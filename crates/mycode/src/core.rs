//! Shared conversation data and provider contracts.
//!
//! This module defines messages, events, attachments, tool specifications and
//! bounded provider streams. It has no application or UI responsibilities.

pub mod attachments;
pub mod error;
pub mod events;
pub mod ids;
pub mod message;
pub mod provider;
pub mod tool;

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
