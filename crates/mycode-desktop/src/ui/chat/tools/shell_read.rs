//! The file a shell call only prints, so its output can be highlighted like
//! a `read` result.
//!
//! The model sometimes reads with `Get-Content`, `cat`, `head`, or `sed -n`
//! instead of the `read` tool. The command is the only language hint: one
//! reader naming exactly one file, optionally piped through line filters.
//! Anything else (redirects, chained commands, other pipeline stages) keeps
//! the plain rendering.

/// Commands whose output is the named file's text.
const READERS: &[&str] = &["get-content", "gc", "cat", "type", "head", "tail", "sed"];

/// Pipeline stages that only drop lines, so the output stays file text.
const FILTERS: &[&str] = &["select-object", "select", "head", "tail"];

/// The path a read-only shell `command` prints, if it is one.
pub(super) fn shell_read_path(command: &str) -> Option<String> {
    let command = command.trim();
    if [";", "&&", "||", ">", "<"]
        .iter()
        .any(|marker| command.contains(marker))
    {
        return None;
    }
    let mut stages = command.split('|');
    let reader = stages.next()?;
    for stage in stages {
        let name = words(stage).into_iter().next()?;
        if !FILTERS.contains(&name.to_ascii_lowercase().as_str()) {
            return None;
        }
    }
    let mut tokens = words(reader).into_iter();
    let first = tokens.next()?;
    // `(Get-Content x.rs)[0..50]` indexes lines of the same file.
    let grouped = first.starts_with('(');
    let name = first.trim_start_matches('(').to_ascii_lowercase();
    if !READERS.contains(&name.as_str()) {
        return None;
    }
    let mut paths = tokens
        .filter(|token| !token.starts_with('-'))
        .map(|token| {
            if grouped {
                token.split(')').next().unwrap_or_default().to_owned()
            } else {
                token
            }
        })
        .filter(|token| std::path::Path::new(token).extension().is_some());
    let path = paths.next()?;
    paths.next().is_none().then_some(path)
}

/// Whitespace-separated words with `'…'` and `"…"` quoting removed.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in text.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::shell_read_path;

    #[test]
    fn powershell_reads_name_their_file() {
        for (command, path) in [
            (r"Get-Content D:\src\main.rs", r"D:\src\main.rs"),
            (
                r#"Get-Content -Path "C:\My Files\lib.rs" -Raw"#,
                r"C:\My Files\lib.rs",
            ),
            ("Get-Content src/app.ts -TotalCount 200", "src/app.ts"),
            ("Get-Content -LiteralPath 'a.py' -Encoding UTF8", "a.py"),
            (
                "Get-Content src/lib.rs | Select-Object -Skip 100 -First 50",
                "src/lib.rs",
            ),
            ("(Get-Content 'src/lib.rs')[10..40]", "src/lib.rs"),
            ("gc Cargo.toml", "Cargo.toml"),
            ("type README.md", "README.md"),
        ] {
            assert_eq!(shell_read_path(command).as_deref(), Some(path), "{command}");
        }
    }

    #[test]
    fn posix_reads_name_their_file() {
        for (command, path) in [
            ("cat src/main.rs", "src/main.rs"),
            ("head -n 80 src/main.rs", "src/main.rs"),
            ("tail -20 build.gradle", "build.gradle"),
            (
                "sed -n '120,200p' crates/app/src/lib.rs",
                "crates/app/src/lib.rs",
            ),
            ("cat src/main.rs | head -50", "src/main.rs"),
        ] {
            assert_eq!(shell_read_path(command).as_deref(), Some(path), "{command}");
        }
    }

    #[test]
    fn other_commands_keep_plain_output() {
        for command in [
            "cargo test",
            "Get-ChildItem src",
            "cat a.rs b.rs",
            "cat notes",
            "Get-Content a.rs | ForEach-Object { $_.Length }",
            "Get-Content a.rs | Out-File b.rs",
            "cat a.rs > b.rs",
            "cat a.rs; cargo build",
            "git show HEAD:src/main.rs",
        ] {
            assert_eq!(shell_read_path(command), None, "{command}");
        }
    }
}
