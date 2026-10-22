//! PowerShell launch: the UTF-8 prelude, the encoded command, and the
//! command-line budget every Windows invocation is checked against.

use super::*;
pub(super) fn powershell_script_for(command: &str) -> String {
    let mut script = String::with_capacity(command.len() + POWERSHELL_UTF8_PRELUDE.len() + 4);
    // Strip a leading UTF-8 BOM so a leading `using` statement remains the
    // first token PowerShell sees after `-EncodedCommand` decoding.
    let command = command.strip_prefix('\u{feff}').unwrap_or(command);
    if command.is_empty() {
        // PowerShell 7 rejects an empty -EncodedCommand payload as not Base64.
        script.push_str("#\n");
        script.push_str(POWERSHELL_UTF8_PRELUDE);
        return script;
    }
    let prologue = powershell_prologue_units(command);
    script.push_str(&command[..prologue]);
    if !script.is_empty() && !script.ends_with('\n') && !script.ends_with(';') {
        script.push('\n');
    }
    script.push_str(POWERSHELL_UTF8_PRELUDE);
    let rest = &command[prologue..];
    if rest.trim_start().starts_with(';') {
        script.push(' ');
    } else {
        script.push('\n');
    }
    script.push_str(rest);
    script
}

/// Byte offset of the end of the leading statement-ordering prologue —
/// whitespace, line/block comments, `using` statements, script attributes,
/// and one `param (...)` block — which PowerShell requires (`using`) or
/// expects (`param` and its attributes) before all other statements. The
/// UTF-8 prelude is inserted right after it, so those forms keep their
/// required position.
pub(super) fn powershell_prologue_units(command: &str) -> usize {
    let mut index = 0_usize;
    let mut saw_attribute = false;
    while index < command.len() {
        let next = skip_ps_trivia(command, index);
        if next > index {
            index = next;
            continue;
        }
        if !saw_attribute && let Some(after_using) = scan_ps_using(command, index) {
            index = after_using;
            continue;
        }
        if let Some(after_param) = scan_ps_param_block(command, index) {
            index = after_param;
            break;
        }
        if let Some(after_attr) = scan_ps_attribute(command, index) {
            let after_trivia = skip_ps_trivia(command, after_attr);
            if scan_ps_attribute(command, after_trivia).is_some()
                || scan_ps_param_block(command, after_trivia).is_some()
            {
                saw_attribute = true;
                index = after_attr;
                continue;
            }
        }
        break;
    }
    if index > 0 && index < command.len() {
        index = consume_trailing_line_break(command, index);
    }
    index
}

pub(super) fn consume_trailing_line_break(text: &str, mut index: usize) -> usize {
    let bytes = text.as_bytes();
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'#' && !text[index..].starts_with("<#") {
        while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
            index += 1;
        }
    }
    if index < bytes.len() && bytes[index] == b'\r' {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'\n' {
        index += 1;
    }
    index
}

pub(super) fn skip_ps_trivia(text: &str, mut index: usize) -> usize {
    while index < text.len() {
        let rest = &text[index..];
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if ch.is_whitespace() || ch == '\u{feff}' {
            index += ch.len_utf8();
            continue;
        }
        if let Some(comment) = rest.strip_prefix("<#") {
            index = match comment.find("#>") {
                Some(end) => index + 2 + end + 2,
                None => text.len(),
            };
            continue;
        }
        if ch == '#' {
            index = match rest.find('\n') {
                Some(nl) => index + nl + 1,
                None => text.len(),
            };
            continue;
        }
        break;
    }
    index
}

pub(super) fn scan_ps_using(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    if !rest
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("using"))
    {
        return None;
    }
    let after_kw = rest[5..].chars().next()?;
    if !after_kw.is_whitespace() || matches!(after_kw, '\r' | '\n') {
        return None;
    }
    let mut index = start + 5;
    while index < text.len() {
        let slice = &text[index..];
        if let Some(comment) = slice.strip_prefix("<#") {
            index = match comment.find("#>") {
                Some(end) => index + 2 + end + 2,
                None => text.len(),
            };
            continue;
        }
        let ch = slice.chars().next()?;
        match ch {
            '\r' | '\n' => return Some(consume_trailing_line_break(text, index)),
            ';' => return Some(index + 1),
            '#' => return Some(consume_trailing_line_break(text, index)),
            '\'' | '"' => index = skip_ps_quoted_string(text, index),
            '{' => index = scan_ps_balanced(text, index, '{', '}')?,
            '(' => index = scan_ps_balanced(text, index, '(', ')')?,
            _ => index += ch.len_utf8(),
        }
    }
    Some(text.len())
}

