//! Bounded concurrent stdout/stderr capture for native execution builtins.
//!
//! Streams are drained together while a fixed retained prefix is kept. The
//! retained size covers a UTF-16 BOM plus two raw bytes per rendered UTF-8
//! byte so truncation remains observable after decoding.

use std::fmt::Write as _;

use tokio::io::{AsyncRead, AsyncReadExt};

/// Combined stdout+stderr cap per call (~50 KiB, then a notice).
pub(crate) const MAX_OUTPUT_BYTES: usize = 50 * 1024;

/// A fixed scratch buffer keeps discarded output from growing user-space memory.
const OUTPUT_READ_CHUNK_BYTES: usize = 16 * 1024;
/// BOM-marked UTF-16 ASCII uses two raw bytes per rendered UTF-8 byte. Retain
/// one rendered byte beyond the output cap so truncation remains observable.
pub(crate) const MAX_RETAINED_OUTPUT_BYTES: usize = 2 + 2 * (MAX_OUTPUT_BYTES + 1);

/// A bounded stream prefix, the newest bytes past it, and the total bytes
/// drained from that stream.
///
/// Build and test runners print their failure summary last, so a capture
/// that kept only the prefix dropped the part worth reading.
#[derive(Debug, Default)]
pub(crate) struct CapturedStream {
    pub(crate) retained: Vec<u8>,
    /// Bytes after the prefix filled, trimmed to the newest
    /// [`MAX_RETAINED_OUTPUT_BYTES`] whenever it doubles.
    pub(crate) tail: Vec<u8>,
    pub(crate) total_bytes: u64,
}

impl CapturedStream {
    /// An empty capture with the retained-prefix capacity reserved.
    pub(crate) fn new() -> Self {
        Self {
            retained: Vec::with_capacity(MAX_RETAINED_OUTPUT_BYTES),
            tail: Vec::new(),
            total_bytes: 0,
        }
    }

    fn keep(&mut self, bytes: &[u8]) {
        let room = MAX_RETAINED_OUTPUT_BYTES.saturating_sub(self.retained.len());
        let (head, rest) = bytes.split_at(bytes.len().min(room));
        self.retained.extend_from_slice(head);
        if rest.is_empty() {
            return;
        }
        self.tail.extend_from_slice(rest);
        if self.tail.len() >= 2 * MAX_RETAINED_OUTPUT_BYTES {
            let excess = self.tail.len() - MAX_RETAINED_OUTPUT_BYTES;
            self.tail.drain(..excess);
        }
    }

    /// The newest retained bytes and their offset in the stream.
    fn newest(&self) -> (&[u8], u64) {
        let start = self.tail.len().saturating_sub(MAX_RETAINED_OUTPUT_BYTES);
        let tail = &self.tail[start..];
        (tail, self.total_bytes.saturating_sub(tail.len() as u64))
    }
}

/// Decodes one capture. A stream that outgrew its prefix renders as that
/// prefix and its newest bytes around a marker naming the gap; the bool
/// says whether bytes were dropped.
pub(crate) fn captured_text(stream: &CapturedStream) -> (String, bool) {
    let head = decode_captured_text(&stream.retained);
    let (tail, tail_start) = stream.newest();
    if tail.is_empty() {
        return (head, false);
    }
    let omitted = tail_start.saturating_sub(stream.retained.len() as u64);
    let mut text = head;
    if omitted > 0 {
        let _ = write!(text, "\n[... {omitted} bytes omitted ...]\n");
    }
    text.push_str(&decode_tail(&stream.retained, tail, tail_start));
    (text, omitted > 0)
}

/// Decodes bytes cut out of the middle of a stream in the encoding its
/// prefix chose. The cut may split a UTF-16 unit or a UTF-8 sequence, so
/// the partial unit is dropped first.
fn decode_tail(head: &[u8], tail: &[u8], tail_start: u64) -> String {
    let utf16 = if head.starts_with(&[0xfe, 0xff]) {
        Some(u16::from_be_bytes as fn([u8; 2]) -> u16)
    } else if head.starts_with(&[0xff, 0xfe]) || decode_utf16_le_without_bom(head).is_some() {
        Some(u16::from_le_bytes as fn([u8; 2]) -> u16)
    } else {
        None
    };
    if let Some(decode_unit) = utf16 {
        let tail = if tail_start % 2 == 1 {
            &tail[1..]
        } else {
            tail
        };
        return decode_utf16(tail, decode_unit);
    }
    let partial = tail
        .iter()
        .take(3)
        .take_while(|byte| (0x80..0xc0).contains(*byte))
        .count();
    decode_captured_text(&tail[partial..])
}

