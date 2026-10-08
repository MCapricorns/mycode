//! Parse a pasted MCP config document into server rows.
//!
//! Accepts Claude Desktop / Cursor `mcpServers` maps, VS Code `servers`
//! maps, a single server object, or an array of those objects. HTTP
//! `Authorization` / `x-api-key` header values are lifted into the vault
//! key; a leading `Bearer ` is stripped so the user can paste either form.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::settings::McpServerSettings;

/// One imported server plus an optional credential lifted from headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedMcpServer {
    /// The settings row to add.
    pub server: McpServerSettings,
    /// Vault key, already run through [`normalize_api_key`].
    pub api_key: Option<String>,
}

/// Strips a leading `Authorization:` / `Bearer ` so the user can paste a
/// raw key or a header value. The wire layer always prefixes `Bearer `.
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
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| "paste is not valid JSON".to_owned())?;
    let mut imported = Vec::new();
    collect_servers(&value, &mut imported)?;
    if imported.is_empty() {
        return Err("no MCP servers found in the pasted JSON".to_owned());
    }
    if imported.len() > crate::settings::MAX_MCP_SERVERS {
        return Err("too many MCP servers in the pasted JSON".to_owned());
    }
    Ok(imported)
}

fn collect_servers(value: &Value, out: &mut Vec<ImportedMcpServer>) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            if let Some(servers) = map.get("mcpServers").or_else(|| map.get("servers")) {
                return collect_server_map(servers, out);
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
                out.push(parse_server(id, value)?);
                return Ok(());
            }
            collect_server_map(value, out)
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let fallback = format!("imported-{index}");
                let id = item
                    .get("id")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(&fallback);
                out.push(parse_server(id, item)?);
            }
            Ok(())
        }
        _ => Err("MCP JSON must be an object or an array".to_owned()),
    }
}

fn collect_server_map(value: &Value, out: &mut Vec<ImportedMcpServer>) -> Result<(), String> {
    let Some(map) = value.as_object() else {
        return Err("mcpServers must be an object".to_owned());
    };
    for (id, spec) in map {
        out.push(parse_server(id, spec)?);
    }
    Ok(())
}

fn parse_server(raw_id: &str, spec: &Value) -> Result<ImportedMcpServer, String> {
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
    let args = spec
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
    let env = spec
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
    let (api_key, key_header) = lift_header_key(spec.get("headers"));
    let is_http = matches!(
        transport,
        "http" | "sse" | "streamable-http" | "streamable_http"
    ) || (endpoint.is_some() && command.is_none());
    if is_http {
        let endpoint = endpoint.ok_or_else(|| format!("server '{id}' is HTTP but has no url"))?;
        if !endpoint.starts_with("https://") {
            return Err(format!("server '{id}' url must be https://"));
        }
        return Ok(ImportedMcpServer {
            server: McpServerSettings {
                id,
                enabled: true,
                transport: "http".to_owned(),
                command: None,
                args: Vec::new(),
                env: BTreeMap::new(),
                endpoint: Some(endpoint),
                key_header: Some(key_header.unwrap_or_else(|| "bearer".to_owned())),
            },
            api_key,
        });
    }
    let command = command.ok_or_else(|| format!("server '{id}' is stdio but has no command"))?;
    Ok(ImportedMcpServer {
        server: McpServerSettings {
            id,
            enabled: true,
            transport: "stdio".to_owned(),
            command: Some(command),
            args,
            env,
            endpoint: None,
            key_header: None,
        },
        api_key,
    })
}

fn lift_header_key(headers: Option<&Value>) -> (Option<String>, Option<String>) {
    let Some(headers) = headers.and_then(Value::as_object) else {
        return (None, None);
    };
    for (name, value) in headers {
        let Some(text) = value.as_str() else {
            continue;
        };
        if name.eq_ignore_ascii_case("authorization") {
            let key = normalize_api_key(text);
            if !key.is_empty() {
                return (Some(key), Some("bearer".to_owned()));
            }
        }
        if name.eq_ignore_ascii_case("x-api-key") {
            let key = normalize_api_key(text);
            if !key.is_empty() {
                return (Some(key), Some("x-api-key".to_owned()));
            }
        }
    }
    (None, None)
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
