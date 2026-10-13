//! Project MCP servers from `<project>/.mycode/mcp.json`.

use std::path::Path;

use crate::settings::McpServerSettings;
use crate::{mcp_import::parse_mcp_import, settings::is_mcp_executable};

/// Reads project MCP servers.
///
/// A project with no file returns an empty list. A project whose file is
/// not valid MCP JSON returns an error.
///
/// # Errors
///
/// Returns a message when the project file cannot be read or parsed,
/// or when a stdio command is a shell string.
pub fn project_mcp_servers(project: &Path) -> Result<Vec<McpServerSettings>, String> {
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
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(root.join(".mycode")).unwrap();
        root
    }

    #[test]
    fn a_directory_in_place_of_the_config_errors() {
        let root = scratch("unread");
        std::fs::create_dir(root.join(".mycode").join("mcp.json")).unwrap();
        let error = project_mcp_servers(&root).unwrap_err();
        assert!(
            error.contains("not a file") || error.contains("unreadable"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_returns_its_servers() {
        let root = scratch("servers");
        std::fs::write(
            root.join(".mycode").join("mcp.json"),
            r#"{"mcpServers":{"local":{"command":"local-tool"}}}"#,
        )
        .unwrap();
        let servers = project_mcp_servers(&root).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].command.as_deref(), Some("local-tool"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_json_errors() {
        let root = scratch("bad");
        std::fs::write(root.join(".mycode").join("mcp.json"), "{").unwrap();
        assert!(project_mcp_servers(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
