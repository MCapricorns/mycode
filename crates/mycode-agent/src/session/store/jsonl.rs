//! One JSON object per line in a branch log.
//!
//! A line carries the event id, kind, optional call id, timestamp, payload
//! digest, and either an inline base64 payload or a relative payload file
//! name. The digest is over the raw payload bytes, matching the reservation
//! the actor already checked.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use super::super::digest::{format_digest, payload_digest};
use super::super::dto::EventKind;
use super::super::ids::{SessionCallId, SessionEventId};
use super::EXTERNAL_PAYLOAD_THRESHOLD;

/// One encoded JSONL record, including its trailing newline.
pub(crate) struct EncodedLine {
    /// Canonical line bytes, newline included.
    pub(crate) line: Vec<u8>,
    /// Whether the payload was written beside the log.
    pub(crate) external: bool,
}

/// One decoded JSONL record.
pub(crate) struct DecodedLine {
    /// Event identity spelling.
    pub(crate) event_id: String,
    /// Event classification.
    pub(crate) kind: EventKind,
    /// Call identity spelling, present only for tool kinds.
    pub(crate) call_id: Option<String>,
    /// Raw payload bytes, digest-checked.
    pub(crate) payload: Vec<u8>,
    /// Canonical digest spelling.
    pub(crate) digest: String,
    /// Line length including the newline.
    pub(crate) record_len: u64,
    /// Payload byte length declared by the line.
    pub(crate) bytes: u64,
    /// Payload bytes live in `payloads/<id>.bin` rather than on the line.
    pub(crate) external: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineFile<'a> {
    id: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<&'a str>,
    ts: u64,
    digest: &'a str,
    bytes: u64,
    /// Plain-text payload when the bytes are valid UTF-8 (the common case:
    /// messages, tool calls, and results are all JSON or text). Base64
    /// `payload` below is only the binary fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_file: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LineOwned {
    id: String,
    kind: String,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    ts: u64,
    digest: String,
    bytes: u64,
    #[serde(default)]
    payload_text: Option<String>,
    #[serde(default)]
    payload: Option<String>,
    #[serde(default)]
    payload_file: Option<String>,
}

/// Bytes one committed record is expected to charge against the session cap.
///
/// The figure is an upper bound used at reservation time, before the line
/// exists. Inline payloads expand under base64; external payloads are charged
/// as the file plus a short line.
pub(crate) fn encoded_record_len(has_call_id: bool, payload: &[u8]) -> Option<u64> {
    let call = if has_call_id { 48 } else { 0 };
    let body = if payload.len() >= EXTERNAL_PAYLOAD_THRESHOLD {
        256usize.checked_add(payload.len())?
    } else if let Ok(text) = std::str::from_utf8(payload) {
        // The stored JSON string is escaped; size it exactly so the session
        // cap reservation matches the bytes that will be written.
        256usize.checked_add(escaped_json_string_len(text))?
    } else {
        let expanded = payload.len().saturating_add(2) / 3 * 4;
        256usize.checked_add(expanded)?
    };
    u64::try_from(body.checked_add(call)?).ok()
}

/// The length of `text` as a serialized JSON string, quotes included.
///
/// serde_json escapes `"`, `\`, the `\b \t \n \f \r` shorthands, and the
/// remaining control bytes below 0x20 as `\u00XX`; everything else —
/// non-ASCII included — is written as-is.
fn escaped_json_string_len(text: &str) -> usize {
    let mut len = 2; // the surrounding quotes
    for byte in text.as_bytes() {
        len += match byte {
            b'"' | b'\\' | 0x08 | 0x09 | 0x0A | 0x0C | 0x0D => 2,
            0x00..=0x1F => 6,
            _ => 1,
        };
    }
    len
}

/// Charge of one committed line against the session byte cap.
pub(crate) fn charge(record_len: u64, external: bool, payload_len: u64) -> Option<u64> {
    if external {
        record_len.checked_add(payload_len)
    } else {
        Some(record_len)
    }
}

/// Sidebar title for a user message payload. Assistant JSON and display-only
/// compaction summaries are skipped.
pub(crate) fn message_title(kind: EventKind, payload: &[u8]) -> Option<String> {
    if kind != EventKind::Message {
        return None;
    }
    if serde_json::from_slice::<mycode_core::AssistantMessage>(payload).is_ok() {
        return None;
    }
    let text = String::from_utf8_lossy(payload);
    // The summary card starts with `COMPACTION SUMMARY` (the app's
    // `display_summary_text`); older ledgers used the XML-style marker.
    let start = text.trim_start();
    if start.starts_with("COMPACTION SUMMARY\n")
        || start.starts_with("<compaction_summary display=\"true\">")
    {
        return None;
    }
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = collapsed.chars().take(60).collect();
    if title.is_empty() { None } else { Some(title) }
}

