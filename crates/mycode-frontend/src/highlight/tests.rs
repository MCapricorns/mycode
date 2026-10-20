use super::{TokenKind, fence_language, highlight};

/// The text and kind of every span, so a test reads like the code it colors.
fn spans<'a>(code: &'a str, lang: &str) -> Vec<(&'a str, TokenKind)> {
    highlight(code, Some(lang))
        .into_iter()
        .map(|(range, kind)| (&code[range], kind))
        .collect()
}

fn kind_of(code: &str, lang: &str, text: &str) -> Option<TokenKind> {
    spans(code, lang)
        .into_iter()
        .find(|(span, _)| *span == text)
        .map(|(_, kind)| kind)
}

#[test]
fn rust_keywords_types_strings_comments_and_macros() {
    let code = "#[derive(Debug)]\npub fn main() -> u32 {\n    // note\n    let s = \"hi \\\" there\";\n    println!(\"{s}\");\n    let c = 'x';\n    0x1F\n}\nfn hold<'a>(v: &'a Vec<u8>) {}";
    use TokenKind::*;
    assert_eq!(kind_of(code, "rust", "#[derive(Debug)]"), Some(Macro));
    assert_eq!(kind_of(code, "rust", "pub"), Some(Keyword));
    assert_eq!(kind_of(code, "rust", "main"), Some(Function));
    assert_eq!(kind_of(code, "rust", "u32"), Some(Type));
    assert_eq!(kind_of(code, "rust", "// note"), Some(Comment));
    assert_eq!(kind_of(code, "rust", "\"hi \\\" there\""), Some(String));
    assert_eq!(kind_of(code, "rust", "println!"), Some(Macro));
    assert_eq!(kind_of(code, "rust", "'x'"), Some(String));
    assert_eq!(kind_of(code, "rust", "0x1F"), Some(Number));
    assert_eq!(kind_of(code, "rust", "'a"), Some(Variable));
    assert_eq!(kind_of(code, "rust", "Vec"), Some(Type));
    assert_eq!(kind_of(code, "rust", "hold"), Some(Function));
}

#[test]
fn rust_raw_and_byte_strings_cover_their_prefix() {
    let code = "let a = r#\"say \"hi\"\"#; let b = b'\\n'; let c = br\"x\";";
    assert_eq!(
        kind_of(code, "rs", "r#\"say \"hi\"\"#"),
        Some(TokenKind::String)
    );
    assert_eq!(kind_of(code, "rs", "b'\\n'"), Some(TokenKind::String));
    assert_eq!(kind_of(code, "rs", "br\"x\""), Some(TokenKind::String));
}

#[test]
fn python_triple_quotes_prefixes_decorators_and_definitions() {
    let code = "@app.route\ndef run(x):\n    \"\"\"Doc\n    more\"\"\"\n    return f\"{x}\" # done\nclass Box: pass\nMAX_SIZE = None";
    use TokenKind::*;
    assert_eq!(kind_of(code, "python", "@app.route"), Some(Macro));
    assert_eq!(kind_of(code, "python", "def"), Some(Keyword));
    assert_eq!(kind_of(code, "python", "run"), Some(Function));
    assert_eq!(
        kind_of(code, "python", "\"\"\"Doc\n    more\"\"\""),
        Some(String)
    );
    assert_eq!(kind_of(code, "python", "f\"{x}\""), Some(String));
    assert_eq!(kind_of(code, "python", "# done"), Some(Comment));
    assert_eq!(kind_of(code, "python", "Box"), Some(Type));
    assert_eq!(kind_of(code, "python", "MAX_SIZE"), Some(Number));
    assert_eq!(kind_of(code, "python", "None"), Some(Number));
}

#[test]
fn shell_variables_flags_and_word_start_comments() {
    let code = "export PATH=\"$HOME/bin\"\nls -la --color=auto $1 ${name} # list\necho a#b";
    use TokenKind::*;
    assert_eq!(kind_of(code, "bash", "export"), Some(Keyword));
    assert_eq!(kind_of(code, "bash", "\"$HOME/bin\""), Some(String));
    assert_eq!(kind_of(code, "bash", "-la"), Some(Property));
    assert_eq!(kind_of(code, "bash", "--color"), Some(Property));
    assert_eq!(kind_of(code, "bash", "$1"), Some(Variable));
    assert_eq!(kind_of(code, "bash", "${name}"), Some(Variable));
    assert_eq!(kind_of(code, "bash", "# list"), Some(Comment));
    assert!(
        spans(code, "bash")
            .iter()
            .all(|(text, _)| !text.contains("#b")),
        "a # inside a word is not a comment"
    );
}

