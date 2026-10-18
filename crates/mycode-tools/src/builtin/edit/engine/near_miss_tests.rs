//! Whitespace-insensitive near-miss hints on literal misses.
use super::{Pick, PreparedOp, plan_edits};
use tokio_util::sync::CancellationToken;

fn literal(old: &str, new: &str) -> PreparedOp {
    PreparedOp::Literal {
        patterns: vec![old.to_owned()],
        replacements: vec![new.to_owned()],
        pick: Pick::Unique,
    }
}

fn miss(body: &str, ops: &[PreparedOp]) -> String {
    plan_edits(body, ops, &CancellationToken::new())
        .err()
        .expect("miss")
        .to_string()
}

#[test]
fn a_miss_with_different_spacing_points_at_the_similar_line() {
    let body = "fn f() {\n    let x = 1;\n}\n";
    // The pattern doubles a space; stripped of whitespace both sides agree.
    let text = miss(body, &[literal("let  x = 1;", "let y = 2;")]);
    assert!(text.contains("pattern not found"), "{text}");
    assert!(text.contains("line 2"), "{text}");
    assert!(text.contains("whitespace/indentation"), "{text}");
}

#[test]
fn a_miss_with_different_indentation_points_at_the_similar_line() {
    let body = "fn f() {\n    let x = 1;\n}\n";
    let text = miss(body, &[literal("\tlet x = 1;", "let y = 2;")]);
    assert!(text.contains("line 2"), "{text}");
    assert!(text.contains("whitespace/indentation"), "{text}");
}

#[test]
fn a_miss_with_different_line_wrapping_points_at_the_start_line() {
    let body = "call(a,\n     b,\n     c);\n";
    let text = miss(body, &[literal("call(a, b, c);", "call(x);")]);
    assert!(text.contains("line 1"), "{text}");
    assert!(text.contains("whitespace/indentation"), "{text}");
}

#[test]
fn a_near_miss_quotes_the_file_text() {
    let body = "fn f() {\n\tlet x = 1;\n\tlet y = 2;\n}\n";
    let text = miss(body, &[literal("    let x = 1;\n    let y = 2;", "z")]);
    assert!(text.contains("line 2"), "{text}");
    assert!(
        text.contains("The file there reads:\n\tlet x = 1;\n\tlet y = 2;"),
        "{text}"
    );
}

#[test]
fn a_stale_copy_is_anchored_on_its_first_line() {
    let body = "fn handle_request(req) {\n    validate(req);\n    respond(req);\n}\n";
    let text = miss(
        body,
        &[literal("fn handle_request(req) {\n    check(req);\n", "x")],
    );
    assert!(text.contains("first line matches line 1"), "{text}");
    assert!(text.contains("validate(req);"), "{text}");
}

#[test]
fn a_genuine_miss_keeps_the_plain_message() {
    let body = "if let Some(v) = conn.last();\n";
    let text = miss(body, &[literal("match conn.last()", "x")]);
    assert!(text.contains("pattern not found"), "{text}");
    assert!(!text.contains("similar text"), "{text}");
    assert!(!text.contains("line "), "{text}");
}

#[test]
fn a_whitespace_only_pattern_never_hints() {
    let body = "let x = 1;\n";
    let text = miss(body, &[literal("   \n  ", "x")]);
    assert!(text.contains("pattern not found"), "{text}");
    assert!(!text.contains("similar text"), "{text}");
}

#[test]
fn a_crlf_body_still_hints_on_indentation_differences() {
    let body = "fn f() {\r\n\t\tlet x = 1;\r\n}\r\n";
    let text = miss(body, &[literal("    let x = 1;", "let y = 2;")]);
    assert!(text.contains("line 2"), "{text}");
}

#[test]
fn the_earliest_near_miss_wins_across_patterns() {
    let body = "a\n  b(x)\n  c(y)\n";
    let ops = [PreparedOp::Literal {
        patterns: vec!["c( y )".to_owned(), "b( x )".to_owned()],
        replacements: vec!["r1".to_owned(), "r2".to_owned()],
        pick: Pick::Unique,
    }];
    let text = miss(body, &ops);
    assert!(text.contains("line 2"), "{text}");
}

#[test]
fn a_near_miss_inside_a_batch_keeps_the_operation_index() {
    let body = "a\n        let x = 1;\n";
    let ops = [literal("zzz", "0"), literal("let x = 1;", "let y = 2;")];
    let text = miss(body, &ops);
    assert!(text.contains("operation 1 of 2"), "{text}");
    assert!(!text.contains("similar text"), "{text}");

    let ops = [literal("a", "b"), literal("let  x = 1;", "let y = 2;")];
    let text = miss(body, &ops);
    assert!(text.contains("operation 2 of 2"), "{text}");
    assert!(text.contains("line 2"), "{text}");
}
