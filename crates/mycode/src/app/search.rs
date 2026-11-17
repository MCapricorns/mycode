//! Bounded project-file search backing the composer's `@` mention.

use std::collections::{BinaryHeap, VecDeque};

/// Bounded file index for the composer's `@` mention: a case-insensitive
/// substring match over the session project, skipping dependency and VCS
/// directories, shortest paths first.
pub(crate) fn search_project_files(root: &std::path::Path, query: &str) -> Vec<String> {
    const MAX_VISIT: usize = 8_192;
    const MAX_MATCHES: usize = 8;
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
    // Keep only the best results, but inspect the whole bounded walk. Stopping
    // after the first matching files made filesystem enumeration order hide
    // shorter paths in later directories.
    let mut matches: BinaryHeap<(usize, String)> = BinaryHeap::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_VISIT {
                return finish_mention_matches(matches);
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str())
                    && (SKIP_DIRS.contains(&name) || (name.starts_with('.') && name != ".agents"))
                {
                    continue;
                }
                queue.push_back(path);
            } else if file_type.is_file() {
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let spelling = rel.to_string_lossy().replace('\\', "/");
                if needle.is_empty() || contains_ignore_ascii_case(&spelling, &needle) {
                    let candidate = (spelling.len(), spelling);
                    if matches.len() < MAX_MATCHES {
                        matches.push(candidate);
                    } else if matches.peek().is_some_and(|worst| &candidate < worst) {
                        *matches.peek_mut().expect("the result heap is full") = candidate;
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

/// Shortest-first rendering shared by the walk's exit points.
fn finish_mention_matches(matches: BinaryHeap<(usize, String)>) -> Vec<String> {
    matches
        .into_sorted_vec()
        .into_iter()
        .map(|(_, path)| path)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn shorter_paths_survive_more_than_sixty_four_earlier_matches() {
        let root =
            std::env::temp_dir().join(format!("mycode-mention-ranked-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        for index in 0..100 {
            std::fs::write(
                root.join(format!("long_library_filename_{index:03}.rs")),
                "",
            )
            .unwrap();
        }
        std::fs::write(root.join("src").join("lib.rs"), "").unwrap();
        let found = super::search_project_files(&root, "LIB");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 8);
        assert_eq!(found[0], "src/lib.rs");
        assert_eq!(found[1], "long_library_filename_000.rs");
        assert_eq!(found[7], "long_library_filename_006.rs");
    }

    #[test]
    fn shallow_matches_survive_a_crowded_subtree() {
        let root = std::env::temp_dir().join(format!(
            "mycode-mention-{}-{}",
            std::process::id(),
            crate::core::unix_now().as_nanos()
        ));
        // Sorts after `src`, so a depth-first walk descends here first.
        let deep = root.join("zz").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for index in 0..100 {
            std::fs::write(deep.join(format!("lib_{index}.rs")), "").unwrap();
        }
        std::fs::write(root.join("src").join("lib.rs"), "").unwrap();
        let found = super::search_project_files(&root, "lib");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            found.first().map(String::as_str),
            Some("src/lib.rs"),
            "{found:?}"
        );
    }
}
