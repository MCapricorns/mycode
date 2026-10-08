//! Project MCP servers from `<project>/.mycode/mcp.json`.
//!
//! The file is not opened until the project is trusted. Opening a folder
//! does not trust it.

use std::path::Path;

use crate::settings::McpServerSettings;
use crate::{mcp_import::parse_mcp_import, settings::is_mcp_executable};

/// Reads project MCP servers when `trusted` is true.
///
/// An untrusted project returns an empty list and does not open
/// `.mycode/mcp.json`. A trusted project with no file returns an empty list.
/// A trusted project whose file is not valid MCP JSON returns an error.
///
/// # Errors
///
/// Returns a message when a trusted project file cannot be read or parsed,
/// or when a stdio command is a shell string.
pub fn project_mcp_servers(
    project: &Path,
    trusted: bool,
) -> Result<Vec<McpServerSettings>, String> {
    if !trusted {
        return Ok(Vec::new());
    }
    let file = project.join(".mycode").join("mcp.json");
    if !file.is_file() {
        if file.exists() {
            return Err("project MCP config is not a file".to_owned());
        }
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|error| format!("project MCP config unreadable: {error}"))?;
    let imported = parse_mcp_import(&text)?;
    for server in &imported {
        if server.server.transport == "stdio"
            && !server
                .server
                .command
                .as_deref()
                .is_some_and(is_mcp_executable)
        {
            return Err(format!(
                "project MCP server '{}' command must be an executable, not a shell string",
                server.server.id
            ));
        }
    }
    Ok(imported.into_iter().map(|server| server.server).collect())
}

#[cfg(test)]
mod tests {
    use super::project_mcp_servers;

    fn scratch(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mycode-project-mcp-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(root.join(".mycode")).unwrap();
        root
    }

    #[test]
    fn untrusted_project_does_not_read_mcp_config() {
        let root = scratch("unread");
        std::fs::create_dir(root.join(".mycode").join("mcp.json")).unwrap();
        assert!(project_mcp_servers(&root, false).unwrap().is_empty());
        let error = project_mcp_servers(&root, true).unwrap_err();
        assert!(
            error.contains("not a file") || error.contains("unreadable"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trusted_project_returns_servers() {
        let root = scratch("trusted");
        std::fs::write(
            root.join(".mycode").join("mcp.json"),
            r#"{"mcpServers":{"local":{"command":"local-tool"}}}"#,
        )
        .unwrap();
        let servers = project_mcp_servers(&root, true).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].command.as_deref(), Some("local-tool"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trusted_invalid_json_errors() {
        let root = scratch("bad");
        std::fs::write(root.join(".mycode").join("mcp.json"), "{").unwrap();
        assert!(project_mcp_servers(&root, true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
