//! Length-framed JSON between `run_code` and its Python process.
//!
//! A frame is a 4-byte big-endian length followed by UTF-8 JSON. The control
//! socket is not stdout or stderr. Those pipes stay free for raw interpreter
//! output, which the host still captures.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Largest accepted frame. Larger lengths are a protocol failure.
pub(super) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Child → host.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub(super) enum ChildFrame {
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "call")]
    Call {
        id: u64,
        name: String,
        #[serde(default)]
        args: Value,
    },
    #[serde(rename = "log")]
    Log { text: String },
    #[serde(rename = "warn")]
    Warn { text: String },
    #[serde(rename = "done")]
    Done {
        #[serde(default)]
        value: Option<Value>,
        #[serde(default)]
        error: Option<DoneError>,
    },
}

/// Program failure reported by the child. Logs already sent stay with the host.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(super) struct DoneError {
    pub kind: String,
    pub message: String,
}

/// Host → child.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub(super) enum HostFrame {
    #[serde(rename = "boot")]
    Boot {
        code: String,
        tools: Vec<String>,
        restricted: bool,
        warn_process: bool,
    },
    #[serde(rename = "reply")]
    Reply {
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
    },
}

/// Encodes one JSON object as a length-prefixed frame.
pub(super) fn encode_frame(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let payload = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "control frame is {} bytes; the limit is {MAX_FRAME_BYTES}",
            payload.len()
        ));
    }
    let len =
        u32::try_from(payload.len()).map_err(|_| "control frame length overflow".to_owned())?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes one frame body that has already been length-checked.
pub(super) fn decode_child(payload: &[u8]) -> Result<ChildFrame, String> {
    serde_json::from_slice(payload).map_err(|error| format!("control frame is not JSON: {error}"))
}

/// Splits a buffer into `(frame_body, rest)` when a full frame is present.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(super) fn split_frame(buffer: &[u8]) -> Result<Option<(&[u8], &[u8])>, String> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(format!("control frame length {len} is not allowed"));
    }
    if buffer.len() < 4 + len {
        return Ok(None);
    }
    Ok(Some((&buffer[4..4 + len], &buffer[4 + len..])))
}

#[cfg(test)]
mod tests {
    use super::{ChildFrame, HostFrame, decode_child, encode_frame, split_frame};
    use serde_json::json;

    #[test]
    fn round_trip_preserves_a_call_and_a_reply() {
        let call = json!({"type": "call", "id": 1, "name": "read", "args": {"path": "a.rs"}});
        let bytes = encode_frame(&call).expect("encode");
        assert_eq!(
            &bytes[..4],
            &u32::try_from(bytes.len() - 4).unwrap().to_be_bytes()
        );
        let (body, rest) = split_frame(&bytes).expect("split").expect("full");
        assert!(rest.is_empty());
        let decoded = decode_child(body).expect("decode");
        assert_eq!(
            decoded,
            ChildFrame::Call {
                id: 1,
                name: "read".to_owned(),
                args: json!({"path": "a.rs"}),
            }
        );
        let reply = HostFrame::Reply {
            id: 1,
            ok: false,
            value: None,
            message: Some("missing".to_owned()),
            tool: Some("read".to_owned()),
        };
        let encoded = encode_frame(&reply).expect("reply");
        let (body, _) = split_frame(&encoded).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["type"], "reply");
        assert_eq!(value["ok"], false);
        assert_eq!(value["message"], "missing");
        assert!(value.get("value").is_none());
    }

    #[test]
    fn a_partial_frame_waits_and_a_zero_length_fails() {
        assert!(split_frame(&[0, 0, 0, 5, 1, 2]).unwrap().is_none());
        assert!(split_frame(&[0, 0, 0]).unwrap().is_none());
        assert!(split_frame(&[0, 0, 0, 0]).is_err());
    }

    #[test]
    fn done_keeps_an_error_beside_no_value() {
        let payload = br#"{"type":"done","error":{"kind":"exception","message":"line 2: boom"}}"#;
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(payload);
        let (body, rest) = split_frame(&frame).unwrap().unwrap();
        assert!(rest.is_empty());
        match decode_child(body).unwrap() {
            ChildFrame::Done { value, error } => {
                assert!(value.is_none());
                let error = error.expect("error");
                assert_eq!(error.kind, "exception");
                assert!(error.message.contains("line 2"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
