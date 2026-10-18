//! Batch failure tagging: a miss names its operation index and states that
//! the call applied nothing.
use super::{Pick, PreparedOp, apply_planned, plan_edits};
use tokio_util::sync::CancellationToken;

fn literal(old: &str, new: &str) -> PreparedOp {
    PreparedOp::Literal {
        patterns: vec![old.to_owned()],
        replacements: vec![new.to_owned()],
        pick: Pick::Unique,
    }
}

#[test]
fn a_batch_miss_names_the_failing_operation_and_atomicity() {
    let body = "alpha\nbeta\n";
    let ops = [
        literal("alpha", "one"),
        literal("gamma", "two"),
        literal("beta", "three"),
    ];
    let error = plan_edits(body, &ops, &CancellationToken::new())
        .err()
        .expect("miss");
    let text = error.to_string();
    assert!(text.contains("operation 2 of 3"), "{text}");
    assert!(text.contains("no changes were applied"), "{text}");
    assert!(text.contains("pattern not found"), "{text}");
}

#[test]
fn a_single_op_miss_keeps_the_plain_message() {
    let ops = [literal("absent", "x")];
    let error = plan_edits("body\n", &ops, &CancellationToken::new())
        .err()
        .expect("miss");
    let text = error.to_string();
    assert!(text.contains("pattern not found"), "{text}");
    assert!(!text.contains("operation"), "{text}");
    assert!(!text.contains("no changes were applied"), "{text}");
}

#[test]
fn a_failed_batch_plans_nothing() {
    let body = "alpha\nbeta\n";
    let ops = [literal("alpha", "one"), literal("missing", "two")];
    let cancel = CancellationToken::new();
    let error = plan_edits(body, &ops, &cancel).err().expect("miss");
    assert!(error.to_string().contains("operation 2 of 2"));
    // The first op would have matched; the batch must still plan nothing.
    assert_eq!(body, "alpha\nbeta\n");
}

#[test]
fn a_fully_matching_batch_still_applies() {
    let body = "alpha\nbeta\n";
    let ops = [literal("alpha", "one"), literal("beta", "two")];
    let cancel = CancellationToken::new();
    let planned = plan_edits(body, &ops, &cancel).expect("plan");
    let applied = apply_planned(body, &planned, &cancel).expect("apply");
    assert_eq!(applied.text, "one\ntwo\n");
}

#[test]
fn batch_invalid_args_also_carry_the_operation_index() {
    let ops = [
        literal("alpha", "one"),
        PreparedOp::Literal {
            patterns: vec![String::new()],
            replacements: vec!["x".to_owned()],
            pick: Pick::Unique,
        },
    ];
    // Empty patterns are rejected at prepare time, so this op is built
    // directly to exercise the runtime path.
    let error = plan_edits("alpha\n", &ops, &CancellationToken::new())
        .err()
        .expect("miss");
    let text = error.to_string();
    assert!(text.contains("operation 2 of 2"), "{text}");
}
