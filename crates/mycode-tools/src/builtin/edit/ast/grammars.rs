//! Statically linked tree-sitter grammar table for AST edits.
//!
//! Grammars used to be resolved through `gpui-kit`'s `LanguageRegistry`,
//! which pulled the whole GPUI stack (and with it the smol runtime) into
//! every binary linking `mycode-tools`. The registry only wrapped the
//! grammar crates below, so this table links the same crates directly and
//! keeps the grammar set and alias behavior identical. Alias and grammar
//! arms mirror `gpui-component` 0.7.1 `tree-sitter-languages`.
//!
//! Version requirements in `Cargo.toml` match `gpui-component` 0.7.1, so a
//! `mycode-desktop` build unifies with its copies of these crates instead of
//! compiling each grammar twice.

/// Maps a language token (name, alias, or file extension) onto the canonical
/// name of a linked grammar. Plain-text tokens and unknown names return
/// `None`.
pub(super) fn grammar_name(token: &str) -> Option<&'static str> {
    Some(match token {
        "json" | "jsonc" => "json",
        "astro" => "astro",
        "bash" | "sh" => "bash",
        "c" => "c",
        "cmake" => "cmake",
        "cpp" | "c++" => "cpp",
        "csharp" | "cs" => "csharp",
        "css" | "scss" => "css",
        "diff" => "diff",
        "ejs" => "ejs",
        "elixir" | "ex" => "elixir",
        "erb" => "erb",
        "go" => "go",
        "graphql" => "graphql",
        "html" => "html",
        "java" => "java",
        "javascript" | "js" => "javascript",
        "jsdoc" => "jsdoc",
        "kt" | "kts" | "ktm" | "kotlin" => "kotlin",
        "lua" => "lua",
        "make" | "makefile" => "make",
        "markdown" | "md" | "mdx" => "markdown",
        "markdown_inline" | "markdown-inline" => "markdown_inline",
        "php" | "php3" | "php4" | "php5" | "phtml" => "php",
        "proto" | "protobuf" => "proto",
        "python" | "py" => "python",
        "ruby" | "rb" => "ruby",
        "rust" | "rs" => "rust",
        "scala" => "scala",
        "sql" => "sql",
        "svelte" => "svelte",
        "swift" => "swift",
        "toml" => "toml",
        "tsx" => "tsx",
        "typescript" | "ts" => "typescript",
        "yaml" | "yml" => "yaml",
        "zig" => "zig",
        _ => return None,
    })
}

/// Returns the statically linked grammar registered under a canonical
/// [`grammar_name`] name.
pub(super) fn grammar_language(name: &str) -> Option<tree_sitter::Language> {
    Some(match name {
        "astro" => tree_sitter_astro_next::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cmake" => tree_sitter_cmake::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "diff" => tree_sitter_diff::LANGUAGE.into(),
        "ejs" | "erb" => tree_sitter_embedded_template::LANGUAGE.into(),
        "elixir" => tree_sitter_elixir::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "graphql" => tree_sitter_graphql::LANGUAGE.into(),
        "html" => tree_sitter_html::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "jsdoc" => tree_sitter_jsdoc::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_sg::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "make" => tree_sitter_make::LANGUAGE.into(),
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        "markdown_inline" => tree_sitter_md::INLINE_LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "proto" => tree_sitter_proto::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "sql" => tree_sitter_sequel::LANGUAGE.into(),
        "svelte" => tree_sitter_svelte_next::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "yaml" => tree_sitter_yaml::LANGUAGE.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_resolves_a_linked_grammar() {
        let tokens = [
            "json", "jsonc", "astro", "bash", "sh", "c", "cmake", "cpp", "c++", "csharp", "cs",
            "css", "scss", "diff", "ejs", "elixir", "ex", "erb", "go", "graphql", "html", "java",
            "javascript", "js", "jsdoc", "kt", "kts", "ktm", "kotlin", "lua", "make", "makefile",
            "markdown", "md", "mdx", "markdown_inline", "markdown-inline", "php", "proto",
            "protobuf", "python", "py", "ruby", "rb", "rust", "rs", "scala", "sql", "svelte",
            "swift", "toml", "tsx", "typescript", "ts", "yaml", "yml", "zig",
        ];
        for token in tokens {
            let name = grammar_name(token)
                .unwrap_or_else(|| panic!("token {token:?} must resolve to a grammar name"));
            assert!(
                grammar_language(name).is_some(),
                "grammar {name:?} must be linked"
            );
        }
    }

    #[test]
    fn plain_text_and_unknown_tokens_have_no_grammar() {
        for token in ["text", "plain", "plaintext", "", "Rust", "unknown"] {
            assert_eq!(grammar_name(token), None, "token {token:?}");
        }
        assert_eq!(grammar_language("text"), None);
        assert_eq!(grammar_language(""), None);
    }
}
