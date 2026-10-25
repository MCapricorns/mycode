use super::{
    code_call_count, diff_counts, diff_line_text, read_path, shows_diff_preview, split_code_calls,
    tool_batch_summary, tool_call_parts, tool_dot_color, tool_status,
};
use crate::view_model::{ConversationEntry, EntryKind};

fn result_entry(text: &str) -> ConversationEntry {
    ConversationEntry {
        event_id: "evt".into(),
        kind: EntryKind::ToolResult,
        text: text.into(),
        call_id: Some("call".into()),
        thinking: String::new(),
    }
}

/// Eleven markdown list lines are what QA saw counted as deletions.
fn eleven_list_lines(header: &str) -> String {
    use std::fmt::Write as _;
    let mut page = format!("{header}\n");
    for n in 1..=11 {
        let _ = writeln!(page, "- line {n}");
    }
    page
}

/// String parsing only: the fixtures name no real file, and the test never
/// touches the filesystem, so it runs the same on any machine.
#[test]
fn read_path_strips_only_a_trailing_line_window() {
    assert_eq!(read_path("src/fluxlog.cpp"), "src/fluxlog.cpp");
    assert_eq!(read_path("src/fluxlog.cpp L520+620"), "src/fluxlog.cpp");
    assert_eq!(read_path("src/fluxlog.h +200"), "src/fluxlog.h");
    assert_eq!(read_path("src/fluxlog.h L520"), "src/fluxlog.h");
    assert_eq!(read_path("src/my file.cpp L520+620"), "src/my file.cpp");
    assert_eq!(read_path("\"src/my file.rs\""), "src/my file.rs");
}

#[test]
fn fetch_content_list_lines_are_not_a_diff() {
    let page = eleven_list_lines("[https://example.com/a]");
    let status = tool_status("fetch_content", Some(&result_entry(&page)), false);
    assert_eq!(status, "");
    assert!(!shows_diff_preview("fetch_content", &page));
}

#[test]
fn web_search_snippets_are_not_a_diff() {
    let text = "[https://example.com] Docs\n- alpha\n- beta\n";
    let status = tool_status("web_search", Some(&result_entry(text)), false);
    assert_eq!(status, "");
    assert!(!shows_diff_preview("web_search", text));
}

#[test]
fn agent_and_scout_markdown_is_not_a_diff() {
    use std::fmt::Write as _;
    let mut text = String::from("scout notes\n");
    for n in 1..=21 {
        let _ = writeln!(text, "- line {n}");
    }
    let status = tool_status("agent", Some(&result_entry(&text)), false);
    assert_eq!(status, "");
    assert!(!shows_diff_preview("agent", &text));
    assert!(!shows_diff_preview("scout", &text));
}

#[test]
fn a_finished_call_shows_no_output_excerpt() {
    for text in [
        "42
",
        "compiled 3 files
",
        "   
",
        "name: context7-mcp
",
    ] {
        assert_eq!(tool_status("bash", Some(&result_entry(text)), false), "");
    }
    assert_eq!(
        tool_status("bash", Some(&result_entry("failed: exit 1")), false),
        crate::i18n::t("failed", "失败")
    );
    assert_eq!(
        tool_status("bash", None, true),
        crate::i18n::t("in progress", "进行中")
    );
}

#[test]
fn edit_removals_still_summarize_as_a_diff() {
    use std::fmt::Write as _;
    let mut text = String::from("edited src/lib.rs\n");
    for n in 1..=11 {
        let _ = writeln!(text, "- line {n}");
    }
    assert_eq!(
        tool_status("edit", Some(&result_entry(&text)), false),
        "+0 -11"
    );
    assert_eq!(diff_counts("+0 -11"), Some(("0", "11")));
    assert!(shows_diff_preview("edit", &text));
}

#[test]
fn write_additions_summarize_as_a_diff() {
    let text = "wrote src/new.rs\n+ alpha\n+ beta\n";
    assert_eq!(
        tool_status("write", Some(&result_entry(text)), false),
        "+2 -0"
    );
    assert!(shows_diff_preview("write", text));
}

#[test]
fn legacy_doubled_signs_still_count_and_display_as_one() {
    let mut text = String::from("edited src/lib.rs\n");
    text.push_str("++ kept\n");
    text.push_str("-- gone\n");
    assert_eq!(
        tool_status("edit", Some(&result_entry(&text)), false),
        "+1 -1"
    );
    assert_eq!(diff_line_text("++ kept"), "+ kept");
    assert_eq!(diff_line_text("-- gone"), "- gone");
    assert_eq!(diff_line_text("+ kept"), "+ kept");
    assert_eq!(diff_line_text("+++ b/file.rs"), "+++ b/file.rs");
    assert_eq!(diff_line_text("--- a/file.rs"), "--- a/file.rs");
}

#[test]
fn a_run_of_reads_and_edits_collapses_to_one_line() {
    let labels = [
        "read  a.rs",
        "read  b.rs",
        "read  c.rs",
        "edit  d.rs",
        "edit  e.rs",
        "edit  f.rs",
        "edit  g.rs",
    ];
    assert_eq!(tool_batch_summary(&labels), "read 3 files, edited 4");
    assert_eq!(
        tool_batch_summary(&["bash  cargo test", "grep  fn main"]),
        "ran 1 command, searched 1 time"
    );
}

