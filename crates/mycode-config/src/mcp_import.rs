//! Parse a pasted MCP config document into server rows.
//!
//! Accepts Claude Desktop / Cursor `mcpServers` maps, VS Code `servers`
//! maps, a single server object, or an array of those objects. HTTP
//! `Authorization` / `x-api-key` header values are extracted into each
//! returned row for the caller to store; a leading `Bearer ` is stripped so
//! the user can paste either form.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::settings::McpServerSettings;

/// One imported server plus an optional credential lifted from headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedMcpServer {
    /// The settings row to add.
    pub server: McpServerSettings,
    /// Secret-store key, already run through [`normalize_api_key`].
    pub api_key: Option<String>,
}

/// Strips a leading `Authorization:` / `Bearer ` so the user can paste a
/// raw key or a header value. The stored key stays scheme-free; the wire
/// layer adds the configured header spelling (`Bearer` or `x-api-key`).
#[must_use]
pub fn normalize_api_key(raw: &str) -> String {
    let mut text = raw.trim();
    if let Some((name, rest)) = text.split_once(':')
        && name.eq_ignore_ascii_case("authorization")
    {
        text = rest.trim();
    }
    if let Some((scheme, rest)) = text.split_once(char::is_whitespace)
        && scheme.eq_ignore_ascii_case("bearer")
    {
        text = rest.trim();
    }
    text.to_owned()
}

/// Parses one pasted JSON document into zero or more MCP server rows.
///
/// # Errors
///
/// Returns a one-line reason when the text is not JSON or contains no
/// recognizable server entries.
pub fn parse_mcp_import(text: &str) -> Result<Vec<ImportedMcpServer>, String> {
    parse_document(text, true)
}

/// Parses a server map magpie's Library wrote.
///
/// The file is magpie's own, not a pasted document: every header stays on
/// the row as written, because a vendor header such as `CONTEXT7_API_KEY`
/// has no key-vault slot here.
///
/// # Errors
///
/// Returns a one-line reason when the text is not JSON or contains no
/// recognizable server entries.
pub fn parse_mcp_library(text: &str) -> Result<Vec<ImportedMcpServer>, String> {
    parse_document(text, false)
}

fn parse_document(text: &str, lift_secrets: bool) -> Result<Vec<ImportedMcpServer>, String> {
    let value = parse_lenient(text).ok_or_else(|| "paste is not valid JSON".to_owned())?;
    let mut imported = Vec::new();
    collect_servers(&value, &mut imported, lift_secrets)?;
    if imported.is_empty() {
        return Err("no MCP servers found in the pasted JSON".to_owned());
    }
    if imported.len() > crate::settings::MAX_MCP_SERVERS {
        return Err("too many MCP servers in the pasted JSON".to_owned());
    }
    Ok(imported)
}

/// Parses JSON the way config files are pasted: VS Code's `mcp.json` allows
/// comments and trailing commas, and docs often show one `"name": {...}`
/// entry without the enclosing braces.
fn parse_lenient(text: &str) -> Option<Value> {
    let cleaned = strip_jsonc(text.trim());
    let cleaned = cleaned.trim();
    serde_json::from_str(cleaned).ok().or_else(|| {
        let fragment = cleaned.trim_end_matches(',');
        serde_json::from_str(&format!("{{{fragment}}}")).ok()
    })
}

/// Drops `//` and `/* */` comments and trailing commas outside strings.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            match ch {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.peek().is_some_and(|next| *next != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
            }
            '}' | ']' => {
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

fn collect_servers(
    value: &Value,
    out: &mut Vec<ImportedMcpServer>,
    lift_secrets: bool,
) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            if let Some(servers) = map.get("mcpServers").or_else(|| map.get("servers")) {
                return collect_server_map(servers, out, lift_secrets);
            }
            if map.contains_key("command")
                || map.contains_key("url")
                || map.contains_key("endpoint")
                || map.contains_key("serverUrl")
            {
                let id = map
                    .get("id")
                    .or_else(|| map.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("imported");
                out.push(parse_server(id, value, lift_secrets)?);
                return Ok(());
            }
            collect_server_map(value, out, lift_secrets)
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let fallback = format!("imported-{index}");
                let id = item
                    .get("id")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(&fallback);
                out.push(parse_server(id, item, lift_secrets)?);
            }
            Ok(())
        }
        _ => Err("MCP JSON must be an object or an array".to_owned()),
    }
}

