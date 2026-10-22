use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{GrepArgs, GrepTool};
use crate::builtin::fs_search::SearchAccess;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::Tool;

#[tokio::test]
async fn grep_finds_lf_text_without_an_external_rg() {
    let root = std::env::temp_dir().join(format!(
        "mycode-grep-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(root.join("sub")).expect("dir");
    std::fs::write(root.join("sub/a.txt"), "alpha needle\nsecond line\n").expect("file");
    let prepared = crate::builtin::fs_search::prepare_search_async_with_access(
        root.clone(),
        None,
        CancellationToken::new(),
        SearchAccess::Content,
    )
    .await
    .expect("prepare");
    let ctx = ToolCtx::new(&root).with_prepared_search(Arc::new(prepared));
    let (mut out, _rx) = ToolStream::channel();
    let result = GrepTool
        .execute(
            GrepArgs {
                pattern: "needle".to_owned(),
                is_regex: false,
                path: Some("sub".to_owned()),
                include: None,
                exclude: None,
                max_results: None,
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("grep");
    let text: String = result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_providers::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!result.is_error, "grep failed on LF files: {text}");
    assert!(text.contains("needle"), "expected the LF match, got {text}");
    let _ = std::fs::remove_dir_all(root);
}

/// A `|`/`.*` pattern sent without `is_regex` is also tried as a regex
/// when that reading compiles and does not match the empty string. The
/// report must say so, instead of leaving a wrong "the file is empty"
/// conclusion.
#[tokio::test]
async fn a_regex_looking_literal_pattern_gets_a_hint() {
    let root = std::env::temp_dir().join(format!(
        "mycode-grep-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("dir");
    std::fs::write(root.join("a.txt"), "alpha needle\nsecond line\n").expect("file");
    // The prepared search handle is consumed per execute, so each call
    // builds its own.
    let run = |pattern: &str, is_regex: bool, path: &str| {
        let root = root.clone();
        let pattern = pattern.to_owned();
        let path = path.to_owned();
        async move {
            let prepared = crate::builtin::fs_search::prepare_search_async_with_access(
                root.clone(),
                None,
                CancellationToken::new(),
                SearchAccess::Content,
            )
            .await
            .expect("prepare");
            let ctx = ToolCtx::new(&root).with_prepared_search(Arc::new(prepared));
            let (mut out, _rx) = ToolStream::channel();
            GrepTool
                .execute(
                    GrepArgs {
                        pattern,
                        is_regex,
                        path: Some(path),
                        include: None,
                        exclude: None,
                        max_results: None,
                    },
                    &ctx,
                    &mut out,
                )
                .await
                .expect("grep")
        }
    };
    let text = |result: &crate::tool::ToolResult| {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                mycode_providers::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<String>()
    };

    // A regex-looking literal is matched as a regex too, and says so.
    let literal = run("alpha|needle", false, "a.txt").await;
    let literal_text = text(&literal);
    assert!(literal_text.contains("alpha needle"), "{literal_text}");
    assert!(literal_text.contains("is_regex"), "{literal_text}");

    let regex = run("alpha|needle", true, "a.txt").await;
    let regex_text = text(&regex);
    assert!(regex_text.contains("alpha needle"), "{regex_text}");
    assert!(!regex_text.contains("is_regex"), "{regex_text}");

    // A pattern whose regex reading matches the empty string stays
    // literal: `a || b` as alternation would match every line.
    std::fs::write(root.join("b.txt"), "a || b\nplain\n").expect("file b");
    let hit = run("a || b", false, "b.txt").await;
    let hit_text = text(&hit);
    assert!(hit_text.contains("a || b"), "{hit_text}");
    assert!(!hit_text.contains("plain"), "{hit_text}");
    assert!(!hit_text.contains("is_regex"), "{hit_text}");

    // Nothing found still says the search ran.
    let miss = run("absent", false, "a.txt").await;
    assert!(
        text(&miss).starts_with("No matching lines found."),
        "{}",
        text(&miss)
    );
    let _ = std::fs::remove_dir_all(root);
}

fn grep_args(pattern: &str) -> GrepArgs {
    GrepArgs {
        pattern: pattern.to_owned(),
        is_regex: false,
        path: None,
        include: None,
        exclude: None,
        max_results: None,
    }
}

async fn grep_text(root: &std::path::Path, args: GrepArgs) -> String {
    let prepared = crate::builtin::fs_search::prepare_search_async_with_access(
        root.to_path_buf(),
        args.path.clone(),
        CancellationToken::new(),
        SearchAccess::Content,
    )
    .await
    .expect("prepare");
    let ctx = ToolCtx::new(root).with_prepared_search(Arc::new(prepared));
    let (mut out, _rx) = ToolStream::channel();
    let result = GrepTool.execute(args, &ctx, &mut out).await.expect("grep");
    result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_providers::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// The ways a model asks for text that is there: a bare file name as
/// `include`, a comma list, a lowercase pattern for a capitalized word,
/// and an explicit hidden path.
#[tokio::test]
async fn common_model_spellings_find_the_text() {
    let root = std::env::temp_dir().join(format!(
        "mycode-grep-spell-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(root.join("src/deep")).expect("dir");
    std::fs::create_dir_all(root.join(".github")).expect("hidden dir");
    std::fs::write(root.join("src/deep/main.rs"), "fn Needle() {}\n").expect("rs");
    std::fs::write(root.join("Cargo.toml"), "needle = 1\n").expect("toml");
    std::fs::write(root.join(".github/ci.yml"), "needle\n").expect("yml");

    let text = grep_text(
        &root,
        GrepArgs {
            include: Some("main.rs".to_owned()),
            ..grep_args("Needle")
        },
    )
    .await;
    assert!(text.contains("src/deep/main.rs:1:"), "{text}");

    let text = grep_text(
        &root,
        GrepArgs {
            include: Some("*.rs, *.toml".to_owned()),
            ..grep_args("needle")
        },
    )
    .await;
    assert!(text.contains("src/deep/main.rs:1:"), "smart case: {text}");
    assert!(text.contains("Cargo.toml:1:"), "{text}");

    // An uppercase letter makes the match exact.
    let text = grep_text(&root, grep_args("NEEDLE")).await;
    assert!(text.starts_with("No matching lines found."), "{text}");

    let text = grep_text(
        &root,
        GrepArgs {
            path: Some(".github".to_owned()),
            ..grep_args("needle")
        },
    )
    .await;
    assert!(text.contains("hidden or gitignored"), "{text}");
    let _ = std::fs::remove_dir_all(root);
}
