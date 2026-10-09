//! Shared C-string assembly for the Unix launch paths.
#![cfg(any(
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]

use std::ffi::{CString, OsString};

use crate::tool::ToolError;

/// Builds the NUL-terminated `argv` table: `argv0` first, then every argument.
pub(super) fn build_cstring_vec(argv0: &str, args: &[String]) -> Result<Vec<CString>, ToolError> {
    let mut out = Vec::with_capacity(args.len() + 1);
    out.push(
        CString::new(argv0)
            .map_err(|_| ToolError::InvalidArgs("program path contains an interior NUL".into()))?,
    );
    for arg in args {
        out.push(
            CString::new(arg.as_str())
                .map_err(|_| ToolError::InvalidArgs("argument contains an interior NUL".into()))?,
        );
    }
    Ok(out)
}

/// Builds the NUL-terminated `envp` table of `KEY=VALUE` byte strings.
pub(super) fn build_env_cstrings(env: &[(OsString, OsString)]) -> Result<Vec<CString>, ToolError> {
    let mut out = Vec::new();
    for (key, value) in env {
        let mut pair = key.clone();
        pair.push("=");
        pair.push(value);
        out.push(CString::new(pair.as_encoded_bytes()).map_err(|_| {
            ToolError::InvalidArgs("environment value contains an interior NUL".into())
        })?);
    }
    Ok(out)
}
