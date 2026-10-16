use super::{
    diff_counts, diff_line_text, shows_diff_preview, tool_batch_summary, tool_call_parts,
    tool_dot_color, tool_status,
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
    assert_eq!(
        tool_dot_color(false, true, false, &theme),
        theme.muted_foreground
    );
    assert_eq!(tool_dot_color(true, false, false, &theme), theme.red);
    assert_eq!(tool_dot_color(false, false, false, &theme), theme.green);

    assert_eq!(
        tool_call_parts("edit  src/lib.rs", true, false),
        ("Edited".to_owned(), "src/lib.rs".to_owned())
    );
    assert_eq!(
        tool_call_parts("edit  src/lib.rs", false, true),
        ("Editing".to_owned(), "src/lib.rs".to_owned())
    );
    assert_eq!(
        tool_call_parts("edit  src/lib.rs", false, false),
        ("Edited".to_owned(), "src/lib.rs".to_owned())
    );
}
