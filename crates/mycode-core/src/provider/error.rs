//! Defines bounded, redacted provider errors.

use std::fmt;
use std::ops::Range;

use crate::MycodeError;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Maximum source characters retained in a provider error message.
const MAX_ERROR_MESSAGE_CHARS: usize = 512;
/// Maximum raw bytes scanned while constructing a provider error.
const MAX_ERROR_SCAN_BYTES: usize = 64 * 1_024;
const REDACTED: &str = "[REDACTED]";

/// Stable provider failure categories visible to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    /// The request was cancelled cooperatively.
    Cancelled,
    /// The provider is temporarily unavailable.
    Unavailable,
    /// The request exceeded its time budget.
    Timeout,
    /// The provider rejected the request.
    Rejected,
    /// The provider or adapter violated the stream contract.
    Protocol,
}

impl ProviderErrorKind {
    fn summary(self) -> &'static str {
        match self {
            Self::Cancelled => "provider request cancelled",
            Self::Unavailable => "provider unavailable",
            Self::Timeout => "provider request timed out",
            Self::Rejected => "provider request rejected",
            Self::Protocol => "provider protocol error",
        }
    }
}

/// A typed provider failure with an optional safe message.
///
/// Messages are redacted and bounded during construction and deserialization.
/// Raw input is never retained, so `Debug`, `Display`, serialization, cloning,
/// and conversion to `MycodeError` cannot expose the original credential text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ProviderError {
    kind: ProviderErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// The HTTP status the server answered with, when the failure has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
}

impl ProviderError {
    /// Creates an error without additional context.
    #[must_use]
    pub const fn new(kind: ProviderErrorKind) -> Self {
        Self {
            kind,
            message: None,
            status: None,
        }
    }

    /// Creates an error with immediately redacted, bounded context.
    #[must_use]
    pub fn with_message(kind: ProviderErrorKind, message: impl AsRef<str>) -> Self {
        let message = sanitize_message(message.as_ref());
        Self {
            kind,
            message: (!message.is_empty()).then_some(message),
            status: None,
        }
    }

    /// Attaches the HTTP status the failure came back with.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// The HTTP status that produced the failure, when there was one.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    /// Returns the already-sanitized optional context.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Returns whether this error represents cancellation.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.kind, ProviderErrorKind::Cancelled)
    }
}

impl<'de> Deserialize<'de> for ProviderError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Representation {
            kind: ProviderErrorKind,
            #[serde(default)]
            message: Option<String>,
            #[serde(default)]
            status: Option<u16>,
        }

        let representation = Representation::deserialize(deserializer)?;
        let mut error = match representation.message {
            Some(message) => Self::with_message(representation.kind, message),
            None => Self::new(representation.kind),
        };
        if let Some(status) = representation.status {
            error = error.with_status(status);
        }
        Ok(error)
    }
}

impl fmt::Debug for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.kind.summary())?;
        if let Some(message) = &self.message {
            write!(formatter, ": {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

impl From<ProviderError> for MycodeError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error.to_string())
    }
}

fn sanitize_message(raw: &str) -> String {
    let redacted = redact_sensitive(&raw[..raw.floor_char_boundary(MAX_ERROR_SCAN_BYTES)]);
    if redacted.chars().count() <= MAX_ERROR_MESSAGE_CHARS {
        redacted
    } else {
        let truncated: String = redacted.chars().take(MAX_ERROR_MESSAGE_CHARS).collect();
        format!("{truncated}… [truncated]")
    }
}

fn redact_sensitive(message: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(message) {
        redact_json(&mut value);
        return value.to_string();
    }
    redact_plain_text(message)
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED.into());
                } else {
                    redact_json(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_json(value);
            }
        }
        Value::String(text) => *text = redact_plain_text(text),
        _ => {}
    }
}

/// Lowercases a key and strips the `-` / `_` separators so header and field
/// spellings (`X-Api-Key`, `api_key`, `apiKey`) compare equal.
fn normalized_key(key: &str) -> String {
    key.to_ascii_lowercase().replace(['-', '_'], "")
}

/// Classifies credential-like header and field names without provider policy.
fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    normalized.ends_with("authorization")
        || normalized.ends_with("apikey")
        || normalized.ends_with("authkey")
        || normalized.ends_with("accesskey")
        || normalized.ends_with("subscriptionkey")
        || normalized.ends_with("token")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.contains("password")
        || matches!(normalized.as_str(), "cookie" | "setcookie")
}

/// Redacts credential assignments and common standalone token forms.
///
/// Confirmed quoted values are scanned once and then skipped, keeping total
/// work linear even for malformed input containing many escaped quotes.
fn redact_plain_text(message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut copied_until = 0;
    let mut cursor = 0;
    while cursor < message.len() {
        let redaction = bare_sensitive_assignment_value(message, cursor)
            .or_else(|| quoted_sensitive_assignment_value(message, cursor))
            .or_else(|| standalone_bearer_value(message, cursor))
            .or_else(|| prefixed_api_key(message, cursor));
        if let Some(redaction) = redaction {
            output.push_str(&message[copied_until..redaction.start]);
            output.push_str(REDACTED);
            cursor = redaction.end;
            copied_until = redaction.end;
        } else {
            cursor = next_char_boundary(message, cursor);
        }
    }
    output.push_str(&message[copied_until..]);
    output
}

