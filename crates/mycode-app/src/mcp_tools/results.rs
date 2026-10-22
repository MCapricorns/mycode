//! MCP result handling: the one-line snippet and server summary the system
//! prompt carries, the byte bound a result keeps inline, and the full-text
//! store beside the home.

/// Longest description summary carried into the system-prompt tool list.
const MCP_SNIPPET_CHARS: usize = 160;

/// One prompt-list line: what the tool does (the first sentence of its
/// description) and which server serves it. The full description stays in
/// the tool spec.
pub(super) fn mcp_snippet(server_id: &str, description: Option<&str>) -> String {
    let summary = description
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|text| match text.find(". ") {
            Some(end) => text[..=end].to_owned(),
            None => text,
        })
        .filter(|text| !text.is_empty());
    match summary {
        Some(summary) => {
            let mut summary: String = summary.chars().take(MCP_SNIPPET_CHARS).collect();
            if summary.chars().count() == MCP_SNIPPET_CHARS {
                summary.push('\u{2026}');
            }
            format!("{summary} (MCP server '{server_id}')")
        }
        None => format!("MCP tool on server '{server_id}'."),
    }
}

/// Bytes of an MCP tool result kept inline for the model.
pub(super) const MAX_MCP_TOOL_RESULT_BYTES: usize = 20 * 1024;

/// Age after which a stored full-text MCP result is deleted, so
/// `mcp-results` cannot grow forever.
const MCP_RESULT_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Removes stored results older than [`MCP_RESULT_RETENTION`]. Best-effort:
/// a file that cannot be inspected or removed stays for the next sweep.
fn prune_stale_mcp_results(dir: &std::path::Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("mcp-") || !name.ends_with(".txt") {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > MCP_RESULT_RETENTION);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Truncates oversized tool text and stores the full body beside the home.
pub(crate) fn bound_mcp_result(dir: &std::path::Path, text: &str) -> String {
    if text.len() <= MAX_MCP_TOOL_RESULT_BYTES {
        return text.to_owned();
    }
    prune_stale_mcp_results(dir, std::time::SystemTime::now());
    let id = format!(
        "mcp-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    );
    let path = dir.join(format!("{id}.txt"));
    let stored = match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, text)) {
        Ok(()) => format!("full text stored at {}", path.display()),
        Err(error) => format!("full text could not be stored: {error}"),
    };
    let head_end = text.floor_char_boundary(MAX_MCP_TOOL_RESULT_BYTES / 2);
    let tail_start =
        text.ceil_char_boundary(text.len().saturating_sub(MAX_MCP_TOOL_RESULT_BYTES / 2));
    format!(
        "{}\n\n[truncated; {stored}]\n\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

/// One line for the prompt's server list: the first sentence of the
/// instructions a server sends in the handshake.
///
/// `None` when the server sent no usable text. The server is untrusted, so
/// the line is flattened, bounded, and never spans lines.
pub(super) fn mcp_server_summary(instructions: &str) -> Option<String> {
    let text = instructions
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    let sentence = match text.find(". ") {
        Some(end) => &text[..=end],
        None => text.as_str(),
    };
    let mut summary: String = sentence.chars().take(MCP_SNIPPET_CHARS).collect();
    if summary.chars().count() == MCP_SNIPPET_CHARS {
        summary.push('\u{2026}');
    }
    Some(summary)
}
