use super::{Pick, PreparedOp, apply_planned, plan_edits};
use crate::tool::ToolError;
use tokio_util::sync::CancellationToken;

fn literal(old: &str, new: &str) -> PreparedOp {
    PreparedOp::Literal {
        patterns: vec![old.to_owned()],
        replacements: vec![new.to_owned()],
        pick: Pick::Unique,
    }
}

fn apply(body: &str, old: &str, new: &str) -> Result<String, ToolError> {
    let ops = [literal(old, new)];
    let cancel = CancellationToken::new();
    let planned = plan_edits(body, &ops, &cancel)?;
    Ok(apply_planned(body, &planned, &cancel)?.text)
}

#[test]
fn trailing_spaces_the_copy_dropped_still_match() {
    let body = "fn main() {  \n    let x = 1;\t\n}\n";
    let updated = apply(
        body,
        "fn main() {\n    let x = 1;\n",
        "fn main() {\n    let x = 2;\n",
    )
    .expect("edit");
    assert_eq!(updated, "fn main() {\n    let x = 2;\n}\n");

    let body = "a = 1;   \r\nb = 2;\r\n";
    let updated = apply(body, "a = 1;\nb = 2;", "a = 3;\nb = 4;").expect("crlf edit");
    assert_eq!(updated, "a = 3;\r\nb = 4;\r\n");

    // Indentation is repaired by the relaxed pass, not by the exact matcher.
    let body = "if x {\n    go();\n}\n";
    let updated = apply(body, "if x {\ngo();", "if y {\ngo();").expect("indentation edit");
    assert_eq!(updated, "if y {\ngo();\n}\n");
}

#[test]
fn crlf_file_matches_lf_old_string_and_keeps_crlf() {
    let body = "fn main() {\r\n    let x = 1;\r\n}\r\n";
    let updated = apply(body, "    let x = 1;\n", "    let x = 2;\n").expect("edit");
    assert_eq!(updated, "fn main() {\r\n    let x = 2;\r\n}\r\n");
}

#[test]
fn lf_file_matches_crlf_old_string_and_keeps_lf() {
    let body = "alpha\nbeta\n";
    let updated = apply(body, "alpha\r\n", "gamma\r\n").expect("edit");
    assert_eq!(updated, "gamma\nbeta\n");
}

#[test]
fn exact_crlf_match_still_replaces_once() {
    let body = "a\r\nb\r\n";
    let updated = apply(body, "a\r\n", "c\r\n").expect("edit");
    assert_eq!(updated, "c\r\nb\r\n");
}

#[test]
fn repeated_crlf_lines_stay_ambiguous() {
    let body = "same\r\nsame\r\n";
    let error = apply(body, "same\n", "x\n").expect_err("two matches");
    let text = error.to_string();
    assert!(text.contains("occurs"), "{text}");
}

#[test]
fn multiline_lf_span_ending_at_a_crlf_keeps_the_following_line() {
    let body = "one\r\ntwo\r\nthree\r\n";
    let updated = apply(body, "one\ntwo\n", "ONE\nTWO\n").expect("edit");
    assert_eq!(updated, "ONE\r\nTWO\r\nthree\r\n");
}

#[test]
fn multiline_lf_span_ending_before_a_crlf_does_not_eat_the_cr() {
    let body = "head\r\none\r\ntwo\r\ntail\r\n";
    let updated = apply(body, "one\ntwo", "ONE\nTWO").expect("edit");
    assert_eq!(updated, "head\r\nONE\r\nTWO\r\ntail\r\n");
}

#[test]
fn multiline_lf_span_ending_at_eof_crlf_replaces_the_whole_tail() {
    let body = "one\r\ntwo\r\n";
    let updated = apply(body, "one\ntwo\n", "ONE\nTWO\n").expect("edit");
    assert_eq!(updated, "ONE\r\nTWO\r\n");
}

#[test]
fn multiline_lf_span_ending_at_eof_before_the_final_crlf_keeps_it() {
    let body = "one\r\ntwo\r\n";
    let updated = apply(body, "one\ntwo", "ONE\nTWO").expect("edit");
    assert_eq!(updated, "ONE\r\nTWO\r\n");
}

#[test]
fn lf_led_exact_match_in_a_crlf_file_takes_the_whole_newline() {
    // `\nb` occurs verbatim inside `\r\nb`; the match must not split the pair.
    let body = "a\r\nb\r\n";
    assert_eq!(apply(body, "\nb", "\nc").expect("edit"), "a\r\nc\r\n");
    assert_eq!(apply(body, "\nb", "c").expect("join"), "ac\r\n");
}
