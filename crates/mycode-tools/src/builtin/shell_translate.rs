//! Conservative bash → PowerShell dialect mapping for the Windows shell.
//!
//! The model often emits POSIX-shell habits (`ls -la`, `rm -rf`, `touch`,
//! `grep -rn`, `which`) even when the runtime shell is PowerShell, and the
//! system prompt's shell naming does not reliably stop that. Rather than a
//! general translator (unbounded and easy to corrupt scripts with), this
//! module rewrites only simple, single commands whose every token is
//! understood. Anything with pipes, redirection, substitution, variables, or
//! an unknown flag is passed through untouched, so the worst case is the
//! error PowerShell already reported before this layer existed.
//!
//! The mapping table follows the published bash↔PowerShell equivalences
//! (Get-ChildItem for `ls -l/-a`, Remove-Item for `rm -r/-f`, Select-String
//! for `grep`, Get-Command for `which`, Get-Content for `head`/`tail`).

/// Result of preparing one command for PowerShell.
pub(crate) struct TranslatedCommand {
    /// The command to run: the cmdlet form when every token was
    /// understood, otherwise the original re-emitted from tokens.
    pub command: String,
    /// Whether a cmdlet mapping or a `2>/dev/null` rewrite changed what
    /// runs. Whitespace-only re-emission is not a rewrite; the caller
    /// surfaces `rewritten` commands to the model so bash habits do not
    /// look like they simply worked.
    pub rewritten: bool,
}

/// Translates well-understood bash one-liners into PowerShell. Everything
/// else is returned unchanged.
#[must_use]
pub(crate) fn translate_bash_to_powershell(command: &str) -> TranslatedCommand {
    let unchanged = || TranslatedCommand {
        command: command.to_owned(),
        rewritten: false,
    };
    let Some(tokens) = tokenize(command) else {
        return unchanged();
    };
    if tokens.is_empty() {
        return unchanged();
    }
    let translated = match tokens[0].as_str() {
        "ls" => translate_ls(&tokens[1..]),
        "rm" => translate_rm(&tokens[1..]),
        "cp" => translate_cp(&tokens[1..]),
        "mkdir" => translate_mkdir(&tokens[1..]),
        "touch" => translate_touch(&tokens[1..]),
        "which" => translate_which(&tokens[1..]),
        "head" | "tail" => translate_head_tail(&tokens[0], &tokens[1..]),
        "wc" => translate_wc(&tokens[1..]),
        "grep" => translate_grep(&tokens[1..]),
        "find" => translate_find(&tokens[1..]),
        _ => None,
    };
    match translated {
        Some(command) => TranslatedCommand {
            command,
            rewritten: true,
        },
        None => TranslatedCommand {
            rewritten: tokens.iter().any(|token| token == "2>/dev/null"),
            command: join(&rewrite_dev_null(&tokens)),
        },
    }
}