pub(super) fn scan_ps_attribute(text: &str, start: usize) -> Option<usize> {
    if text.as_bytes().get(start) != Some(&b'[') {
        return None;
    }
    scan_ps_balanced(text, start, '[', ']')
}

pub(super) fn scan_ps_param_block(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    if !rest
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("param"))
    {
        return None;
    }
    let after_kw = start + 5;
    if let Some(ch) = text[after_kw..].chars().next()
        && (ch.is_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return None;
    }
    let open = skip_ps_trivia(text, after_kw);
    if text.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    scan_ps_balanced(text, open, '(', ')')
}

pub(super) fn scan_ps_balanced(text: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0_usize;
    let mut index = start;
    while index < text.len() {
        let slice = &text[index..];
        if let Some(comment) = slice.strip_prefix("<#") {
            index += comment.find("#>")? + 4;
            continue;
        }
        let ch = slice.chars().next()?;
        if ch == '#' {
            index += slice.find('\n')? + 1;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            index = skip_ps_quoted_string(text, index);
            continue;
        }
        if ch == open {
            depth += 1;
            index += ch.len_utf8();
            continue;
        }
        if ch == close {
            depth = depth.saturating_sub(1);
            index += ch.len_utf8();
            if depth == 0 {
                return Some(index);
            }
            continue;
        }
        index += ch.len_utf8();
    }
    None
}

pub(super) fn skip_ps_quoted_string(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[start];
    let mut index = start + 1;
    while index < text.len() {
        let ch = text[index..].chars().next().expect("char boundary");
        if quote == b'"' && ch == '`' {
            index += 1;
            if let Some(escaped) = text[index..].chars().next() {
                index += escaped.len_utf8();
            }
            continue;
        }
        if ch == char::from(quote) {
            index += 1;
            if bytes.get(index) == Some(&quote) {
                index += 1;
                continue;
            }
            return index;
        }
        index += ch.len_utf8();
    }
    text.len()
}

/// Skips a PowerShell here-string (`@'\n...\n'@` or `@"\n...\n"@`) when
/// `start` points at the leading `@`.
pub(super) fn skip_ps_here_string(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    let (quote, after_open) = if let Some(tail) = rest.strip_prefix("@'") {
        ('\'', tail)
    } else {
        ('"', rest.strip_prefix("@\"")?)
    };
    let line_end = if after_open.starts_with("\r\n") {
        2
    } else if after_open.starts_with('\n') {
        1
    } else {
        return None;
    };
    let mut index = start + 2 + line_end;
    let close = if quote == '\'' { "'@" } else { "\"@" };
    while index < text.len() {
        if text[index..].starts_with(close) {
            return Some(index + 2);
        }
        index = match text[index..].find('\n') {
            Some(nl) => index + nl + 1,
            None => text.len(),
        };
    }
    Some(text.len())
}

