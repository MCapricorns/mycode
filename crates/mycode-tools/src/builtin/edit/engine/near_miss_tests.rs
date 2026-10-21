//! Literal misses: the compatibility repairs that now apply, and the
//! near-miss hints left for the misses they cannot repair.
use super::{Pick, PreparedOp, apply_planned, plan_edits, plan_edits_with_notes};
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

fn apply(body: &str, old: &str, new: &str) -> (String, Vec<String>) {
    let ops = [literal(old, new)];
    let cancel = CancellationToken::new();
    let (planned, notes) =
        plan_edits_with_notes(body, &ops, &cancel).expect("relaxed plan succeeds");
    let applied = apply_planned(body, &planned, &cancel).expect("apply");
    (applied.text, notes)
}

#[test]
fn different_spacing_is_repaired() {
    let body = "fn f() {\n    let x = 1;\n}\n";
    let (text, notes) = apply(body, "let  x = 1;", "let y = 2;");
    assert_eq!(text, "fn f() {\nlet y = 2;\n}\n");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("ignoring whitespace"))
    );
}

#[test]
fn different_indentation_is_repaired() {
    let body = "fn f() {\n    let x = 1;\n}\n";
    let (text, _) = apply(body, "\tlet x = 1;", "let y = 2;");
    assert_eq!(text, "fn f() {\nlet y = 2;\n}\n");
}

#[test]
fn different_line_wrapping_is_repaired() {
    let body = "call(a,\n     b,\n     c);\n";
    let (text, notes) = apply(body, "call(a, b, c);", "call(x);");
    assert_eq!(text, "call(x);\n");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("ignore") || note.contains("fuzzy")),
        "{notes:?}"
    );
}

#[test]
fn a_multi_line_indentation_difference_is_repaired() {
    let body = "fn f() {\n\tlet x = 1;\n\tlet y = 2;\n}\n";
    let (text, notes) = apply(body, "    let x = 1;\n    let y = 2;", "    let z = 3;");
    assert_eq!(text, "fn f() {\n    let z = 3;\n}\n");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("ignoring whitespace"))
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
fn a_crlf_body_still_repairs_indentation_differences() {
    let body = "fn f() {\r\n\t\tlet x = 1;\r\n}\r\n";
    let (text, _) = apply(body, "    let x = 1;", "let y = 2;");
    assert_eq!(text, "fn f() {\r\nlet y = 2;\r\n}\r\n");
}

#[test]
fn the_earliest_near_miss_wins_across_patterns() {
    let body = "a\n  b(x)\n  c(y)\n";
    let ops = [PreparedOp::Literal {
        patterns: vec!["c(  y )".to_owned(), "b(  x )".to_owned()],
        replacements: vec!["r1".to_owned(), "r2".to_owned()],
        pick: Pick::Unique,
    }];
    let text = miss(body, &ops);
    assert!(text.contains("line 2"), "{text}");
}

#[test]
fn a_near_miss_inside_a_batch_keeps_the_operation_index() {
    let body = "alpha\n        let x = 1;\n    tail\n";
    let ops = [literal("zzz", "0"), literal("qqq", "1")];
    let text = miss(body, &ops);
    assert!(text.contains("operation 1 of 2"), "{text}");
    assert!(!text.contains("similar text"), "{text}");

    let ops = [
        literal("alpha", "b"),
        literal("        let x = 1;\n    extra", "let y = 2;"),
    ];
    let text = miss(body, &ops);
    assert!(text.contains("operation 2 of 2"), "{text}");
    assert!(!text.contains("operation 1 of 2"), "{text}");
}