fn bare_sensitive_assignment_value(message: &str, start: usize) -> Option<Range<usize>> {
    let bytes = message.as_bytes();
    if !bytes.get(start).is_some_and(|byte| is_key_byte(*byte))
        || (start > 0 && is_key_byte(bytes[start - 1]))
    {
        return None;
    }
    let mut key_end = start + 1;
    while bytes.get(key_end).is_some_and(|byte| is_key_byte(*byte)) {
        key_end += 1;
    }
    let key = &message[start..key_end];
    is_sensitive_key(key).then(|| assignment_value(message, key_end, key))?
}

fn quoted_sensitive_assignment_value(message: &str, closing: usize) -> Option<Range<usize>> {
    let bytes = message.as_bytes();
    let quote = match bytes.get(closing).copied()? {
        quote @ (b'"' | b'\'') => quote,
        _ => return None,
    };
    let mut key_start = closing;
    while key_start > 0 && is_key_byte(bytes[key_start - 1]) {
        key_start -= 1;
    }
    let opening = key_start.checked_sub(1)?;
    if bytes[opening] != quote {
        return None;
    }
    let key = &message[key_start..closing];
    is_sensitive_key(key).then(|| assignment_value(message, closing + 1, key))?
}

fn assignment_value(message: &str, after_key: usize, key: &str) -> Option<Range<usize>> {
    let mut cursor = skip_ascii_whitespace(message, after_key);
    if !matches!(message.as_bytes().get(cursor), Some(b'=' | b':')) {
        return None;
    }
    cursor = skip_ascii_whitespace(message, cursor + 1);
    match message.as_bytes().get(cursor).copied() {
        Some(quote @ (b'"' | b'\'')) => {
            let start = cursor + 1;
            let end = find_closing_quote(message, cursor, quote).unwrap_or(message.len());
            Some(start..end)
        }
        _ => {
            let end = if is_cookie_key(key) || is_authorization_key(key) {
                line_value_end(message, cursor)
            } else {
                field_value_end(message, cursor)
            };
            Some(cursor..end)
        }
    }
}

fn find_closing_quote(message: &str, open: usize, quote: u8) -> Option<usize> {
    let mut cursor = open + 1;
    let mut escaped = false;
    while cursor < message.len() {
        let byte = message.as_bytes()[cursor];
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

fn bearer_value_end(message: &str, start: usize) -> Option<usize> {
    const BEARER: &str = "Bearer";
    let scheme_end = start.checked_add(BEARER.len())?;
    if !message.get(start..scheme_end)?.eq_ignore_ascii_case(BEARER)
        || !message
            .as_bytes()
            .get(scheme_end)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }
    let credential_start = skip_ascii_whitespace(message, scheme_end);
    match message.as_bytes().get(credential_start).copied() {
        Some(quote @ (b'"' | b'\'')) => Some(
            find_closing_quote(message, credential_start, quote)
                .map_or(message.len(), |end| end + 1),
        ),
        Some(_) => {
            let end = credential_token_end(message, credential_start);
            (end > credential_start).then_some(end)
        }
        None => None,
    }
}

fn standalone_bearer_value(message: &str, start: usize) -> Option<Range<usize>> {
    if start > 0 && is_word_byte(message.as_bytes()[start - 1]) {
        return None;
    }
    bearer_value_end(message, start).map(|end| start..end)
}

fn prefixed_api_key(message: &str, start: usize) -> Option<Range<usize>> {
    if start > 0 && is_word_byte(message.as_bytes()[start - 1]) {
        return None;
    }
    let end = start.checked_add(3)?;
    let prefix = message.get(start..end)?;
    if !prefix.eq_ignore_ascii_case("sk-") && !prefix.eq_ignore_ascii_case("sk_") {
        return None;
    }
    let token_end = credential_token_end(message, start);
    (token_end > end).then_some(start..token_end)
}

fn credential_token_end(message: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < message.len() {
        if is_value_delimiter(message.as_bytes()[cursor]) {
            break;
        }
        cursor = next_char_boundary(message, cursor);
    }
    cursor
}

fn field_value_end(message: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < message.len() {
        let byte = message.as_bytes()[cursor];
        if matches!(byte, b'\r' | b'\n')
            || (byte.is_ascii_control() && !byte.is_ascii_whitespace())
            || matches!(
                byte,
                b'&' | b',' | b';' | b'"' | b'\'' | b'}' | b']' | b'|' | b'#'
            )
        {
            break;
        }
        if byte.is_ascii_whitespace() {
            let next = skip_ascii_whitespace(message, cursor);
            if starts_assignment(message, next) {
                break;
            }
            cursor = next;
        } else {
            cursor = next_char_boundary(message, cursor);
        }
    }
    cursor
}

fn starts_assignment(message: &str, start: usize) -> bool {
    let bytes = message.as_bytes();
    let Some(first) = bytes.get(start).copied() else {
        return false;
    };
    let mut cursor = match first {
        quote @ (b'"' | b'\'') => {
            let mut closing = start + 1;
            while bytes.get(closing).is_some_and(|byte| is_key_byte(*byte)) {
                closing += 1;
            }
            if bytes.get(closing) != Some(&quote) {
                return false;
            }
            closing + 1
        }
        byte if is_key_byte(byte) => {
            let mut end = start + 1;
            while bytes.get(end).is_some_and(|byte| is_key_byte(*byte)) {
                end += 1;
            }
            end
        }
        _ => return false,
    };
    cursor = skip_ascii_whitespace(message, cursor);
    matches!(bytes.get(cursor), Some(b'=' | b':'))
}

fn line_value_end(message: &str, start: usize) -> usize {
    let mut cursor = start;
    while cursor < message.len() {
        let byte = message.as_bytes()[cursor];
        if byte.is_ascii_control() || matches!(byte, b'&' | b'}' | b']' | b'|' | b'#') {
            break;
        }
        cursor = next_char_boundary(message, cursor);
    }
    cursor
}

fn is_cookie_key(key: &str) -> bool {
    matches!(normalized_key(key).as_str(), "cookie" | "setcookie")
}

fn is_authorization_key(key: &str) -> bool {
    normalized_key(key).ends_with("authorization")
}

const fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

const fn is_value_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(
            byte,
            b'&' | b';' | b',' | b'"' | b'\'' | b'}' | b']' | b'|' | b'#'
        )
}