#[test]
fn code_mode_rows_have_their_own_verbs_and_batch_phrase() {
    assert_eq!(
        tool_call_parts("run_code  read · grep", true, false, false).0,
        "Ran code"
    );
    assert_eq!(
        tool_call_parts("run_code  read · grep", false, true, false).0,
        "Running code"
    );
    assert_eq!(
        tool_call_parts("wait_code  cell-3", true, false, false),
        ("Waited for code".to_owned(), "cell-3".to_owned())
    );
    assert_eq!(
        tool_batch_summary(&["run_code  read", "run_code  edit", "read  a.rs"]),
        "ran 2 scripts, read 1 file"
    );
}

#[test]
fn a_run_code_result_splits_into_its_inner_calls() {
    let text = "script output\n[code calls]\nread  src/lib.rs\nedit  src/main.rs (failed)\n+ new\n- old\ngrep  needle src";
    let (head, calls) = split_code_calls(text).expect("breakdown");
    assert_eq!(head, "script output");
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].label, "read  src/lib.rs");
    assert!(!calls[0].failed);
    assert!(calls[0].body.is_empty());
    assert_eq!(calls[1].label, "edit  src/main.rs");
    assert!(calls[1].failed);
    assert_eq!(calls[1].body, ["+ new", "- old"]);
    assert_eq!(calls[2].label, "grep  needle src");
    assert_eq!(code_call_count(text), 3);
    // No heading, no split; a bare result counts nothing.
    assert!(split_code_calls("plain output").is_none());
    assert_eq!(code_call_count("plain output"), 0);
    assert_eq!(
        tool_status("run_code", Some(&result_entry(text)), false),
        "3 calls"
    );
}

#[test]
fn the_counting_scan_agrees_with_the_full_split() {
    // Continuations (diff lines, hunk headers, the truncation note) belong
    // to the call above them; a blank line starts nothing.
    let text = "out\n[code calls]\nread  a\nedit  b (failed)\n+ x\n@@ -1 +1 @@\n[diff truncated]\n\ngrep  c";
    assert_eq!(code_call_count(text), 3);
    assert_eq!(
        code_call_count(text),
        split_code_calls(text).expect("breakdown").1.len()
    );
    // A heading with no calls under it counts zero, like the split's None.
    assert_eq!(code_call_count("[code calls]"), 0);
    assert_eq!(code_call_count("[code calls]\n (failed)"), 0);
}

#[test]
fn numbering_a_diff_prefix_matches_the_full_numbering() {
    // The preview numbers only the painted prefix; numbering is
    // positional, so the prefix must agree line for line with the full
    // pass it replaces.
    let lines: Vec<String> = (0..120)
        .map(|n| {
            if n % 2 == 0 {
                format!("+ line {n}")
            } else {
                format!("- line {n}")
            }
        })
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let full = super::number_diff(&refs);
    let prefix = super::number_diff(&refs[..80]);
    assert_eq!(prefix.len(), 80);
    for (capped, whole) in prefix.iter().zip(&full) {
        assert_eq!(capped.old, whole.old);
        assert_eq!(capped.new, whole.new);
        assert_eq!(capped.text, whole.text);
        assert!(capped.kind == whole.kind);
    }
}

#[test]
fn diff_lines_are_numbered_from_the_first_change() {
    let lines = ["edited src/lib.rs", "+ alpha", "- beta", "  kept"];
    let numbered = super::number_diff(&lines);
    assert_eq!(numbered[0].old, None);
    assert_eq!(numbered[1].new, Some(1));
    assert_eq!(numbered[1].old, None);
    assert_eq!(numbered[2].old, Some(1));
    assert_eq!(numbered[2].new, None);
    assert_eq!(numbered[3].old, Some(2));
    assert_eq!(numbered[3].new, Some(2));
}

#[test]
fn unfinished_tool_calls_report_in_progress_only_while_a_turn_runs() {
    assert_eq!(tool_status("read", None, true), "in progress");
    assert_eq!(tool_status("read", None, false), "interrupted");

    let mut theme = gpui_kit::component::theme::Theme::default();
    crate::ui::desk::apply(&mut theme);
    assert_eq!(tool_dot_color(false, true, true, &theme), theme.primary);
    // An unfinished call whose turn ended died with it: red, like a failure.
    assert_eq!(tool_dot_color(false, true, false, &theme), theme.red);
    assert_eq!(tool_dot_color(true, false, false, &theme), theme.red);
    assert_eq!(tool_dot_color(false, false, false, &theme), theme.green);

    assert_eq!(
        tool_call_parts("edit  src/lib.rs", true, false, false),
        ("Edited".to_owned(), "src/lib.rs".to_owned())
    );
    assert_eq!(
        tool_call_parts("edit  src/lib.rs", false, true, false),
        ("Editing".to_owned(), "src/lib.rs".to_owned())
    );
    assert_eq!(
        tool_call_parts("edit  src/lib.rs", false, false, false),
        ("Edited".to_owned(), "src/lib.rs".to_owned())
    );

    assert_eq!(
        tool_call_parts("agent  scout", false, true, false).0,
        "Running agent"
    );
    assert_eq!(
        tool_call_parts("agent  scout", true, false, true).0,
        "Agent failed"
    );
    assert_eq!(
        tool_call_parts("agent  scout", false, false, false).0,
        "Agent interrupted"
    );
    assert_eq!(
        tool_call_parts("agent  scout", true, false, false).0,
        "Ran agent"
    );
}