/// Encodes one event as a single JSONL line.
pub(crate) fn encode_line(
    event_id: &SessionEventId,
    kind: EventKind,
    call_id: Option<&SessionCallId>,
    payload: &[u8],
    digest: &str,
    ts: u64,
) -> Result<EncodedLine, ()> {
    let external = payload.len() >= EXTERNAL_PAYLOAD_THRESHOLD;
    let payload_file = external.then(|| format!("payloads/{}.bin", event_id.as_str()));
    let text = (!external)
        .then(|| std::str::from_utf8(payload).ok())
        .flatten();
    let inline = (!external && text.is_none()).then(|| STANDARD.encode(payload));
    let line = LineFile {
        id: event_id.as_str(),
        kind: kind_label(kind),
        call_id: call_id.map(SessionCallId::as_str),
        ts,
        digest,
        bytes: u64::try_from(payload.len()).map_err(|_| ())?,
        payload_text: text,
        payload: inline,
        payload_file: payload_file.as_deref(),
    };
    let mut bytes = serde_json::to_vec(&line).map_err(|_| ())?;
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(());
    }
    bytes.push(b'\n');
    Ok(EncodedLine {
        line: bytes,
        external,
    })
}

/// Decodes one JSONL line, including its trailing newline, and checks the digest.
pub(crate) fn decode_line(bytes: &[u8]) -> Result<DecodedLine, ()> {
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(());
    };
    if body.contains(&b'\n') || body.contains(&b'\r') {
        return Err(());
    }
    let line: LineOwned = serde_json::from_slice(body).map_err(|_| ())?;
    let kind = kind_from_label(&line.kind).ok_or(())?;
    if SessionEventId::parse(&line.id).is_none() {
        return Err(());
    }
    if let Some(call) = &line.call_id
        && SessionCallId::parse(call).is_none()
    {
        return Err(());
    }
    let tool = matches!(kind, EventKind::ToolCall | EventKind::ToolResult);
    if tool != line.call_id.is_some() {
        return Err(());
    }
    let _ = line.ts;
    let payload = match (&line.payload_text, &line.payload, &line.payload_file) {
        (Some(text), None, None) => text.clone().into_bytes(),
        (None, Some(inline), None) => STANDARD.decode(inline).map_err(|_| ())?,
        (None, None, Some(_)) => Vec::new(),
        _ => return Err(()),
    };
    let external = line.payload_file.is_some();
    if !external {
        if payload.len() as u64 != line.bytes || payload.len() >= EXTERNAL_PAYLOAD_THRESHOLD {
            return Err(());
        }
        let digest = format_digest(&payload_digest(&payload));
        if digest != line.digest {
            return Err(());
        }
        return Ok(DecodedLine {
            event_id: line.id,
            kind,
            call_id: line.call_id,
            payload,
            digest,
            record_len: u64::try_from(bytes.len()).map_err(|_| ())?,
            bytes: line.bytes,
            external: false,
        });
    }
    let expected = format!("payloads/{}.bin", line.id);
    if line.payload_file.as_deref() != Some(expected.as_str()) {
        return Err(());
    }
    if !super::super::digest::is_canonical_digest(&line.digest)
        || line.bytes < EXTERNAL_PAYLOAD_THRESHOLD as u64
        || line.bytes > kind.payload_bound() as u64
        || usize::try_from(line.bytes).is_err()
    {
        return Err(());
    }
    Ok(DecodedLine {
        event_id: line.id,
        kind,
        call_id: line.call_id,
        payload,
        digest: line.digest,
        record_len: u64::try_from(bytes.len()).map_err(|_| ())?,
        bytes: line.bytes,
        external: true,
    })
}

pub(crate) fn kind_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Message => "message",
        EventKind::ToolCall => "tool-call",
        EventKind::ToolResult => "tool-result",
        EventKind::Usage => "usage",
        EventKind::Task => "task",
    }
}

pub(crate) fn kind_from_label(label: &str) -> Option<EventKind> {
    match label {
        "message" => Some(EventKind::Message),
        "tool-call" => Some(EventKind::ToolCall),
        "tool-result" => Some(EventKind::ToolResult),
        "usage" => Some(EventKind::Usage),
        "task" => Some(EventKind::Task),
        _ => None,
    }
}
