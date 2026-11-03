//! MCP servers from magpie's Library file `~/.mycode/mcp.json`.

use std::path::Path;

use crate::config::mcp_import::parse_mcp_library;
use crate::config::settings::McpServerSettings;

/// Largest Library MCP document read; a pasted config is a few KiB.
const MAX_PROJECT_MCP_BYTES: u64 = 256 * 1024;

/// Reads the MCP servers magpie's Library writes at `~/.mycode/mcp.json`.
///
/// The document is `{"mcpServers": {…}}` (`mcpFormat: "claude"`), and its
/// headers stay on the row: magpie owns the file and keeps vendor keys there.
///
/// # Errors
///
/// Returns a message when the file cannot be read or parsed, or is oversized.
pub fn global_mcp_servers(
    home: &crate::config::HomeLayout,
) -> Result<Vec<McpServerSettings>, String> {
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
    // Bound the actual read too: the Library writer can replace or grow the
    // file after the metadata check. Never allocate an unbounded document.
    let file = std::fs::File::open(file)
        .map_err(|error| format!("{label} MCP config unreadable: {error}"))?;
    let text = read_mcp_text(file, label)?;
    // The import parser already refuses a stdio command that is a shell
    // string, so every row it returns is safe to spawn.
    Ok(parse_mcp_library(&text)?
        .into_iter()
        .map(|server| server.server)
        .collect())
}

fn read_mcp_text(reader: impl std::io::Read, label: &str) -> Result<String, String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    reader
        .take(MAX_PROJECT_MCP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{label} MCP config unreadable: {error}"))?;
    if bytes.len() as u64 > MAX_PROJECT_MCP_BYTES {
        return Err(format!("{label} MCP config is larger than 256 KiB"));
    }
    String::from_utf8(bytes).map_err(|_| format!("{label} MCP config is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::global_mcp_servers;
    use crate::config::HomeLayout;

    fn scratch(label: &str) -> (std::path::PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-library-mcp-{label}-{}-{}",
            std::process::id(),
            crate::core::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let home = HomeLayout::from_root(&root).unwrap();
        (root, home)
    }

    #[test]
    fn actual_mcp_reads_are_bounded_even_when_metadata_is_stale() {
        use std::io::Read as _;
        // This source never reaches EOF; the read must stop at cap + 1.
        let mut source = std::io::repeat(b' ').take(u64::MAX);
        let error = super::read_mcp_text(&mut source, "magpie").unwrap_err();
        assert!(error.contains("larger than 256 KiB"));
        assert_eq!(source.limit(), u64::MAX - super::MAX_PROJECT_MCP_BYTES - 1);
        let text = " ".repeat(super::MAX_PROJECT_MCP_BYTES as usize);
        assert_eq!(
            super::read_mcp_text(text.as_bytes(), "magpie").unwrap(),
            text
        );
    }

    #[test]
    fn a_directory_in_place_of_the_config_errors() {
        let (root, home) = scratch("unread");
        std::fs::create_dir(root.join("mcp.json")).unwrap();
        let error = global_mcp_servers(&home).unwrap_err();
        assert!(
            error.contains("not a file") || error.contains("unreadable"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_library_returns_its_servers() {
        let (root, home) = scratch("servers");
        std::fs::write(
            root.join("mcp.json"),
            r#"{"mcpServers":{"local":{"command":"local-tool"}}}"#,
        )
        .unwrap();
        let servers = global_mcp_servers(&home).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].command.as_deref(), Some("local-tool"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_json_errors() {
        let (root, home) = scratch("bad");
        std::fs::write(root.join("mcp.json"), "{").unwrap();
        assert!(global_mcp_servers(&home).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