/// How long to keep reading after the leader exits, so output it already
/// wrote can leave the pipe buffer. This does not wait for a background
/// child to close the inherited write end.
const OUTPUT_GRACE_AFTER_EXIT: std::time::Duration = std::time::Duration::from_millis(50);

/// Read both pipes until `wait_for_exit` finishes.
///
/// # Errors
///
/// Returns the first I/O error from either pipe or from the wait.
pub(crate) async fn collect_until_exit<Out, ErrStream, Wait>(
    stdout_pipe: &mut Option<Out>,
    stderr_pipe: &mut Option<ErrStream>,
    stdout: &mut CapturedStream,
    stderr: &mut CapturedStream,
    wait_for_exit: Wait,
) -> std::io::Result<std::process::ExitStatus>
where
    Out: AsyncRead + Unpin,
    ErrStream: AsyncRead + Unpin,
    Wait: std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
{
    let drain = drain_pipes(stdout_pipe, stderr_pipe, stdout, stderr);
    tokio::pin!(drain);
    tokio::pin!(wait_for_exit);
    let mut status = None;
    let mut drained = false;
    loop {
        tokio::select! {
            biased;
            result = &mut wait_for_exit, if status.is_none() => {
                status = Some(result?);
            }
            result = &mut drain, if !drained => {
                result?;
                drained = true;
            }
        }
        if let Some(status) = status {
            if !drained {
                match tokio::time::timeout(OUTPUT_GRACE_AFTER_EXIT, &mut drain).await {
                    Ok(result) => result?,
                    Err(_elapsed) => {}
                }
            }
            return Ok(status);
        }
    }
}

/// Drain stdout and stderr concurrently while retaining bounded prefixes.
///
/// # Errors
///
/// Returns the first I/O error from either pipe.
async fn drain_pipes<Out, Err>(
    stdout_pipe: &mut Option<Out>,
    stderr_pipe: &mut Option<Err>,
    stdout: &mut CapturedStream,
    stderr: &mut CapturedStream,
) -> std::io::Result<()>
where
    Out: AsyncRead + Unpin,
    Err: AsyncRead + Unpin,
{
    let (out, err) = tokio::join!(
        read_bounded(stdout_pipe, stdout),
        read_bounded(stderr_pipe, stderr),
    );
    out.and(err)
}

/// Drain one stream to EOF while retaining only the prefix and tail needed
/// for rendering.
///
/// Continuing to read prevents capture limits from changing the child's exit
/// behavior, and a fixed scratch buffer bounds memory after the prefix is full.
///
/// # Errors
///
/// Returns an I/O error from the reader or when the total length overflows
/// `u64`.
pub(crate) async fn read_bounded<R>(
    pipe: &mut Option<R>,
    captured: &mut CapturedStream,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let Some(reader) = pipe.as_mut() else {
        return Ok(());
    };
    let mut chunk = [0_u8; OUTPUT_READ_CHUNK_BYTES];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        captured.total_bytes = captured
            .total_bytes
            .checked_add(u64::try_from(count).map_err(|_| {
                std::io::Error::other("process output read length does not fit u64")
            })?)
            .ok_or_else(|| std::io::Error::other("process output length overflowed u64"))?;
        captured.keep(&chunk[..count]);
    }
    drop(pipe.take());
    Ok(())
}

