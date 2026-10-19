//! MSVC-style Windows argv quoting for `CreateProcessW` launches.
#![cfg_attr(not(windows), allow(dead_code))]
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
/// Returns [`ToolError::InvalidArgs`] when the program path or an argument
/// contains an interior NUL or the encoded command line exceeds 32,767 UTF-16
/// units including NUL.
pub(crate) fn windows_command_line_utf16(
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

/// ASCII symbols cmd.exe can leave unquoted in a batch argument.
const BATCH_UNQUOTED: &str = "#$*+-./:?@\\_";

/// Whether `program` is a `.bat` or `.cmd` launcher, ignoring ASCII case.
#[must_use]
pub(crate) fn is_batch_script(program: &OsStr) -> bool {
    let text = program.to_string_lossy();
    let file = text.rsplit(['\\', '/']).next().unwrap_or(text.as_ref());
    let Some((_, ext)) = file.rsplit_once('.') else {
        return false;
    };
    ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd")
}

/// Command line for `cmd.exe /e:ON /v:OFF /d /c`, without a trailing NUL.
///
/// `.bat` and `.cmd` files are parsed by cmd.exe, not `CommandLineToArgvW`.
/// `%` in the script path and in arguments is rewritten so it is not an
/// environment expansion. `"` in an argument is doubled. An argument that
/// contains `\r` or `\n` is rejected because it truncates the command.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when the script path contains `"` or
/// ends with `\`, an argument contains a NUL or a newline, or the encoded
/// line exceeds 32,767 UTF-16 units including the terminator.
pub(crate) fn windows_batch_command_line_utf16(
    script: &OsStr,
    args: &[String],
) -> Result<Vec<u16>, ToolError> {
    let script_units = encode_os(script)?;
    if script_units.contains(&u16::from(b'"')) || script_units.last() == Some(&u16::from(b'\\')) {
        return Err(ToolError::InvalidArgs(
            "Windows batch script path may not contain `\"` or end with `\\`".into(),
        ));
    }
    let mut cmd: Vec<u16> = "cmd.exe /e:ON /v:OFF /d /c \"".encode_utf16().collect();
    cmd.push(u16::from(b'"'));
    // The path sits inside the same `/c` string as the arguments, so cmd.exe
    // expands `%` here too. Guard it the same way as an argument.
    append_bat_percent_units(&mut cmd, &script_units);
    cmd.push(u16::from(b'"'));
    for arg in args {
        if arg.contains('\r') || arg.contains('\n') {
            return Err(ToolError::InvalidArgs(
                "batch file arguments must not contain a newline".into(),
            ));
        }
        reject_nul(arg, "argument")?;
        cmd.push(u16::from(b' '));
        append_bat_arg(&mut cmd, arg);
    }
    cmd.push(u16::from(b'"'));
    let with_terminator = cmd.len().saturating_add(1);
    if with_terminator > WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS {
        return Err(ToolError::InvalidArgs(format!(
            "command line is too long for CreateProcessW's 32,767 UTF-16-code-unit limit \
             (including the terminator): encoded length is {with_terminator}"
        )));
    }
    Ok(cmd)
}

/// Quote one batch argument the way Rust's standard library does.
fn append_bat_arg(cmd: &mut Vec<u16>, arg: &str) {
    let mut quote = arg.is_empty() || arg.ends_with('\\');
    for c in arg.chars() {
        let ascii_needs_quotes =
            c.is_ascii() && !(c.is_ascii_alphanumeric() || BATCH_UNQUOTED.contains(c));
        if ascii_needs_quotes || c.is_control() {
            quote = true;
        }
    }
    if quote {
        cmd.push(u16::from(b'"'));
    }
    let mut backslashes = 0usize;
    for unit in arg.encode_utf16() {
        if unit == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if unit == u16::from(b'"') {
                cmd.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
                cmd.push(u16::from(b'"'));
            } else if unit == u16::from(b'%') || unit == u16::from(b'\r') {
                push_percent_guard(cmd);
            }
            backslashes = 0;
        }
        cmd.push(unit);
    }
    if quote {
        cmd.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
        cmd.push(u16::from(b'"'));
    }
}

