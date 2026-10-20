//! The user's own messages kept word for word after a summary.
//!
//! They ride inside the summary text, so a checkpoint's message indexes and
//! the end-of-turn tail check see one summary message as before.

use std::sync::Arc;

use mycode_core::Message;

use super::sizing::{TokenScale, text_tokens};
use super::{blocks_text, elide, is_summary_message};

/// Most characters of user messages kept after a summary, so summary and
/// messages together stay inside [`mycode_config::MAX_SUMMARY_CHARS`].
const MAX_ECHO_CHARS: usize = 22_000;
/// Least room worth keeping part of a user message that does not fit whole.
const MIN_ECHO_PART_TOKENS: usize = 200;
/// Characters a cut message adds: its marker and the omission note.
const ELISION_CHARS: usize = 64;
/// Opens the user's messages kept after a summary.
const ECHO_HEADER: &str = "\n\n<user_messages>\nWhat the user wrote in the part \
    of the session this checkpoint replaces, word for word, oldest first. The \
    summary above tells which of it is done.";
/// Starts each message in that block.
const ECHO_ITEM: &str = "\n\n<message>\n";
const ECHO_FOOTER: &str = "\n</user_messages>";

/// A summary followed by the user's messages kept with it.
pub(super) fn with_echo(summary: &str, echo: &[String]) -> String {
    if echo.is_empty() {
        return summary.to_owned();
    }
    let mut text = format!("{summary}{ECHO_HEADER}");
    for message in echo {
        text.push_str(ECHO_ITEM);
        text.push_str(message);
    }
    text.push_str(ECHO_FOOTER);
    text
}

/// Splits stored summary text into the model's summary and the user
/// messages [`with_echo`] kept after it.
///
/// A user message that itself holds the item marker comes back as two;
/// both are still the user's words.
pub(super) fn split_echo(text: &str) -> (&str, Vec<&str>) {
    let Some(start) = text.find(ECHO_HEADER) else {
        return (text, Vec::new());
    };
    let Some(body) = text[start + ECHO_HEADER.len()..].strip_suffix(ECHO_FOOTER) else {
        return (text, Vec::new());
    };
    let messages = body
        .strip_prefix(ECHO_ITEM)
        .map(|body| body.split(ECHO_ITEM).collect())
        .unwrap_or_default();
    (&text[..start], messages)
}

/// The user messages to keep after a new summary, oldest first: the newest
/// of the earlier checkpoint's and the newly summarized head's that fit
/// `budget` provider tokens. The newest one that does not fit whole keeps
/// its start and end when there is room for a part of it.
pub(super) fn user_echo(
    earlier: &[&str],
    head: &[Arc<Message>],
    budget: usize,
    scale: TokenScale,
) -> Vec<String> {
    let fresh = head
        .iter()
        .filter(|message| !is_summary_message(message))
        .filter_map(|message| match message.as_ref() {
            Message::User(user) => Some(blocks_text(&user.content)),
            _ => None,
        });
    let all: Vec<String> = earlier
        .iter()
        .map(|text| (*text).to_owned())
        .chain(fresh)
        .filter(|text| !text.trim().is_empty())
        .collect();
    let mut kept = Vec::new();
    let mut tokens_left = budget;
    let mut chars_left = MAX_ECHO_CHARS;
    // Each message also costs its marker, so many short ones cannot push
    // the stored text past its cap.
    let marker_tokens = text_tokens(ECHO_ITEM);
    for text in all.into_iter().rev() {
        let tokens = scale.apply(text_tokens(&text) + marker_tokens);
        let chars = text.chars().count() + ECHO_ITEM.len();
        if tokens <= tokens_left && chars <= chars_left {
            tokens_left -= tokens;
            chars_left -= chars;
            kept.push(text);
            continue;
        }
        if tokens_left >= MIN_ECHO_PART_TOKENS {
            // Room for the marker and the omission note.
            let room = chars_left.saturating_sub(ELISION_CHARS);
            kept.push(elide(&text, (chars * tokens_left / tokens).min(room)));
        }
        break;
    }
    kept.reverse();
    kept
}