fn collect_server_map(
    value: &Value,
    out: &mut Vec<ImportedMcpServer>,
    lift_secrets: bool,
) -> Result<(), String> {
    let Some(map) = value.as_object() else {
        return Err("mcpServers must be an object".to_owned());
    };
    for (id, spec) in map {
        out.push(parse_server(id, spec, lift_secrets)?);
    }
    Ok(())
}

fn parse_server(
    raw_id: &str,
    spec: &Value,
    lift_secrets: bool,
) -> Result<ImportedMcpServer, String> {
    let id = sanitize_mcp_id(raw_id)
        .ok_or_else(|| format!("server id '{raw_id}' is not a usable name"))?;
    let transport = spec
        .get("transport")
        .or_else(|| spec.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let endpoint = spec
        .get("url")
        .or_else(|| spec.get("endpoint"))
        .or_else(|| spec.get("serverUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let command = spec
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let args: Vec<String> = spec
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let env: BTreeMap<String, String> = spec
        .get("env")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let (api_key, key_header, headers) = header_fields(spec.get("headers"), lift_secrets);
    // Claude Desktop and Cline mark a parked server `"disabled": true`.
    let enabled = spec.get("disabled").and_then(Value::as_bool) != Some(true)
        && spec.get("enabled").and_then(Value::as_bool) != Some(false);
    let is_http = matches!(
        transport,
        "http" | "sse" | "streamable-http" | "streamable_http"
    ) || (endpoint.is_some() && command.is_none());
    if is_http {
        let endpoint = endpoint.ok_or_else(|| format!("server '{id}' is HTTP but has no url"))?;
        if !crate::settings::is_https_url(&endpoint) {
            return Err(format!("server '{id}' url must be a valid https:// URL"));
        }
        return Ok(ImportedMcpServer {
            server: McpServerSettings {
                id,
                enabled,
                transport: "http".to_owned(),
                command: None,
                args: Vec::new(),
                env: BTreeMap::new(),
                endpoint: Some(endpoint),
                key_header: Some(key_header.unwrap_or_else(|| "bearer".to_owned())),
                headers,
            },
            api_key,
        });
    }
    let command = command.ok_or_else(|| format!("server '{id}' is stdio but has no command"))?;
    if crate::settings::bounded_text(&command, crate::settings::MAX_FIELD_BYTES).is_err()
        || !crate::settings::is_mcp_executable(&command)
    {
        return Err(format!(
            "server '{id}' command must be an executable path, not a shell string"
        ));
    }
    if args.len() > crate::settings::MAX_MCP_ARGS
        || args.iter().any(|arg| {
            crate::settings::bounded_text(arg, crate::settings::MAX_FIELD_BYTES).is_err()
        })
    {
        return Err(format!("server '{id}' has invalid or too many args"));
    }
    if env.len() > crate::settings::MAX_MCP_ENV_VARS
        || env.iter().any(|(key, value)| {
            !crate::settings::is_env_var_name(key)
                || crate::settings::bounded_text(value, crate::settings::MAX_FIELD_BYTES).is_err()
        })
    {
        return Err(format!("server '{id}' has invalid or too many env vars"));
    }
    Ok(ImportedMcpServer {
        server: McpServerSettings {
            id,
            enabled,
            transport: "stdio".to_owned(),
            command: Some(command),
            args,
            env,
            endpoint: None,
            key_header: None,
            headers: BTreeMap::new(),
        },
        api_key,
    })
}

/// Splits HTTP headers into a vault key, its header style, and the headers
/// kept on the row.
///
/// A pasted document lifts `Authorization` / `x-api-key` into the key vault.
/// magpie's Library file keeps every header as written.
fn header_fields(
    headers: Option<&Value>,
    lift_secrets: bool,
) -> (Option<String>, Option<String>, BTreeMap<String, String>) {
    let mut key = None;
    let mut style = None;
    let mut kept = BTreeMap::new();
    let Some(headers) = headers.and_then(Value::as_object) else {
        return (key, style, kept);
    };
    for (name, value) in headers {
        let Some(text) = value.as_str() else {
            continue;
        };
        if lift_secrets && key.is_none() && name.eq_ignore_ascii_case("authorization") {
            let candidate = normalize_api_key(text);
            if !candidate.is_empty() {
                key = Some(candidate);
                style = Some("bearer".to_owned());
                continue;
            }
        }
        if lift_secrets && key.is_none() && name.eq_ignore_ascii_case("x-api-key") {
            let candidate = normalize_api_key(text);
            if !candidate.is_empty() {
                key = Some(candidate);
                style = Some("x-api-key".to_owned());
                continue;
            }
        }
        if kept.len() < crate::settings::MAX_MCP_HEADERS
            && crate::settings::is_header_name(name)
            && crate::settings::bounded_text(text, crate::settings::MAX_FIELD_BYTES).is_ok()
        {
            kept.insert(name.clone(), text.to_owned());
        }
    }
    (key, style, kept)
}

fn sanitize_mcp_id(raw: &str) -> Option<String> {
    if crate::home::is_valid_portable_id(raw) {
        return Some(raw.to_owned());
    }
    let mapped: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else if ch == ' ' || ch == '_' || ch == '.' || ch == '-' {
                if ch == ' ' { '-' } else { ch }
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = mapped
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    crate::home::is_valid_portable_id(&collapsed).then_some(collapsed)
}

#[cfg(test)]
mod tests {
    use super::parse_mcp_import;

    #[test]
    fn import_rejects_shell_commands_and_malformed_https_urls() {
        assert!(
            parse_mcp_import(r#"{"mcpServers":{"bad":{"command":"npx -y @scope/pkg"}}}"#).is_err(),
            "shell command string must be rejected at import time"
        );
        assert!(
            parse_mcp_import(r#"{"mcpServers":{"bad":{"url":"https://"}}}"#).is_err(),
            "empty https host must be rejected at import time"
        );
        assert!(
            parse_mcp_import(r#"{"mcpServers":{"bad":{"url":"https://host/path\ninjected"}}}"#)
                .is_err(),
            "URL with newline must be rejected at import time"
        );
        let imported = parse_mcp_import(
            r#"{"mcpServers":{"good":{"command":"npx","args":["-y","@scope/pkg"]}}}"#,
        )
        .expect("valid stdio import");
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].server.id, "good");
    }

    #[test]
    fn import_accepts_jsonc_fragments_and_disabled_rows() {
        let jsonc = r#"{
            // VS Code mcp.json
            "servers": {
                "docs": { "url": "https://mcp.example.com/mcp", /* remote */ },
                "local": { "command": "uvx", "args": ["tool//name"], "disabled": true },
            },
        }"#;
        let imported = parse_mcp_import(jsonc).expect("jsonc import");
        assert_eq!(imported.len(), 2);
        let local = imported
            .iter()
            .find(|row| row.server.id == "local")
            .expect("local");
        assert!(!local.server.enabled);
        assert_eq!(local.server.args, vec!["tool//name".to_owned()]);
        assert!(
            imported
                .iter()
                .any(|row| row.server.id == "docs" && row.server.enabled)
        );

        let fragment = r#""github": { "command": "npx", "args": ["-y", "server-github"] },"#;
        let imported = parse_mcp_import(fragment).expect("fragment import");
        assert_eq!(imported[0].server.id, "github");
    }
}