/// Copy UTF-16 units, guarding `%` and `\r` the way [`append_bat_arg`] does.
fn append_bat_percent_units(cmd: &mut Vec<u16>, units: &[u16]) {
    for &unit in units {
        if unit == u16::from(b'%') || unit == u16::from(b'\r') {
            push_percent_guard(cmd);
        }
        cmd.push(unit);
    }
}

/// Zero-length `%cd:~,%` so cmd.exe does not expand `%VAR%`.
///
/// Command extensions (`/e:ON`) are on. `%cd:~,%` is a substring of the
/// always-defined `cd` variable with both indexes omitted, so it expands to
/// nothing and the `%` that follows is not the start of a variable.
fn push_percent_guard(cmd: &mut Vec<u16>) {
    cmd.extend("%%cd:~,".encode_utf16());
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{is_batch_script, windows_batch_command_line_utf16, windows_command_line_utf16};

    fn decode(units: &[u16]) -> String {
        String::from_utf16(units).expect("command line is valid UTF-16")
    }

    #[test]
    fn quotes_spaces_empty_args_quotes_and_trailing_backslashes() {
        let line = windows_command_line_utf16(
            OsStr::new(r"C:\Program Files\py.exe"),
            &[
                "-u".to_owned(),
                String::new(),
                "a b".to_owned(),
                "x\"y".to_owned(),
                "tail \\".to_owned(),
            ],
        )
        .expect("command line");
        assert_eq!(
            decode(&line),
            "\"C:\\Program Files\\py.exe\" -u \"\" \"a b\" \"x\\\"y\" \"tail \\\\\""
        );
    }

    #[test]
    fn rejects_an_interior_nul() {
        let error =
            windows_command_line_utf16(OsStr::new("py"), &["a\0b".to_owned()]).expect_err("nul");
        let text = error.to_string();
        assert!(text.contains("NUL"), "{text}");
    }

    #[test]
    fn batch_scripts_are_detected_by_extension() {
        assert!(is_batch_script(OsStr::new(r"C:\Tools\npx.cmd")));
        assert!(is_batch_script(OsStr::new(r"C:\Tools\npx.CMD")));
        assert!(is_batch_script(OsStr::new("run.bat")));
        assert!(!is_batch_script(OsStr::new(r"C:\cmd\npx.exe")));
        assert!(!is_batch_script(OsStr::new("npx")));
    }

    #[test]
    fn batch_command_line_escapes_percent_quotes_and_ampersand() {
        let line = windows_batch_command_line_utf16(
            OsStr::new(r"C:\Tools\npx.cmd"),
            &[
                "%PATH%".to_owned(),
                "say \"hi\"".to_owned(),
                "a&b".to_owned(),
            ],
        )
        .expect("batch command line");
        assert_eq!(
            decode(&line),
            "cmd.exe /e:ON /v:OFF /d /c \"\"C:\\Tools\\npx.cmd\" \
             \"%%cd:~,%PATH%%cd:~,%\" \"say \"\"hi\"\"\" \"a&b\"\""
        );
    }

    #[test]
    fn batch_script_path_escapes_percent_like_an_argument() {
        let line = windows_batch_command_line_utf16(
            OsStr::new(r"C:\Tools\100%done\npx.cmd"),
            &["%PATH%".to_owned()],
        )
        .expect("batch command line");
        assert_eq!(
            decode(&line),
            "cmd.exe /e:ON /v:OFF /d /c \"\"C:\\Tools\\100%%cd:~,%done\\npx.cmd\" \
             \"%%cd:~,%PATH%%cd:~,%\"\""
        );
    }

    #[test]
    fn batch_command_line_rejects_newlines_and_a_quoted_script() {
        let newline = windows_batch_command_line_utf16(OsStr::new("npx.cmd"), &["a\nb".to_owned()])
            .expect_err("newline");
        assert!(newline.to_string().contains("newline"), "{newline}");
        let quoted =
            windows_batch_command_line_utf16(OsStr::new("np\"x.cmd"), &[]).expect_err("quote");
        assert!(quoted.to_string().contains("batch script"), "{quoted}");
    }
}
