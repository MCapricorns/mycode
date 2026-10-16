use super::{apply_planned, normalize_args, plan_edits};
use crate::builtin::edit::EditArgs;
use crate::tool::validate_args;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn apply_json(body: &str, raw: Value) -> Result<String, String> {
    validate_args::<EditArgs>(&raw).map_err(|err| err.to_string())?;
    let args: EditArgs = serde_json::from_value(raw).map_err(|err| err.to_string())?;
    let ops = normalize_args(&args).map_err(|err| err.to_string())?;
    let cancel = CancellationToken::new();
    let planned = plan_edits(body, &ops, &cancel).map_err(|err| err.to_string())?;
    apply_planned(body, &planned, &cancel)
        .map(|applied| applied.text)
        .map_err(|err| err.to_string())
}

fn mixed_call(old_string: &str, new_string: &str) -> Value {
    json!({
        "path": "src/plane.rs",
        "old_string": old_string,
        "new_string": new_string,
        "operations": [{
            "type": "literal",
            "pattern": "lives -= 1",
            "replacement": "lives -= 2"
        }]
    })
}

#[test]
fn mixed_legacy_fields_and_operations_apply_operations_only() {
    let body = "score += 1;\nlives -= 1;\n";
    let updated = apply_json(body, mixed_call("score += 1", "score += 10")).expect("edit");
    assert_eq!(updated, "score += 1;\nlives -= 2;\n");
}

#[test]
fn mixed_call_does_not_fall_back_to_legacy_fields() {
    let err = apply_json("score += 1;\n", mixed_call("score += 1", "score += 10"))
        .expect_err("operations needle is absent");
    assert!(err.contains("pattern not found"), "{err}");
    assert!(!err.to_ascii_lowercase().contains("combine"), "{err}");
}

#[test]
fn mixed_call_ignores_invalid_legacy_fields() {
    let body = "score += 1;\nlives -= 1;\n";
    let updated = apply_json(body, mixed_call("", "score += 10")).expect("edit");
    assert_eq!(updated, "score += 1;\nlives -= 2;\n");
}

#[test]
fn legacy_replace_still_applies_when_operations_are_omitted() {
    let updated = apply_json(
        "score += 1;\n",
        json!({
            "path": "src/plane.rs",
            "old_string": "score += 1",
            "new_string": "score += 10"
        }),
    )
    .expect("edit");
    assert_eq!(updated, "score += 10;\n");
}