#[test]
fn powershell_keeps_backslash_paths_inside_their_strings() {
    let code = "Get-ChildItem -Path \"C:\\temp\\\" -Recurse | ForEach-Object { $env:PATH }";
    use TokenKind::*;
    assert_eq!(kind_of(code, "pwsh", "\"C:\\temp\\\""), Some(String));
    assert_eq!(kind_of(code, "pwsh", "-Recurse"), Some(Property));
    assert_eq!(kind_of(code, "pwsh", "$env:PATH"), Some(Variable));
}

#[test]
fn json_keys_differ_from_values() {
    let code = "{\"name\": \"mycode\", \"ok\": true, \"n\": 1.5}";
    use TokenKind::*;
    assert_eq!(kind_of(code, "json", "\"name\""), Some(Property));
    assert_eq!(kind_of(code, "json", "\"mycode\""), Some(String));
    assert_eq!(kind_of(code, "json", "true"), Some(Number));
    assert_eq!(kind_of(code, "json", "1.5"), Some(Number));
}

#[test]
fn yaml_and_toml_keys_and_sections() {
    let yaml = "name: ci\non:\n  - push: x\nurl: http://a.b # c";
    use TokenKind::*;
    assert_eq!(kind_of(yaml, "yaml", "name"), Some(Property));
    assert_eq!(kind_of(yaml, "yaml", "push"), Some(Property));
    assert_eq!(kind_of(yaml, "yaml", "url"), Some(Property));
    assert_eq!(kind_of(yaml, "yaml", "# c"), Some(Comment));
    assert!(kind_of(yaml, "yaml", "http").is_none());
    let toml = "[workspace.package]\nversion = \"0.12.7\"\n# pinned";
    assert_eq!(kind_of(toml, "toml", "[workspace.package]"), Some(Type));
    assert_eq!(kind_of(toml, "toml", "version"), Some(Property));
    assert_eq!(kind_of(toml, "toml", "\"0.12.7\""), Some(String));
}

#[test]
fn sql_keywords_ignore_case() {
    let code = "SELECT id FROM users where name = 'it''s' -- tail";
    use TokenKind::*;
    assert_eq!(kind_of(code, "sql", "SELECT"), Some(Keyword));
    assert_eq!(kind_of(code, "sql", "where"), Some(Keyword));
    assert_eq!(kind_of(code, "sql", "-- tail"), Some(Comment));
}

#[test]
fn diff_lines_and_markup_tags() {
    let diff = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same";
    use TokenKind::*;
    assert_eq!(
        spans(diff, "diff"),
        [
            ("--- a/x", Meta),
            ("+++ b/x", Meta),
            ("@@ -1 +1 @@", Meta),
            ("-old", Deleted),
            ("+new", Inserted),
        ]
    );
    let html = "<!-- hi --><a href=\"/x\">link</a>";
    assert_eq!(
        spans(html, "html"),
        [
            ("<!-- hi -->", Comment),
            ("<a", Tag),
            ("href", Property),
            ("\"/x\"", String),
            (">", Tag),
            ("</a", Tag),
            (">", Tag),
        ]
    );
}

#[test]
fn unknown_and_missing_languages_stay_plain() {
    assert!(highlight("don't color 42", None).is_empty());
    assert!(highlight("don't color 42", Some("text")).is_empty());
    assert!(highlight("x", Some("")).is_empty());
}

#[test]
fn fence_info_strings_resolve_to_one_language() {
    assert_eq!(fence_language("Rust,ignore").as_deref(), Some("rust"));
    assert_eq!(
        fence_language("{.python title=x}").as_deref(),
        Some("python")
    );
    assert_eq!(fence_language("  ts  ").as_deref(), Some("ts"));
    assert_eq!(fence_language(""), None);
}

/// Every span sits on UTF-8 boundaries, in order, without overlap, even in
/// code full of CJK text, unclosed strings, and stray markers.
#[test]
fn spans_are_ordered_disjoint_and_on_char_boundaries() {
    let samples = [
        "let 名字 = \"未闭合\n// 注释 ✓\nfn 函数() {}",
        "print('中文 \\",
        "'\u{1F980}' r#\"",
        "$ \"${\n#[",
        "<a 属性=\"值\">文本</a",
        "key: 值\n- 项: x",
    ];
    for lang in [
        "rust", "python", "js", "go", "c", "java", "cs", "swift", "php", "ruby", "lua", "bash",
        "pwsh", "sql", "json", "yaml", "toml", "css", "diff", "html",
    ] {
        for code in samples {
            let mut last = 0;
            for (range, _) in highlight(code, Some(lang)) {
                assert!(
                    range.start >= last && range.start < range.end,
                    "{lang}: {code:?}"
                );
                assert!(code.is_char_boundary(range.start) && code.is_char_boundary(range.end));
                last = range.end;
            }
            assert!(last <= code.len());
        }
    }
}