/// Rejects bash-only commands (`touch`, `export`, `source`, `which`, `chmod`,
/// `chown`, `sudo`, Unix package managers, `man`) at command position and
/// bash-only redirection/heredoc tokens (`>/dev/null`, `<<`) outside strings
/// and comments before PowerShell runs a script halfway through.
pub(super) fn lint_powershell_command(command: &str) -> Result<(), ToolError> {
    let mut index = 0_usize;
    let mut at_command_start = true;
    while index < command.len() {
        let rest = &command[index..];
        if let Some(comment) = rest.strip_prefix("<#") {
            index = match comment.find("#>") {
                Some(end) => index + 2 + end + 2,
                None => command.len(),
            };
            continue;
        }
        let ch = rest.chars().next().expect("non-empty slice");
        match ch {
            ' ' | '\t' | '\u{feff}' => {
                index += ch.len_utf8();
                continue;
            }
            '\r' | '\n' | ';' => {
                index += 1;
                at_command_start = true;
                continue;
            }
            '#' => {
                index = match rest.find('\n') {
                    Some(nl) => index + nl,
                    None => command.len(),
                };
                continue;
            }
            '@' => {
                if let Some(after_here) = skip_ps_here_string(command, index) {
                    index = after_here;
                    at_command_start = false;
                    continue;
                }
                index += 1;
                if matches!(command.as_bytes().get(index), Some(b'{' | b'(')) {
                    index += 1;
                }
                at_command_start = false;
                continue;
            }
            '\'' | '"' => {
                index = skip_ps_quoted_string(command, index);
                at_command_start = false;
                continue;
            }
            '<' if rest.starts_with("<<") => {
                return Err(ToolError::InvalidArgs(
                    "`<<` heredocs are bash syntax and are rejected in PowerShell; use a here-string (`@'\\n...\\n'@`)"
                        .into(),
                ));
            }
            '>' => {
                let after_gt = rest.trim_start_matches('>').trim_start_matches([' ', '\t']);
                if let Some(after_null) = after_gt.strip_prefix("/dev/null")
                    && after_null.chars().next().is_none_or(|next| {
                        next.is_whitespace() || matches!(next, ';' | '|' | '&' | ')' | '}')
                    })
                {
                    return Err(ToolError::InvalidArgs(
                        "`/dev/null` does not exist in PowerShell; redirect to `$null` (for example `2>$null`)"
                            .into(),
                    ));
                }
                index += 1;
                at_command_start = false;
                continue;
            }
            '|' => {
                index += if rest.starts_with("||") { 2 } else { 1 };
                at_command_start = true;
                continue;
            }
            '&' if rest.starts_with("&&") => {
                index += 2;
                at_command_start = true;
                continue;
            }
            '{' | '(' => {
                index += 1;
                at_command_start = true;
                continue;
            }
            _ => {}
        }
        if at_command_start && ch.is_ascii_alphabetic() {
            let word_len = rest
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                .count();
            let word = &rest[..word_len];
            let after_word = &rest[word_len..];
            let next_char = after_word.chars().next();
            if matches!(next_char, Some(' ' | '\t')) {
                let after_space = after_word.trim_start_matches([' ', '\t']);
                if let Some(arg_start) = after_space.chars().next()
                    && !matches!(
                        arg_start,
                        '\r' | '\n' | ';' | '|' | '&' | '}' | ')' | '#' | '='
                    )
                    && let Some(advice) = powershell_unix_command_advice(word)
                {
                    return Err(ToolError::InvalidArgs(advice.into()));
                }
            }
            index += word_len;
            at_command_start = false;
            continue;
        }
        at_command_start = false;
        index += ch.len_utf8();
    }
    Ok(())
}

pub(super) fn powershell_unix_command_advice(word: &str) -> Option<&'static str> {
    // PowerShell commands are case-insensitive; `EXPORT` bypassed the match.
    match word.to_ascii_lowercase().as_str() {
        "touch" => Some(
            "`touch` is a Unix command and does not exist in PowerShell; use `(Get-Item <path>).LastWriteTime = Get-Date` to update a timestamp or `New-Item -ItemType File <path>` to create a file",
        ),
        "export" => Some(
            "`export` is a bash builtin and does not exist in PowerShell; use `$env:NAME = \"value\"`",
        ),
        "source" => Some(
            "`source` is a bash builtin and does not exist in PowerShell; dot-source scripts with `. .\\script.ps1`",
        ),
        "which" => Some(
            "`which` is a Unix command and does not exist in PowerShell; use `Get-Command <name>`",
        ),
        "chmod" | "chown" => Some(
            "`chmod`/`chown` are Unix commands and do not exist in PowerShell; use `icacls` or `Set-Acl` on Windows",
        ),
        "sudo" => Some("`sudo` is a Unix command and does not exist in PowerShell"),
        "apt" | "apt-get" | "yum" | "dnf" | "brew" => Some(
            "Unix package managers (`apt`, `yum`, `dnf`, `brew`) do not exist on Windows; use `winget` or `scoop`",
        ),
        // Windows PowerShell aliases `man` to `help`, which pipes through a
        // pager; the captured, input-less process has nobody to page it.
        "man" => Some("`man` runs the paging `help` function; use `Get-Help <cmdlet>`"),
        _ => None,
    }
}