/// Splits on whitespace, honoring single- and double-quoted segments.
///
/// `None` means the command has an unclosed quote or carries a shell
/// metacharacter this layer does not reason about (`|`, `&`, `;`, parens,
/// `$`, backtick, or a newline), or a redirect token other than the exact
/// bash `2>/dev/null`: those commands are left exactly as written. Quote
/// characters stay inside their tokens so re-emission keeps the original
/// quoting, and quoted content may carry any character.
fn tokenize(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for character in command.chars() {
        match quote {
            Some(open) => {
                current.push(character);
                if character == open {
                    quote = None;
                }
            }
            None => match character {
                '\'' | '"' => {
                    has_token = true;
                    current.push(character);
                    quote = Some(character);
                }
                c if c.is_whitespace() => {
                    if has_token {
                        tokens.push(std::mem::take(&mut current));
                        has_token = false;
                    }
                }
                '|' | '&' | ';' | '(' | ')' | '$' | '`' | '\n' | '\r' => return None,
                _ => {
                    has_token = true;
                    current.push(character);
                }
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if has_token {
        tokens.push(current);
    }
    // Redirects survive to token level; only the exact `2>/dev/null` token
    // is understood, and only outside quotes.
    for token in &tokens {
        if token != "2>/dev/null" && token_contains_redirect(token) {
            return None;
        }
    }
    Some(tokens)
}

/// Whether an unquoted token carries `<` or `>`. A token wrapped in one
/// matching quote pair is quoted content, whatever it holds.
fn token_contains_redirect(token: &str) -> bool {
    let inner = token
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            token
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    match inner {
        Some(_) => false,
        None => token.contains('<') || token.contains('>'),
    }
}

/// Re-emits tokens as one space-joined line.
fn join(tokens: &[String]) -> String {
    tokens.join(" ")
}

/// Rewrites the exact bash token `2>/dev/null` into PowerShell's `2> $null`.
/// The tokenizer admits the `>` only inside this shape, so nothing else in
/// the command can carry a redirect.
fn rewrite_dev_null(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .flat_map(|token| {
            if token == "2>/dev/null" {
                vec!["2>".to_owned(), "$null".to_owned()]
            } else {
                vec![token.clone()]
            }
        })
        .collect()
}

/// Splits leading `-`-flags from the rest. A `--` long flag or a negative
/// number is not a flag here.
fn split_flags(args: &[String]) -> Option<(String, Vec<String>)> {
    let mut flags = String::new();
    let mut rest = args;
    while let Some(first) = rest.first() {
        if first == "--" {
            return None;
        }
        if let Some(body) = first.strip_prefix('-') {
            if body.is_empty() || body.starts_with(|c: char| c.is_ascii_digit()) {
                break;
            }
            flags.push_str(body);
            rest = &rest[1..];
            continue;
        }
        break;
    }
    Some((flags, rest.to_vec()))
}

/// `ls` with only display flags (`-l -a -h`) maps to `Get-ChildItem -Force`;
/// adding `-r` makes it `-Recurse`. Plain `ls` and path-only forms already
/// work in PowerShell and are not rewritten.
fn translate_ls(args: &[String]) -> Option<String> {
    let (flags, paths) = split_flags(args)?;
    if flags.is_empty() {
        return None;
    }
    let mut recurse = false;
    for flag in flags.chars() {
        match flag {
            'l' | 'a' | 'h' => {}
            'r' | 'R' => recurse = true,
            _ => return None,
        }
    }
    let mut out = String::from("Get-ChildItem -Force");
    if recurse {
        out.push_str(" -Recurse");
    }
    for path in &paths {
        out.push(' ');
        out.push_str(path);
    }
    Some(out)
}

/// `rm -rf …` maps to `Remove-Item -Recurse -Force`. Plain `rm path` already
/// works in PowerShell.
fn translate_rm(args: &[String]) -> Option<String> {
    let (flags, paths) = split_flags(args)?;
    if flags.is_empty() || paths.is_empty() {
        return None;
    }
    let mut recurse = false;
    let mut force = false;
    for flag in flags.chars() {
        match flag {
            'r' | 'R' => recurse = true,
            'f' => force = true,
            _ => return None,
        }
    }
    let mut out = String::from("Remove-Item");
    if recurse {
        out.push_str(" -Recurse");
    }
    if force {
        out.push_str(" -Force");
    }
    for path in &paths {
        out.push(' ');
        out.push_str(path);
    }
    Some(out)
}

/// `cp -r …` maps to `Copy-Item -Recurse`.
fn translate_cp(args: &[String]) -> Option<String> {
    let (flags, paths) = split_flags(args)?;
    if flags.is_empty() || paths.is_empty() {
        return None;
    }
    for flag in flags.chars() {
        if !matches!(flag, 'r' | 'R') {
            return None;
        }
    }
    let mut out = String::from("Copy-Item -Recurse");
    for path in &paths {
        out.push(' ');
        out.push_str(path);
    }
    Some(out)
}

/// `mkdir -p a/b` maps to `New-Item -ItemType Directory -Force`.
fn translate_mkdir(args: &[String]) -> Option<String> {
    let (flags, paths) = split_flags(args)?;
    if flags != "p" || paths.is_empty() {
        return None;
    }
    let mut out = String::from("New-Item -ItemType Directory -Force");
    for path in &paths {
        out.push(' ');
        out.push_str(path);
    }
    Some(out)
}

/// `touch f` maps to `New-Item -ItemType File -Force`.
fn translate_touch(args: &[String]) -> Option<String> {
    let (flags, paths) = split_flags(args)?;
    if !flags.is_empty() || paths.is_empty() {
        return None;
    }
    let mut out = String::from("New-Item -ItemType File -Force");
    for path in &paths {
        out.push(' ');
        out.push_str(path);
    }
    Some(out)
}

/// `which x` maps to `Get-Command x`.
fn translate_which(args: &[String]) -> Option<String> {
    let (flags, names) = split_flags(args)?;
    if !flags.is_empty() || names.is_empty() {
        return None;
    }
    let mut out = String::from("Get-Command");
    for name in &names {
        out.push(' ');
        out.push_str(name);
    }
    Some(out)
}

/// `head -n N f` → `Get-Content f -TotalCount N`; `tail -n N f` →
/// `Get-Content f -Tail N`. The no-flag forms default to 10 lines like the
/// originals. Exactly one path is supported.
fn translate_head_tail(command: &str, args: &[String]) -> Option<String> {
    let (count, rest) = match args.first().map(String::as_str) {
        Some("-n") => {
            let count = args.get(1)?.clone();
            if count.is_empty() || !count.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            (count, args[2..].to_vec())
        }
        Some(flag) if flag.starts_with("-n") && flag.len() > 2 => {
            let count = flag.strip_prefix("-n")?.to_owned();
            if count.is_empty() || !count.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            (count, args[1..].to_vec())
        }
        _ => ("10".to_owned(), args.to_vec()),
    };
    if rest.len() != 1 {
        return None;
    }
    let path = &rest[0];
    let parameter = if command == "head" {
        "-TotalCount"
    } else {
        "-Tail"
    };
    Some(format!("Get-Content {path} {parameter} {count}"))
}

/// `wc -l f` maps to `(Get-Content f).Count`.
fn translate_wc(args: &[String]) -> Option<String> {
    if args.len() != 2 || args[0] != "-l" {
        return None;
    }
    Some(format!("(Get-Content {}).Count", args[1]))
}

/// `grep [-r]…[-i]… pattern path…` maps to `Select-String` (recursive form
/// pipes `Get-ChildItem -Recurse -File` into it, which also searches child
/// directories the way bash `-r` does). `-n` and `-i` are Select-String's
/// own defaults. Only the flag set {r, R, i, n} is understood.
fn translate_grep(args: &[String]) -> Option<String> {
    let (flags, rest) = split_flags(args)?;
    let mut recursive = false;
    for flag in flags.chars() {
        match flag {
            'r' | 'R' | 'i' | 'n' => recursive = recursive || flag == 'r' || flag == 'R',
            _ => return None,
        }
    }
    if rest.len() < 2 {
        return None;
    }
    let pattern = &rest[0];
    let paths = &rest[1..];
    if recursive {
        let list = paths.join(" ");
        Some(format!(
            "Get-ChildItem -Recurse -File {list} | Select-String {pattern}"
        ))
    } else {
        let mut out = format!("Select-String {pattern}");
        for path in paths {
            out.push(' ');
            out.push_str(path);
        }
        Some(out)
    }
}

/// `find path -name glob` maps to `Get-ChildItem -Path path -Recurse -File
/// -Filter glob`. Any other find expression is passed through.
fn translate_find(args: &[String]) -> Option<String> {
    if args.len() != 3 || args[1] != "-name" {
        return None;
    }
    Some(format!(
        "Get-ChildItem -Path {} -Recurse -File -Filter {}",
        args[0], args[2]
    ))
}

#[cfg(test)]
mod tests {
    use super::translate_bash_to_powershell as translate;

    #[test]
    fn common_bash_habits_are_mapped() {
        assert_eq!(translate("ls -la").command, "Get-ChildItem -Force");
        assert_eq!(translate("ls -la .").command, "Get-ChildItem -Force .");
        assert_eq!(
            translate("ls -l -a src").command,
            "Get-ChildItem -Force src"
        );
        assert_eq!(translate("ls -R").command, "Get-ChildItem -Force -Recurse");
        assert_eq!(
            translate("rm -rf target").command,
            "Remove-Item -Recurse -Force target"
        );
        assert_eq!(translate("rm -r a b").command, "Remove-Item -Recurse a b");
        assert_eq!(
            translate("cp -r src dst").command,
            "Copy-Item -Recurse src dst"
        );
        assert_eq!(
            translate("mkdir -p a/b").command,
            "New-Item -ItemType Directory -Force a/b"
        );
        assert_eq!(
            translate("touch notes.txt").command,
            "New-Item -ItemType File -Force notes.txt"
        );
        assert_eq!(translate("which git").command, "Get-Command git");
        assert_eq!(
            translate("head -n 20 log.txt").command,
            "Get-Content log.txt -TotalCount 20"
        );
        assert_eq!(
            translate("tail log.txt").command,
            "Get-Content log.txt -Tail 10"
        );
        assert_eq!(
            translate("wc -l main.rs").command,
            "(Get-Content main.rs).Count"
        );
        assert_eq!(
            translate("grep -rn \"TODO\" src").command,
            "Get-ChildItem -Recurse -File src | Select-String \"TODO\""
        );
        assert_eq!(
            translate("grep -n fixed tools.rs").command,
            "Select-String fixed tools.rs"
        );
        assert_eq!(
            translate("find . -name \"*.rs\"").command,
            "Get-ChildItem -Path . -Recurse -File -Filter \"*.rs\""
        );
    }

    #[test]
    fn dev_null_redirects_become_null_writes() {
        assert_eq!(
            translate("node build.js 2>/dev/null").command,
            "node build.js 2> $null"
        );
    }

    #[test]
    fn powershell_native_and_unknown_forms_pass_through() {
        assert_eq!(
            translate("Get-ChildItem -Force").command,
            "Get-ChildItem -Force"
        );
        assert_eq!(translate("git status").command, "git status");
        assert_eq!(translate("ls").command, "ls");
        assert_eq!(translate("ls src").command, "ls src");
        assert_eq!(translate("echo hi").command, "echo hi");
        // Unknown flags stay for PowerShell to answer.
        assert_eq!(translate("ls -Z").command, "ls -Z");
        assert_eq!(translate("rm -iv x").command, "rm -iv x");
        assert_eq!(translate("grep -v pat file").command, "grep -v pat file");
    }

    #[test]
    fn metacharacters_and_quotes_block_translation() {
        assert_eq!(translate("ls -la | wc -l").command, "ls -la | wc -l");
        assert_eq!(
            translate("grep pat . && echo ok").command,
            "grep pat . && echo ok"
        );
        assert_eq!(translate("echo $HOME").command, "echo $HOME");
        assert_eq!(translate("ls 'unclosed").command, "ls 'unclosed");
        // A pipe inside quotes is content, so this still translates.
        assert_eq!(
            translate("grep -n \"a|b\" file.rs").command,
            "Select-String \"a|b\" file.rs"
        );
    }

    #[test]
    fn rewritten_marks_real_rewrites_not_whitespace_reemission() {
        for rewritten in ["ls -la", "rm -rf target", "node build.js 2>/dev/null"] {
            assert!(translate(rewritten).rewritten, "{rewritten}");
        }
        for unchanged in ["git status", "ls src", "ls -Z"] {
            assert!(!translate(unchanged).rewritten, "{unchanged}");
        }
        // Collapsed whitespace changes the executed string but is not a
        // rewrite the model needs to hear about.
        assert_eq!(translate("ls  src").command, "ls src");
        assert!(!translate("ls  src").rewritten);
    }
}
