//! Bounded project-file search backing the composer's `@` mention.

/// Bounded file index for the composer's `@` mention: a case-insensitive
/// substring match over the session project, skipping dependency and VCS
/// directories, shortest paths first.
pub(crate) fn search_project_files(root: &std::path::Path, query: &str) -> Vec<String> {
    const MAX_VISIT: usize = 8_192;
    const MAX_COLLECT: usize = 64;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        "out",
        ".next",
        ".venv",
        "__pycache__",
        ".mycode",
        "checkpoints",
    ];
    let needle = query.to_ascii_lowercase();
    let mut matches: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_VISIT {
                return finish_mention_matches(matches);
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str())
                    && (SKIP_DIRS.contains(&name) || (name.starts_with('.') && name != ".agents"))
                {
                    continue;
                }
                stack.push(path);
            } else if meta.is_file() {
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let spelling = rel.to_string_lossy().replace('\\', "/");
                if needle.is_empty() || contains_ignore_ascii_case(&spelling, &needle) {
                    matches.push(spelling);
                    if matches.len() >= MAX_COLLECT {
                        return finish_mention_matches(matches);
                    }
                }
            }
        }
    }
    finish_mention_matches(matches)
}

/// ASCII case-insensitive substring test that does not allocate a lowered copy.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.as_bytes();
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.to_ascii_lowercase() == *right)
    })
}

/// Shortest-first truncation shared by the walk's exit points.
fn finish_mention_matches(mut matches: Vec<String>) -> Vec<String> {
    const MAX_MATCHES: usize = 8;
    matches.sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));
    matches.truncate(MAX_MATCHES);
    matches
}
