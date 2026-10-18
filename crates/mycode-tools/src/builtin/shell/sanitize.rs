//! Captured-output cleanup: PowerShell CLIXML error records, `_xHHHH_`
//! escapes, and ANSI/VT control sequences become readable plain text.

const CLIXML_MARKER: &str = "#< CLIXML";

/// Turns captured shell text into readable plain text.
///
/// PowerShell's redirected error stream is CLIXML. Control characters there
/// are `_xHHHH_` escapes (`_x001B_` is ESC), and the message may still carry
/// ANSI SGR codes. Escapes are decoded for every captured string, including
/// fragments with no `#< CLIXML` marker. When the marker is present,
/// `<S S="Error">` and `<S S="Warning">` nodes are extracted as before, then
/// ANSI/VT sequences are stripped from that text.
#[must_use]
pub(crate) fn sanitize_captured_shell_text(text: &str) -> String {
    if !text.contains(CLIXML_MARKER) && !text.contains("_x") && !has_ansi_introducer(text) {
        return text.to_owned();
    }
    let readable = if text.contains(CLIXML_MARKER) {
        let errors = extract_clixml_s_nodes(text, "Error");
        if errors.is_empty() {
            decode_clixml_char_escapes(&strip_clixml_header(text))
        } else {
            let mut lines = errors;
            lines.extend(extract_clixml_s_nodes(text, "Warning"));
            lines.join("\n")
        }
    } else {
        decode_clixml_char_escapes(text)
    };
    strip_ansi_sequences(&readable)
}

fn extract_clixml_s_nodes(text: &str, kind: &str) -> Vec<String> {
    let open = format!(r#"<S S="{kind}">"#);
    let mut nodes = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find("</S>") else {
            break;
        };
        nodes.push(decode_clixml_text(&after[..end]));
        rest = &after[end + 4..];
    }
    nodes
}

fn decode_clixml_text(text: &str) -> String {
    // `&amp;` last, so an escaped entity such as `&amp;lt;` stays `&lt;`.
    let unescaped = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    decode_clixml_char_escapes(&unescaped).trim_end().to_owned()
}

/// Decodes every CLIXML `_xHHHH_` escape (4 hex digits) to a Unicode scalar.
///
/// Surrogate code points are dropped. The scan is one pass, so `_x005F_`
/// (underscore) does not re-open the following text as another escape.
fn decode_clixml_char_escapes(text: &str) -> String {
    if !text.contains("_x") {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if is_clixml_char_escape(bytes, index) {
            let code =
                u32::from_str_radix(&text[index + 2..index + 6], 16).expect("four hex digits");
            if let Some(ch) = char::from_u32(code) {
                out.push(ch);
            }
            index += 7;
            continue;
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index on a char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn is_clixml_char_escape(bytes: &[u8], index: usize) -> bool {
    index + 7 <= bytes.len()
        && bytes[index] == b'_'
        && bytes[index + 1] == b'x'
        && bytes[index + 6] == b'_'
        && bytes[index + 2..index + 6]
            .iter()
            .all(u8::is_ascii_hexdigit)
}

fn has_ansi_introducer(text: &str) -> bool {
    text.chars().any(is_ansi_introducer)
}

fn is_ansi_introducer(ch: char) -> bool {
    matches!(
        ch,
        '\u{1b}' | '\u{90}' | '\u{98}' | '\u{9b}' | '\u{9d}' | '\u{9e}' | '\u{9f}'
    )
}

/// Removes ANSI/VT sequences: CSI, OSC, string Fe sequences, and other
/// single-character ESC Fe/Fp/Fs sequences. `text` is already decoded, so
/// both a real ESC and a former `_x001B_` are the U+001B character here.
fn strip_ansi_sequences(text: &str) -> String {
    if !has_ansi_introducer(text) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_ansi_introducer) {
        out.push_str(&rest[..start]);
        let skip = ansi_sequence_len(&rest[start..]);
        if skip == 0 {
            let ch = rest[start..].chars().next().expect("introducer");
            out.push(ch);
            rest = &rest[start + ch.len_utf8()..];
            continue;
        }
        rest = &rest[start + skip..];
    }
    out.push_str(rest);
    out
}

fn ansi_sequence_len(text: &str) -> usize {
    let mut chars = text.char_indices();
    let Some((_, first)) = chars.next() else {
        return 0;
    };
    match first {
        '\u{1b}' => esc_sequence_len(text, chars),
        '\u{9b}' => csi_payload_len(text, chars),
        '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => string_payload_len(text, chars),
        _ => 0,
    }
}

fn esc_sequence_len(text: &str, mut chars: std::str::CharIndices<'_>) -> usize {
    let Some((index, next)) = chars.next() else {
        return 1;
    };
    match next {
        '[' => csi_payload_len(text, chars),
        ']' | 'P' | 'X' | '^' | '_' => string_payload_len(text, chars),
        ch if ('\u{20}'..='\u{2f}').contains(&ch) => escape_intermediate_len(index, chars),
        ch if ('\u{30}'..='\u{7e}').contains(&ch) => index + ch.len_utf8(),
        _ => 1,
    }
}

/// CSI payload, with the introducer (`ESC [` or U+009B) already consumed.
fn csi_payload_len(text: &str, chars: std::str::CharIndices<'_>) -> usize {
    let mut end = bytes_already_consumed(text, &chars);
    for (index, ch) in chars {
        if ('\u{40}'..='\u{7e}').contains(&ch) {
            return index + ch.len_utf8();
        }
        if !('\u{20}'..='\u{3f}').contains(&ch) {
            return index;
        }
        end = index + ch.len_utf8();
    }
    end
}

/// OSC/DCS/SOS/PM/APC payload. Ends at BEL, 8-bit ST, or `ESC \`.
fn string_payload_len(text: &str, mut chars: std::str::CharIndices<'_>) -> usize {
    let mut end = bytes_already_consumed(text, &chars);
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\u{7}' | '\u{9c}' => return index + ch.len_utf8(),
            '\u{1b}' => {
                if chars.as_str().starts_with('\\') {
                    return index + 2;
                }
                return index;
            }
            _ => end = index + ch.len_utf8(),
        }
    }
    end
}

/// ESC intermediate bytes (0x20–0x2F) plus the final byte (0x30–0x7E).
fn escape_intermediate_len(first_index: usize, chars: std::str::CharIndices<'_>) -> usize {
    let mut end = first_index + 1;
    for (index, ch) in chars {
        if ('\u{20}'..='\u{2f}').contains(&ch) {
            end = index + ch.len_utf8();
            continue;
        }
        if ('\u{30}'..='\u{7e}').contains(&ch) {
            return index + ch.len_utf8();
        }
        return index;
    }
    end
}

fn bytes_already_consumed(text: &str, chars: &std::str::CharIndices<'_>) -> usize {
    text.len() - chars.as_str().len()
}

fn strip_clixml_header(text: &str) -> String {
    let Some(start) = text.find(CLIXML_MARKER) else {
        return text.to_owned();
    };
    let prefix = text[..start].trim_end_matches(['\r', '\n']);
    let rest = &text[start + CLIXML_MARKER.len()..];
    let suffix = rest
        .rfind("</Objs>")
        .map(|end| rest[end + "</Objs>".len()..].trim_start_matches(['\r', '\n']))
        .unwrap_or("");
    match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => String::new(),
        (false, true) => prefix.to_owned(),
        (true, false) => suffix.to_owned(),
        (false, false) => format!("{prefix}\n{suffix}"),
    }
}

#[cfg(test)]
mod tests;
