//! MSVC-style Windows argv quoting for the `shell` program launch.
//!
//! `CreateProcessW` receives a single UTF-16 command line. argv0 is always
//! quoted. Empty arguments and arguments containing whitespace or `"` are
//! wrapped; `n` backslashes immediately before a quote become `2n+1`
//! backslashes plus the escaped quote, and `n` trailing backslashes before a
//! closing quote become `2n`.
use std::ffi::OsStr;

use crate::tool::ToolError;

/// Documented `CreateProcessW` UTF-16 command-line limit, including NUL.
pub(super) const WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS: usize = 32_767;

/// Encode `argv0` plus `args` as a UTF-16 command line without terminator.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when an argument contains an interior
/// NUL or the encoded command line exceeds 32,767 UTF-16 units including NUL.
pub(super) fn windows_command_line_utf16(
    argv0: &OsStr,
    args: &[String],
) -> Result<Vec<u16>, ToolError> {
    let mut cmd = Vec::new();
    append_quoted(&mut cmd, encode_os(argv0)?, true);
    for arg in args {
        reject_nul(arg, "argument")?;
        cmd.push(u16::from(b' '));
        append_quoted(&mut cmd, arg.encode_utf16(), needs_quotes(arg.chars()));
    }
    let with_terminator = cmd.len().saturating_add(1);
    if with_terminator > WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS {
        return Err(ToolError::InvalidArgs(format!(
            "command line is too long for CreateProcessW's 32,767 UTF-16-code-unit limit \
             (including the terminator): encoded length is {with_terminator}"
        )));
    }
    Ok(cmd)
}

fn encode_os(value: &OsStr) -> Result<Vec<u16>, ToolError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let units: Vec<u16> = value.encode_wide().collect();
        if units.contains(&0) {
            return Err(ToolError::InvalidArgs(
                "program path contains an interior NUL".into(),
            ));
        }
        Ok(units)
    }
    #[cfg(not(windows))]
    {
        reject_nul(&value.to_string_lossy(), "program path")?;
        Ok(value.to_string_lossy().encode_utf16().collect())
    }
}

fn needs_quotes<I>(chars: I) -> bool
where
    I: IntoIterator<Item = char>,
{
    let mut empty = true;
    for c in chars {
        empty = false;
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '"') {
            return true;
        }
    }
    empty
}

fn append_quoted<I>(cmd: &mut Vec<u16>, units: I, quote: bool)
where
    I: IntoIterator<Item = u16>,
{
    if quote {
        cmd.push(u16::from(b'"'));
    }
    let mut backslashes = 0usize;
    for unit in units {
        if unit == u16::from(b'\\') {
            backslashes += 1;
            cmd.push(unit);
            continue;
        }
        if unit == u16::from(b'"') {
            cmd.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            backslashes = 0;
            cmd.push(unit);
            continue;
        }
        backslashes = 0;
        cmd.push(unit);
    }
    if quote {
        cmd.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
        cmd.push(u16::from(b'"'));
    }
}

fn reject_nul(value: &str, what: &str) -> Result<(), ToolError> {
    if value.contains('\0') {
        Err(ToolError::InvalidArgs(format!(
            "{what} contains an interior NUL"
        )))
    } else {
        Ok(())
    }
}
