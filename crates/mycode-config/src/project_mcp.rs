//! MCP servers from a project's `.mycode/mcp.json` and from the magpie
//! Library's `~/.mycode/mcp.json`.

use std::path::Path;

use crate::mcp_import::parse_mcp_library;
use crate::settings::McpServerSettings;

/// Largest project MCP document read; a pasted config is a few KiB.
const MAX_PROJECT_MCP_BYTES: u64 = 256 * 1024;

/// Reads project MCP servers.
///
/// A project with no file returns an empty list. A project whose file is
/// not valid MCP JSON returns an error.
///
/// # Errors
///
/// Returns a message when the project file cannot be read or parsed,
/// is oversized, or a stdio command is a shell string.
pub fn project_mcp_servers(project: &Path) -> Result<Vec<McpServerSettings>, String> {
    read_mcp_file(&project.join(".mycode").join("mcp.json"), "project")
}

/// Reads the MCP servers magpie's Library writes at `~/.mycode/mcp.json`.
///
/// The document is `{"mcpServers": {…}}` (`mcpFormat: "claude"`), and its
/// headers stay on the row: magpie owns the file and keeps vendor keys there.
///
/// # Errors
///
/// Returns a message when the file cannot be read or parsed, or is oversized.
pub fn global_mcp_servers(home: &crate::HomeLayout) -> Result<Vec<McpServerSettings>, String> {
    read_mcp_file(&home.root().join("mcp.json"), "magpie")
}

fn read_mcp_file(file: &Path, label: &str) -> Result<Vec<McpServerSettings>, String> {
    let metadata = match std::fs::metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{label} MCP config unreadable: {error}")),
    };
    if !metadata.is_file() {
        return Err(format!("{label} MCP config is not a file"));
    }
    if metadata.len() > MAX_PROJECT_MCP_BYTES {
        return Err(format!("{label} MCP config is larger than 256 KiB"));
    }
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("{label} MCP config unreadable: {error}"))?;
    // The import parser already refuses a stdio command that is a shell
    // string, so every row it returns is safe to spawn.
    Ok(parse_mcp_library(&text)?
        .into_iter()
        .map(|server| server.server)
        .collect())
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