fn skip_ascii_whitespace(message: &str, mut cursor: usize) -> usize {
    while message
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    cursor
}

fn next_char_boundary(message: &str, cursor: usize) -> usize {
    cursor
        + message[cursor..]
            .chars()
            .next()
            .expect("cursor is before the string end")
            .len_utf8()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitized(raw: &str) -> String {
        ProviderError::with_message(ProviderErrorKind::Protocol, raw)
            .message()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn credential_assignments_and_tokens_are_redacted() {
        assert_eq!(sanitized("password=hunter2"), "password=[REDACTED]");
        assert_eq!(
            sanitized("Authorization: Bearer abc.def.ghi"),
            "Authorization: [REDACTED]"
        );
        assert_eq!(sanitized("Bearer sk-abc123 tail"), "[REDACTED] tail");
        assert_eq!(sanitized("sk-abc123 tail"), "[REDACTED] tail");
        assert_eq!(
            sanitized("{\"api_key\": \"v\"}"),
            "{\"api_key\":\"[REDACTED]\"}"
        );
        assert_eq!(sanitized("x-api-key: keyvalue"), "x-api-key: [REDACTED]");
        // Ordinary prose words containing `sk-` / `sk_` or `bearer` without a
        // token are not credentials and must not be clobbered.
        assert_eq!(
            sanitized("task-123 and mask_value failed; missing bearer\n"),
            "task-123 and mask_value failed; missing bearer\n"
        );
    }

    #[test]
    fn multibyte_text_never_panics_the_scanner() {
        // Multibyte chars adjacent to assignments, quotes, and colons.
        let cases = [
            "错误 password=secret123 done",
            "password=密码密码密码",
            "éé\"api_key\": \"value\"",
            "sk-ключ-123",
            "Bearer токен",
            "\"authorization\": «Bearer abc»",
            "api_key=中文\r\nnext line",
            "token:ççç",
            "password=\"_unclosed 中文",
            "日本語sk-abc def",
            "cookie=a=b&中文=1",
        ];
        for case in cases {
            let _ = sanitized(case);
        }
    }

    #[test]
    fn messages_are_truncated_and_scan_windows_bounded() {
        let long = format!("{}{}", "a".repeat(600), " password=zzz");
        let out = sanitized(&long);
        assert!(out.ends_with("… [truncated]"), "{out}");
        let huge = format!("{}{}", "b".repeat(100_000), " password=zzz");
        let _ = sanitized(&huge);
    }

    #[test]
    fn deterministic_fuzz_never_panics_the_scanner() {
        // Pseudo-random mix of credential fragments, ASCII punctuation, and
        // multibyte characters; only UTF-8-valid strings are produced.
        let fragments = [
            "password=",
            "sk-",
            "sk_",
            "Bearer ",
            "bearer\t",
            "token:",
            "\"",
            "'",
            "\\",
            "}",
            "]",
            ",",
            ";",
            "&",
            "#",
            "|",
            " ",
            "\t",
            "\n",
            "\r",
            "a",
            "Z",
            "0",
            "_",
            "-",
            ".",
            "é",
            "汉",
            "🦀",
            "мир",
            "key",
            "api_key",
            "authorization",
            "cookie",
            "=",
            ":",
        ];
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 24) as usize;
            let text: String = (0..len)
                .map(|_| fragments[(next() as usize) % fragments.len()])
                .collect();
            let _ = sanitized(&text);
        }
    }
}
