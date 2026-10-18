//! Incremental server-sent-events frame parser.
//!
//! Feeds raw response bytes and yields complete `data:` payloads. The parser
//! tolerates CRLF and LF endings, ignores comments and non-data fields, and
//! fails closed on oversized frames instead of buffering without bound.

use mycode_core::{ProviderError, ProviderErrorKind};

/// Maximum accepted size of one line, and so of one data payload.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Assembles SSE data payloads from response chunks.
#[derive(Debug, Default)]
pub struct FrameParser {
    /// The unterminated tail of the last chunk.
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
    /// Returns a protocol error when a single line exceeds
    /// [`MAX_FRAME_BYTES`].
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, ProviderError> {
        let mut frames = Vec::new();
        let mut rest = chunk;
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n') {
            let (line, tail) = rest.split_at(end + 1);
            rest = tail;
            self.reserve(line.len())?;
            let data = if self.buffer.is_empty() {
                take_data_line(line)
            } else {
                self.buffer.extend_from_slice(line);
                let data = take_data_line(&self.buffer);
                self.buffer.clear();
                data
            };
            frames.extend(data);
        }
        self.reserve(rest.len())?;
        self.buffer.extend_from_slice(rest);
        Ok(frames)
    }

    /// Fails closed before a line would grow past [`MAX_FRAME_BYTES`].
    fn reserve(&mut self, extra: usize) -> Result<(), ProviderError> {
        if self.buffer.len().saturating_add(extra) > MAX_FRAME_BYTES {
            self.buffer.clear();
            return Err(ProviderError::with_message(
                ProviderErrorKind::Protocol,
                "SSE frame exceeds the size limit",
            ));
        }
        Ok(())
    }

    /// Flushes a trailing unterminated line at end of stream.
    ///
    /// # Errors
    ///
    /// Never fails today: [`Self::feed`] already bounds the held line. The
    /// `Result` keeps end-of-stream failures expressible for the driver.
    pub fn finish(&mut self) -> Result<Option<String>, ProviderError> {
        let line = std::mem::take(&mut self.buffer);
        Ok(take_data_line(&line))
    }
}

/// Extracts one `data:` payload from a single line. Blank lines and non-data
/// fields are ignored. Supported providers emit one payload per line, so this
/// does not join multi-line data events.
fn take_data_line(line: &[u8]) -> Option<String> {
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    let stripped = trimmed.strip_prefix(b"data:")?;
    let payload = stripped.strip_prefix(b" ").unwrap_or(stripped);
    Some(String::from_utf8_lossy(payload).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{FrameParser, MAX_FRAME_BYTES};

    #[test]
    fn payloads_split_across_chunks_and_line_endings_reassemble() {
        let mut parser = FrameParser::new();
        assert!(parser.feed(b"event: x\r\ndata: {\"a\"").unwrap().is_empty());
        let frames = parser
            .feed(b":1}\r\n\r\n: comment\ndata:[DONE]\ndata: tail")
            .unwrap();
        assert_eq!(frames, ["{\"a\":1}", "[DONE]"]);
        assert_eq!(parser.finish().unwrap().as_deref(), Some("tail"));
        assert_eq!(parser.finish().unwrap(), None);
    }

    #[test]
    fn an_oversized_line_fails_closed_whether_split_or_whole() {
        let mut whole = vec![b'x'; MAX_FRAME_BYTES];
        whole.push(b'\n');
        let mut parser = FrameParser::new();
        assert!(parser.feed(&whole).is_err());

        let mut parser = FrameParser::new();
        assert!(
            parser
                .feed(&whole[..MAX_FRAME_BYTES / 2])
                .unwrap()
                .is_empty()
        );
        assert!(parser.feed(&whole[MAX_FRAME_BYTES / 2..]).is_err());
        // The parser recovers for the next line.
        assert_eq!(parser.feed(b"data: ok\n").unwrap(), ["ok"]);
    }

    #[test]
    fn a_line_at_the_limit_is_accepted() {
        let mut line = b"data: ".to_vec();
        line.resize(MAX_FRAME_BYTES - 1, b'y');
        line.push(b'\n');
        let frames = FrameParser::new().feed(&line).unwrap();
        assert_eq!(frames[0].len(), MAX_FRAME_BYTES - 7);
    }
}