/// Decode captured process text.
///
/// Precedence: byte-order marks, a BOM-less UTF-16LE heuristic, strict UTF-8,
/// then Windows legacy pages.
/// A GUI host often has no console, so `GetConsoleOutputCP` is 0 or UTF-8
/// while `cmd` / `dir` still write the OEM page (GBK on Chinese Windows).
/// Redirected pipes are bytes, not a console, so the OEM page is tried
/// before the ANSI page and the console output page.
pub(crate) fn decode_captured_text(bytes: &[u8]) -> String {
    if let Some(payload) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(payload).into_owned();
    }
    if let Some(payload) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(payload, u16::from_le_bytes);
    }
    if let Some(payload) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(payload, u16::from_be_bytes);
    }
    // PowerShell writes UTF-16LE to a pipe with no BOM. Those bytes are
    // often also valid UTF-8 (ASCII plus NUL), so the UTF-16 check has to
    // run before `from_utf8` or CJK comes out as `o\0k\0`.
    if let Some(text) = decode_utf16_le_without_bom(bytes) {
        return text;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => return text.to_owned(),
        // A capped prefix can end inside a sequence. That is still UTF-8;
        // sending it down the code-page path turned the whole buffer into
        // mojibake.
        Err(error) if error.error_len().is_none() => {
            return String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned();
        }
        Err(_) => {}
    }
    if let Some(text) = decode_console_codepage(bytes) {
        return text;
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// UTF-16LE with no BOM. PowerShell pipes often emit this for non-ASCII
/// text. A buffer is accepted only when most units look like BMP text
/// (ASCII, CJK, kana, fullwidth), so arbitrary binary stays on the
/// code-page path.
fn decode_utf16_le_without_bom(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
        return None;
    }
    // UTF-16LE text has NULs in the high byte (odd index). Plain UTF-8,
    // including the shell launch probe, does not. Requiring half of those
    // high bytes to be NUL keeps ASCII UTF-8 from being read as CJK.
    let pairs = bytes.len() / 2;
    let mut odd_nuls = 0usize;
    let mut even_nuls = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != 0 {
            continue;
        }
        if index % 2 == 0 {
            even_nuls += 1;
        } else {
            odd_nuls += 1;
        }
    }
    if odd_nuls == 0 || odd_nuls * 2 < pairs || even_nuls > odd_nuls {
        return None;
    }
    let (chunks, _) = bytes.as_chunks::<2>();
    let decoded: Vec<u16> = chunks.iter().copied().map(u16::from_le_bytes).collect();
    if decoded.contains(&0) {
        return None;
    }
    let text = String::from_utf16(&decoded).ok()?;
    let chars = text.chars().count();
    let printable = text
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        .count();
    (chars > 0 && printable * 4 >= chars * 3).then_some(text)
}

/// Decodes bytes with the OEM page, then the ANSI page, then the console
/// output page. UTF-8 and page 0 are skipped; the first page that converts
/// wins.
#[cfg(windows)]
fn decode_console_codepage(bytes: &[u8]) -> Option<String> {
    // SAFETY: these queries read process-global code-page ids. No handles
    // or buffers are involved.
    let oem = unsafe { windows_sys::Win32::Globalization::GetOEMCP() };
    let ansi = unsafe { windows_sys::Win32::Globalization::GetACP() };
    let console = unsafe { windows_sys::Win32::System::Console::GetConsoleOutputCP() };
    decode_legacy_pages(bytes, &[oem, ansi, console])
}

/// Tries each code page in order, skipping UTF-8 and duplicates.
#[cfg(windows)]
fn decode_legacy_pages(bytes: &[u8], pages: &[u32]) -> Option<String> {
    let mut seen = [0_u32; 4];
    let mut count = 0_usize;
    for &page in pages {
        if page == 0 || page == 65001 || seen[..count].contains(&page) {
            continue;
        }
        if count < seen.len() {
            seen[count] = page;
            count += 1;
        }
        if let Some(text) = decode_multibyte(bytes, page) {
            return Some(text);
        }
    }
    None
}

#[cfg(not(windows))]
const fn decode_console_codepage(_bytes: &[u8]) -> Option<String> {
    None
}

/// Best-effort conversion of one Windows code page; `None` keeps the lossy
/// UTF-8 fallback when the conversion fails or the page is UTF-8 itself.
#[cfg(windows)]
pub(crate) fn decode_multibyte(bytes: &[u8], codepage: u32) -> Option<String> {
    if codepage == 0 || codepage == 65001 || bytes.is_empty() {
        return None;
    }
    // SAFETY: MultiByteToWideChar receives the input buffer by pointer with
    // its exact length and writes only into the sized output buffer.
    let size = unsafe {
        windows_sys::Win32::Globalization::MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        )
    };
    if size <= 0 {
        return None;
    }
    let mut buffer = vec![0_u16; size as usize];
    let written = unsafe {
        windows_sys::Win32::Globalization::MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            buffer.as_mut_ptr(),
            size,
        )
    };
    if written <= 0 {
        return None;
    }
    buffer.truncate(written as usize);
    String::from_utf16(&buffer).ok()
}

