//! Incremental server-sent-events frame parser.
//!
//! Feeds raw response bytes and yields complete `data:` payloads. The parser
//! tolerates CRLF and LF endings, ignores comments and non-data fields, and
//! fails closed on oversized frames instead of buffering without bound.

use mycode_core::{ProviderError, ProviderErrorKind};

/// Maximum accepted size of one assembled data payload.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Assembles SSE data payloads from response chunks.
#[derive(Debug, Default)]
pub struct FrameParser {
    buffer: Vec<u8>,
}

impl FrameParser {
    /// Creates an empty parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk and returns every completed data payload.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when a single frame exceeds
    /// [`MAX_FRAME_BYTES`].
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, ProviderError> {
        let mut frames = Vec::new();
        for &byte in chunk {
            if self.buffer.len() >= MAX_FRAME_BYTES {
                self.buffer.clear();
                return Err(ProviderError::with_message(
                    ProviderErrorKind::Protocol,
                    "SSE frame exceeds the size limit",
                ));
            }
            self.buffer.push(byte);
            if byte == b'\n' {
                let line = std::mem::take(&mut self.buffer);
                if let Some(data) = self.take_data_line(&line) {
                    if data.len() > MAX_FRAME_BYTES {
                        return Err(ProviderError::with_message(
                            ProviderErrorKind::Protocol,
                            "SSE frame exceeds the size limit",
                        ));
                    }
                    frames.push(data);
                }
            }
        }
        Ok(frames)
    }

    /// Flushes a trailing unterminated line at end of stream.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when the leftover line is an oversized data
    /// payload.
    pub fn finish(&mut self) -> Result<Option<String>, ProviderError> {
        let line = std::mem::take(&mut self.buffer);
        match self.take_data_line(&line) {
            Some(data) if data.len() > MAX_FRAME_BYTES => Err(ProviderError::with_message(
                ProviderErrorKind::Protocol,
                "SSE frame exceeds the size limit",
            )),
            Some(data) => Ok(Some(data)),
            None => Ok(None),
        }
    }

    /// Joins multi-line data payloads per the SSE spec once a blank line is
    /// seen. This parser handles single-line `data:` records, which every
    /// supported provider emits; continuation lines are appended with `\n`.
    fn take_data_line(&self, line: &[u8]) -> Option<String> {
        let mut trimmed = line;
        if trimmed.last() == Some(&b'\n') {
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        if trimmed.last() == Some(&b'\r') {
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        if trimmed.is_empty() {
            return None;
        }
        let stripped = trimmed.strip_prefix(b"data:")?;
        let payload = if stripped.first() == Some(&b' ') {
            &stripped[1..]
        } else {
            stripped
        };
        Some(String::from_utf8_lossy(payload).into_owned())
    }
}
