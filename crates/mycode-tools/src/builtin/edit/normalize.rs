//! Repairs model-emitted `edit` arguments before schema validation.
//!
//! `operations` is a tagged union. Models regularly leave the `type` tag off
//! later entries of a batch, or attach an annotation key to an otherwise
//! valid operation (`{"type":"literal","pattern":…,"type_note":"literal"}`).
//! The schema rejects both, so an unambiguous edit costs a whole turn. This
//! pass fills the tag from the fields that are present and keeps only the
//! fields of the chosen variant; the schema and [`super::engine`] still
//! validate everything that is left.
use serde_json::{Map, Value};

/// Spellings models use for the operation fields; the schema names are the
/// second element.
const FIELD_ALIASES: &[(&str, &str)] = &[
    ("old_string", "pattern"),
    ("old_str", "pattern"),
    ("old", "pattern"),
    ("find", "pattern"),
    ("search", "pattern"),
    ("needle", "pattern"),
    ("new_string", "replacement"),
    ("new_str", "replacement"),
    ("new", "replacement"),
    ("replace", "replacement"),
    ("start", "start_line"),
    ("from_line", "start_line"),
    ("end", "end_line"),
    ("to_line", "end_line"),
    ("distance", "max_distance"),
];

/// Fields that belong to one variant. Anything else on the operation is
/// dropped, because `EditOp` denies unknown fields and a stray annotation
/// would otherwise reject an operation whose intent is complete.
const LITERAL_FIELDS: &[&str] = &[
    "type",
    "pattern",
    "replacement",
    "patterns",
    "replacements",
    "occurrence",
    "n",
];
const REGEX_FIELDS: &[&str] = &["type", "pattern", "replacement", "occurrence", "n"];
const FUZZY_FIELDS: &[&str] = &["type", "pattern", "replacement", "max_distance"];
const LINE_RANGE_FIELDS: &[&str] = &[
    "type",
    "start_line",
    "end_line",
    "expected_text",
    "expected_hash",
    "replacement",
];

/// Rewrites `operations` entries into the schema shape.
///
/// Everything outside `operations` is left exactly as it arrived.
pub(super) fn normalize_edit_args(args: Value) -> Value {
    let Value::Object(mut outer) = args else {
        return args;
    };
    let Some(Value::Array(operations)) = outer.get_mut("operations") else {
        return Value::Object(outer);
    };
    for operation in operations.iter_mut() {
        normalize_operation(operation);
    }
    Value::Object(outer)
}

fn normalize_operation(operation: &mut Value) {
    let Value::Object(op) = operation else {
        return;
    };
    alias_fields(op);
    normalize_occurrence(op);
    coerce_counts(op);
    let kind = op
        .get("type")
        .and_then(Value::as_str)
        .and_then(canonical_kind)
        .or_else(|| infer_kind(op));
    // A bare object with none of the variant fields is not an operation any
    // guess can repair; the schema names the tag and the allowed variants.
    let Some(kind) = kind else {
        return;
    };
    let fields = fields_of(kind);
    let mut cleaned = Map::new();
    for (key, value) in std::mem::take(op) {
        // The tag is rewritten below; a spelled-out variant ("Regexp") must
        // not survive as the schema's `const` value.
        if key != "type" && fields.contains(&key.as_str()) {
            cleaned.insert(key, value);
        }
    }
    cleaned.insert("type".to_owned(), Value::String(kind.to_owned()));
    *op = cleaned;
}

fn alias_fields(op: &mut Map<String, Value>) {
    for (from, to) in FIELD_ALIASES {
        if op.contains_key(*to) {
            continue;
        }
        if let Some(value) = op.remove(*from) {
            op.insert((*to).to_owned(), value);
        }
    }
}

/// `replace_all` is the flag every other editor uses; `first`/`each` are
/// occurrence spellings the schema does not name.
fn normalize_occurrence(op: &mut Map<String, Value>) {
    if !op.contains_key("occurrence")
        && (op.get("replace_all") == Some(&Value::Bool(true))
            || op.get("all") == Some(&Value::Bool(true)))
    {
        op.insert("occurrence".to_owned(), Value::String("all".to_owned()));
    }
    let Some(raw) = op
        .get("occurrence")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let normalized = match raw.trim().to_ascii_lowercase().as_str() {
        "first" | "1st" => {
            op.insert("n".to_owned(), Value::from(1));
            "nth"
        }
        "each" | "every" | "replace_all" => "all",
        "one" | "single" | "once" => "unique",
        _ => return,
    };
    op.insert(
        "occurrence".to_owned(),
        Value::String(normalized.to_owned()),
    );
}

/// Line numbers and distances arrive quoted from XML tool calls and from
/// models that stringify every scalar. Only the operation's own integers are
/// touched; the schema still rejects a value that does not parse.
fn coerce_counts(op: &mut Map<String, Value>) {
    for key in ["start_line", "end_line", "max_distance", "n"] {
        let Some(Value::String(text)) = op.get(key).cloned() else {
            continue;
        };
        let Ok(number) = text.trim().parse::<u64>() else {
            continue;
        };
        op.insert(key.to_owned(), Value::from(number));
    }
}

fn canonical_kind(raw: &str) -> Option<&'static str> {
    let key = raw.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    match key.as_str() {
        "literal" | "replace" | "exact" | "text" => Some("literal"),
        "regex" | "regex_replace" | "regexp" => Some("regex"),
        "fuzzy" | "fuzzy_replace" | "approximate" => Some("fuzzy"),
        "line_range" | "line" | "lines" | "range" => Some("line_range"),
        _ => None,
    }
}