fn decode_utf16(payload: &[u8], decode_unit: fn([u8; 2]) -> u16) -> String {
    let (chunks, remainder) = payload.as_chunks::<2>();
    let units = chunks.iter().copied().map(decode_unit).collect::<Vec<_>>();
    let mut decoded = String::from_utf16_lossy(&units);
    if !remainder.is_empty() {
        decoded.push('\u{fffd}');
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::{CapturedStream, MAX_RETAINED_OUTPUT_BYTES, captured_text, decode_captured_text};

    fn feed(stream: &mut CapturedStream, bytes: &[u8]) {
        for chunk in bytes.chunks(16 * 1024) {
            stream.total_bytes += chunk.len() as u64;
            stream.keep(chunk);
        }
    }

    #[test]
    fn long_output_keeps_its_last_lines() {
        let mut stream = CapturedStream::new();
        let mut bytes = b"first line\n".to_vec();
        bytes.resize(5 * MAX_RETAINED_OUTPUT_BYTES, b'.');
        bytes.extend_from_slice(b"\ntest result: FAILED. 1 failed\n");
        feed(&mut stream, &bytes);
        assert!(stream.tail.len() < 2 * MAX_RETAINED_OUTPUT_BYTES);
        let (text, cut) = captured_text(&stream);
        assert!(cut);
        assert!(text.starts_with("first line\n"), "{}", &text[..40]);
        assert!(text.ends_with("test result: FAILED. 1 failed\n"));
        let omitted = bytes.len() - 2 * MAX_RETAINED_OUTPUT_BYTES;
        assert!(text.contains(&format!("[... {omitted} bytes omitted ...]")));
    }

    #[test]
    fn a_short_tail_past_the_prefix_drops_nothing() {
        let mut stream = CapturedStream::new();
        let mut bytes = vec![b'a'; MAX_RETAINED_OUTPUT_BYTES];
        bytes.extend_from_slice(b"end");
        feed(&mut stream, &bytes);
        let (text, cut) = captured_text(&stream);
        assert!(!cut);
        assert_eq!(text.len(), bytes.len());
        assert!(text.ends_with("aend"));
    }

    #[test]
    fn a_cut_tail_realigns_to_the_prefix_encoding() {
        let mut stream = CapturedStream::new();
        let mut bytes = utf16_le(&"ok 你好\r\n".repeat(MAX_RETAINED_OUTPUT_BYTES / 4));
        bytes.extend(utf16_le("完成\r\n"));
        feed(&mut stream, &bytes);
        let (text, cut) = captured_text(&stream);
        assert!(cut);
        assert!(text.starts_with("ok 你好\r\n"));
        assert!(text.ends_with("ok 你好\r\n完成\r\n"));

        // The prefix ends on a lead byte and the newest window starts on a
        // continuation byte.
        let mut stream = CapturedStream::new();
        let mut bytes = b"a".to_vec();
        bytes.extend("é".repeat(2 * MAX_RETAINED_OUTPUT_BYTES).into_bytes());
        bytes.extend_from_slice(b"done!");
        feed(&mut stream, &bytes);
        let (text, _) = captured_text(&stream);
        assert!(!text.contains('\u{fffd}'), "a split sequence was decoded");
        assert!(text.starts_with("aé"));
        assert!(text.ends_with("éédone!"));
    }

    fn utf16_le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn utf16_le_without_bom_decodes_cjk() {
        let bytes = utf16_le("你好 pwsh\r\n");
        assert!(!bytes.starts_with(&[0xFF, 0xFE]));
        assert_eq!(decode_captured_text(&bytes), "你好 pwsh\r\n");
    }

    #[test]
    fn utf8_still_wins_over_the_utf16_heuristic() {
        assert_eq!(decode_captured_text("café".as_bytes()), "café");
    }

    #[test]
    fn odd_invalid_utf8_is_not_decoded_as_utf16() {
        let text = decode_captured_text(&[0xFF, 0x00, 0x41]);
        // The odd length keeps this off the UTF-16 path, which would swallow
        // the NUL into a code unit. Whatever fallback page decodes the 0xFF,
        // the NUL and the ASCII byte must survive verbatim.
        assert!(text.contains('\0') && text.ends_with('A'), "{text:?}");
    }

    #[test]
    fn ascii_utf16_le_without_bom_decodes() {
        assert_eq!(decode_captured_text(&utf16_le("ok\r\n")), "ok\r\n");
    }
}
