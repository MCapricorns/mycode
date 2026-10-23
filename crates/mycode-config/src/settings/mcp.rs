//! MCP-server row shape shared by the magpie Library parser and the settings
//! UI.
//!
//! Stdio servers spawn a local command, so the command-line splitter used by
//! the import path lives here next to its consumer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Maximum MCP server entries.
pub const MAX_MCP_SERVERS: usize = 64;
/// Maximum extra environment variables per stdio server.
pub const MAX_MCP_ENV_VARS: usize = 32;
/// Maximum extra HTTP headers per server.
pub const MAX_MCP_HEADERS: usize = 32;
/// Maximum arguments per stdio server.
pub(crate) const MAX_MCP_ARGS: usize = 64;

/// Whether `key` can name a child-process environment variable.
pub(crate) fn is_env_var_name(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.chars().any(char::is_control)
}

/// Whether `key` is one HTTP header name (`token` grammar).
pub(crate) fn is_header_name(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// One configured MCP server binding.
///
/// Stdio servers spawn a local command; HTTP servers speak the MCP
/// Streamable-HTTP wire against one https endpoint. The optional API key is
/// stored in the secret store under `mcp-<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerSettings {
    /// Unique server identity (lowercase portable).
    pub id: String,
    /// Enabled.
    pub enabled: bool,
    /// Transport: `stdio` or `http`.
    #[serde(rename = "transport")]
    pub transport: String,
    /// Stdio: executable command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Stdio: bounded argument list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Stdio: extra environment variables for the child process. Secrets
    /// belong in the key vault, not here; this is for switches and paths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// HTTP: full https endpoint URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// HTTP: credential header style, `bearer` (default) or `x-api-key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_header: Option<String>,
    /// HTTP: extra headers sent with every request. magpie's Library writes
    /// vendor headers here (`X-Anysearch-Client`, `CONTEXT7_API_KEY`, …);
    /// a pasted `Authorization` / `x-api-key` value still moves to the key
    /// vault instead.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// Whether `command` is one executable, not a shell string.
///
/// A single token is accepted. A path that contains spaces is accepted when
/// it includes `/` or `\` and none of its tokens is a flag. Shell
/// metacharacters, pipelines, and `program -flag` strings are rejected.
/// Arguments belong in the `args` list.
#[must_use]
pub fn is_mcp_executable(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() || command.chars().any(char::is_control) {
        return false;
    }
    if command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '&' | ';' | '$' | '`' | '<' | '>' | '(' | ')' | '*' | '?' | '!'
        )
    }) {
        return false;
    }
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() || parts.iter().any(|part| part.starts_with('-')) {
        return false;
    }
    if parts.len() == 1 {
        return true;
    }
    command.contains('/') || command.contains('\\')
}

/// Splits one command line into the program and its arguments.
///
/// Whitespace separates words; single or double quotes group a word. Inside
/// double quotes a backslash escapes only `"` and `\`, so Windows paths such
/// as `"C:\My Docs"` survive. This is the shape MCP server docs publish
/// (`npx -y @scope/server --flag "a b"`), so the settings form can take the
/// whole line in one field.
#[must_use]
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some('"') if ch == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::is_mcp_executable;

    #[test]
    fn command_is_an_executable_not_a_shell_string() {
        assert!(is_mcp_executable("npx"));
        assert!(is_mcp_executable(r"C:\Program Files\npx.cmd"));
        assert!(is_mcp_executable("/usr/local/bin/my tool"));
        assert!(!is_mcp_executable("npx -y foo"));
        assert!(!is_mcp_executable("echo hello"));
        assert!(!is_mcp_executable("cat file | head"));
        assert!(!is_mcp_executable(""));
    }
}