/// `literal` and `regex` share `pattern`+`replacement`, so an untagged pair
/// is read as a literal: that is the shape models mean, and a regex that
/// matched accidentally would be a surprising edit.
fn infer_kind(op: &Map<String, Value>) -> Option<&'static str> {
    if op.contains_key("start_line") || op.contains_key("end_line") {
        return Some("line_range");
    }
    if op.contains_key("max_distance") {
        return Some("fuzzy");
    }
    if op.contains_key("pattern") || op.contains_key("patterns") {
        if op.get("regex") == Some(&Value::Bool(true)) {
            return Some("regex");
        }
        return Some("literal");
    }
    None
}

fn fields_of(kind: &str) -> &'static [&'static str] {
    match kind {
        "regex" => REGEX_FIELDS,
        "fuzzy" => FUZZY_FIELDS,
        "line_range" => LINE_RANGE_FIELDS,
        _ => LITERAL_FIELDS,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::normalize_edit_args;
    use crate::builtin::EditTool;
    use crate::tool::{ToolDyn, validate_args_with_schema};

    fn schema() -> Value {
        ToolDyn::spec(&EditTool).params_schema
    }

    fn assert_valid(args: &Value) {
        let normalized = normalize_edit_args(args.clone());
        validate_args_with_schema(&schema(), &normalized).unwrap_or_else(|error| {
            panic!("normalized arguments must validate: {error}\n{normalized}")
        });
    }

    #[test]
    fn an_operation_without_a_tag_is_inferred() {
        let args = json!({
            "path": "a.rs",
            "operations": [
                { "pattern": "foo", "replacement": "bar" },
                { "type": "regex", "pattern": "a+", "replacement": "b", "occurrence": "all" }
            ]
        });
        assert!(
            validate_args_with_schema(&schema(), &args).is_err(),
            "the untagged shape is what the model sends and what fails today"
        );
        let normalized = normalize_edit_args(args);
        assert_eq!(normalized["operations"][0]["type"], "literal");
        assert_eq!(normalized["operations"][1]["type"], "regex");
        assert_valid(&normalized);
    }

    #[test]
    fn an_unknown_annotation_key_does_not_reject_the_operation() {
        let args = json!({
            "path": "a.rs",
            "operations": [
                { "type": "literal", "pattern": "foo", "replacement": "bar", "type_note": "literal" }
            ]
        });
        assert!(validate_args_with_schema(&schema(), &args).is_err());
        let normalized = normalize_edit_args(args);
        assert!(normalized["operations"][0].get("type_note").is_none());
        assert_valid(&normalized);
    }

    #[test]
    fn variant_fields_are_inferred_from_their_own_keys() {
        let args = json!({
            "path": "a.rs",
            "operations": [
                { "start_line": "3", "end_line": "4", "expected_text": "a\nb", "replacement": "c" },
                { "pattern": "fuzzy text", "replacement": "x", "max_distance": "2" }
            ]
        });
        let normalized = normalize_edit_args(args);
        assert_eq!(normalized["operations"][0]["type"], "line_range");
        assert_eq!(normalized["operations"][0]["start_line"], 3);
        assert_eq!(normalized["operations"][1]["type"], "fuzzy");
        assert_eq!(normalized["operations"][1]["max_distance"], 2);
        assert_valid(&normalized);
    }

    #[test]
    fn other_tools_field_names_become_the_schema_names() {
        let args = json!({
            "path": "a.rs",
            "operations": [
                { "type": "literal", "old_string": "foo", "new_string": "bar", "replace_all": true }
            ]
        });
        let normalized = normalize_edit_args(args);
        let op = &normalized["operations"][0];
        assert_eq!(op["pattern"], "foo");
        assert_eq!(op["replacement"], "bar");
        assert_eq!(op["occurrence"], "all");
        assert_valid(&normalized);
    }

    #[test]
    fn an_unknown_occurrence_spelling_is_mapped() {
        let args = json!({
            "path": "a.rs",
            "operations": [{ "pattern": "foo", "replacement": "bar", "occurrence": "First" }]
        });
        let normalized = normalize_edit_args(args);
        assert_eq!(normalized["operations"][0]["type"], "literal");
        assert_eq!(normalized["operations"][0]["occurrence"], "nth");
        assert_eq!(normalized["operations"][0]["n"], 1);
        assert_valid(&normalized);
    }

    #[test]
    fn a_spelled_out_tag_is_rewritten_to_the_schema_name() {
        let args = json!({
            "path": "a.rs",
            "operations": [
                { "type": "Regexp", "pattern": "a+", "replacement": "b" },
                { "type": "line-range", "start_line": 1, "end_line": 1, "expected_text": "x", "replacement": "y" }
            ]
        });
        let normalized = normalize_edit_args(args);
        assert_eq!(normalized["operations"][0]["type"], "regex");
        assert_eq!(normalized["operations"][1]["type"], "line_range");
        assert_valid(&normalized);
    }

    #[test]
    fn an_operation_with_no_recognizable_field_keeps_failing() {
        let args = json!({
            "path": "a.rs",
            "operations": [{ "unrelated": 1 }]
        });
        let normalized = normalize_edit_args(args.clone());
        assert_eq!(normalized, args);
        assert!(validate_args_with_schema(&schema(), &normalized).is_err());
    }

    #[test]
    fn calls_without_operations_are_untouched() {
        let args = json!({ "path": "a.rs", "old_string": "a", "new_string": "b" });
        assert_eq!(normalize_edit_args(args.clone()), args);
    }
}
