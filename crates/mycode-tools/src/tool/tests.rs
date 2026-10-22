use serde_json::json;

use super::normalize_tool_args;

#[test]
fn an_echoed_parameter_object_is_unwrapped() {
    let args = normalize_tool_args(json!({"mode": {"mode": "program"}}));
    assert_eq!(args, json!({"mode": "program"}));
}

#[test]
fn non_echoed_objects_pass_through_untouched() {
    // Different inner key, extra keys, and nested objects stay as-is.
    let args = normalize_tool_args(json!({"mode": {"launch": "program"}}));
    assert_eq!(args, json!({"mode": {"launch": "program"}}));
    let args = normalize_tool_args(json!({"mode": {"mode": "program", "x": 1}}));
    assert_eq!(args, json!({"mode": {"mode": "program", "x": 1}}));
    let args = normalize_tool_args(json!({"questions": [{"question": "Go?"}]}));
    assert_eq!(args, json!({"questions": [{"question": "Go?"}]}));
}

#[test]
fn argument_checks_compile_once_per_tool() {
    let tool = crate::builtin::read::ReadTool;
    let first = super::args_check(&tool).unwrap();
    let second = super::args_check(&tool).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert!(first.validate(&json!({"path": "a.rs"})).is_ok());
    assert!(first.validate(&json!({})).is_err());
}

#[test]
fn quoted_scalars_become_the_type_the_schema_expects() {
    let schema = json!({
        "type": "object",
        "properties": {
            "limit": { "type": ["integer", "null"], "minimum": 0 },
            "ratio": { "type": "number" },
            "overwrite": { "type": "boolean", "default": false },
            "maybe": { "anyOf": [{ "type": "boolean" }, { "type": "null" }] },
            "name": { "type": "string" },
            "either": { "type": ["string", "integer"] }
        }
    });
    let mut args = json!({
        "limit": " 10 ",
        "ratio": "0.5",
        "overwrite": "True",
        "maybe": "false",
        "name": "42",
        "either": "7",
        "unknown": "1",
        "nested": [{ "count": "3" }]
    });
    super::repair_args(&mut args, &schema_with_items(&schema));
    assert_eq!(
        args,
        json!({
            "limit": 10,
            "ratio": 0.5,
            "overwrite": true,
            "maybe": false,
            "name": "42",
            "either": "7",
            "unknown": "1",
            "nested": [{ "count": 3 }]
        })
    );
    let mut bad = json!({ "limit": "ten", "overwrite": "yes" });
    super::repair_args(&mut bad, &schema);
    assert_eq!(bad, json!({ "limit": "ten", "overwrite": "yes" }));
}

fn schema_with_items(schema: &serde_json::Value) -> serde_json::Value {
    let mut extended = schema.clone();
    extended["properties"]["nested"] = json!({
        "type": "array",
        "items": { "type": "object", "properties": { "count": { "type": "integer" } } }
    });
    extended
}

#[test]
fn required_fields_accept_other_agents_spellings() {
    let schema = json!({
        "type": "object",
        "required": ["path", "content"],
        "properties": {
            "path": { "type": "string" },
            "content": { "type": "string" }
        }
    });
    let mut args = json!({ "file_path": "a.rs", "text": "body" });
    super::repair_args(&mut args, &schema);
    assert_eq!(args, json!({ "path": "a.rs", "content": "body" }));

    // A declared property with the alias's name is a real field.
    let schema = json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string" },
            "file": { "type": "string" }
        }
    });
    let mut args = json!({ "file": "keep me" });
    super::repair_args(&mut args, &schema);
    assert_eq!(args, json!({ "file": "keep me" }));

    // One URL satisfies a required list of URLs.
    let schema = json!({
        "type": "object",
        "required": ["urls"],
        "properties": {
            "urls": { "type": "array", "items": { "type": "string" } }
        }
    });
    let mut args = json!({ "url": "https://example.com" });
    super::repair_args(&mut args, &schema);
    assert_eq!(args, json!({ "urls": ["https://example.com"] }));
}

#[test]
fn an_untagged_variant_is_filled_from_its_fields() {
    let schema = union_schema();
    let mut literal = json!({ "pattern": "a", "replacement": "b" });
    super::repair_args(&mut literal, &schema);
    assert_eq!(literal["type"], "literal");

    let mut fuzzy = json!({ "pattern": "a", "replacement": "b", "max_distance": "2" });
    super::repair_args(&mut fuzzy, &schema);
    assert_eq!(fuzzy["type"], "fuzzy");
    assert_eq!(fuzzy["max_distance"], 2);

    // The same repair runs inside a nested array of operations.
    let wrapper = json!({
        "type": "object",
        "properties": {
            "operations": { "type": "array", "items": schema.clone() }
        }
    });
    let mut call = json!({ "operations": [{ "pattern": "a", "replacement": "b" }] });
    super::repair_args(&mut call, &wrapper);
    assert_eq!(call["operations"][0]["type"], "literal");
}

#[test]
fn a_misspelled_tag_is_corrected_and_annotations_are_dropped() {
    let schema = union_schema();
    let mut args = json!({
        "type": "Literal",
        "pattern": "a",
        "replacement": "b",
        "type_note": "literal"
    });
    super::repair_args(&mut args, &schema);
    assert_eq!(
        args,
        json!({ "type": "literal", "pattern": "a", "replacement": "b" })
    );
}

#[test]
fn an_empty_or_foreign_object_is_left_for_validation() {
    let schema = union_schema();
    let mut empty = json!({});
    super::repair_args(&mut empty, &schema);
    assert_eq!(empty, json!({}));

    let mut foreign = json!({ "unrelated": 1, "other": 2, "third": 3 });
    let before = foreign.clone();
    super::repair_args(&mut foreign, &schema);
    assert_eq!(foreign, before);
}

#[test]
fn a_union_without_a_tag_is_not_guessed() {
    let schema = json!({
        "oneOf": [
            { "type": "object", "properties": { "left": { "type": "string" } } },
            { "type": "object", "properties": { "right": { "type": "string" } } }
        ]
    });
    let mut args = json!({ "left": "a" });
    super::repair_args(&mut args, &schema);
    assert_eq!(args, json!({ "left": "a" }));
}

#[test]
fn builtin_schemas_accept_common_misspellings() {
    let write = super::args_check(&crate::builtin::WriteTool).unwrap();
    let mut args = json!({ "file": "a.txt", "text": "hi" });
    super::repair_args(&mut args, &write.schema);
    assert!(write.validate(&args).is_ok(), "{args}");

    let grep = super::args_check(&crate::builtin::GrepTool).unwrap();
    let mut args = json!({ "query": "fn main", "path": "src" });
    super::repair_args(&mut args, &grep.schema);
    assert!(grep.validate(&args).is_ok(), "{args}");

    let read = super::args_check(&crate::builtin::ReadTool).unwrap();
    let mut args = json!({ "file_path": "a.rs", "offset": "3", "limit": "10" });
    super::repair_args(&mut args, &read.schema);
    assert!(read.validate(&args).is_ok(), "{args}");
}

fn union_schema() -> serde_json::Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "type": { "const": "literal" },
                    "pattern": { "type": "string" },
                    "replacement": { "type": "string" }
                },
                "required": ["type"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "regex" },
                    "pattern": { "type": "string" },
                    "replacement": { "type": "string" }
                },
                "required": ["type", "pattern", "replacement"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "fuzzy" },
                    "pattern": { "type": "string" },
                    "replacement": { "type": "string" },
                    "max_distance": { "type": "integer" }
                },
                "required": ["type", "pattern", "replacement", "max_distance"],
                "additionalProperties": false
            }
        ]
    })
}

#[test]
fn a_stringified_argument_object_is_unwrapped() {
    let args = normalize_tool_args(json!("{\"path\": \"a.rs\", \"limit\": 5}"));
    assert_eq!(args, json!({ "path": "a.rs", "limit": 5 }));
    let untouched = normalize_tool_args(json!("not json"));
    assert_eq!(untouched, json!("not json"));
}

#[tokio::test]
async fn execute_dyn_validates_against_overridden_params_schema() {
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::Value;

    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolDyn, ToolError, ToolResult};

    #[derive(Deserialize, JsonSchema)]
    struct CustomArgs {
        count: u32,
    }

    struct CustomSchemaTool;

    impl Tool for CustomSchemaTool {
        type Args = CustomArgs;
        type Output = ();

        fn name(&self) -> &str {
            "custom"
        }

        fn description(&self) -> &str {
            "custom schema tool"
        }

        fn params_schema(&self) -> Value {
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "required": ["count"],
                "properties": {
                    "count": { "type": "integer", "minimum": 1, "maximum": 5 }
                }
            })
        }

        async fn execute(
            &self,
            args: Self::Args,
            _ctx: &ToolCtx,
            _out: &mut ToolStream,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::text(args.count.to_string()))
        }
    }

    let tool = CustomSchemaTool;
    let spec = ToolDyn::spec(&tool);
    assert!(spec.params_schema.get("$schema").is_none());

    let ctx = ToolCtx::new(".");
    let mut out = ToolStream::closed();
    let err = ToolDyn::execute_dyn(&tool, json!({"count": 10}), &ctx, &mut out)
        .await
        .expect_err("count above custom maximum must fail schema validation");
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");

    let ok = ToolDyn::execute_dyn(&tool, json!({"count": 3}), &ctx, &mut out)
        .await
        .expect("valid count");
    assert!(!ok.is_error);
}

/// The `edit`-style tagged union: an op object without `type` must not
/// dump its multiline contents into the error. The message names the
/// missing tag and the allowed variants.
#[test]
fn a_tagged_union_failure_names_the_missing_tag() {
    use serde_json::json;

    let schema = json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string" },
            "operations": {
                "type": "array",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "required": ["type", "pattern", "replacement"],
                            "properties": {
                                "type": { "const": "literal" },
                                "pattern": { "type": "string" },
                                "replacement": { "type": "string" }
                            },
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "required": ["type", "pattern", "replacement"],
                            "properties": {
                                "type": { "const": "regex" },
                                "pattern": { "type": "string" },
                                "replacement": { "type": "string" }
                            },
                            "additionalProperties": false
                        }
                    ]
                }
            }
        }
    });
    let args = json!({
        "path": "a.rs",
        "operations": [
            { "pattern": "long\n".repeat(200), "replacement": "x" }
        ]
    });
    let err = super::validate_args_with_schema(&schema, &args)
        .expect_err("an op without a tag must fail");
    let text = err.to_string();
    assert!(text.contains("missing required field `type`"), "{text}");
    assert!(text.contains("literal, regex"), "{text}");
    assert!(text.contains("/operations/0"), "{text}");
    // The multibyte pattern content must not flood the message.
    assert!(text.len() < 400, "{text}");
}

/// A present-but-wrong tag reports the allowed variants.
#[test]
fn a_tagged_union_failure_lists_the_allowed_tags() {
    use serde_json::json;

    let schema = json!({
        "type": "object",
        "properties": {
            "ops": {
                "type": "array",
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "required": ["type"],
                            "properties": { "type": { "const": "literal" } }
                        },
                        {
                            "type": "object",
                            "required": ["type"],
                            "properties": { "type": { "const": "regex" } }
                        }
                    ]
                }
            }
        }
    });
    let args = json!({ "ops": [ { "type": "bogus" } ] });
    let err =
        super::validate_args_with_schema(&schema, &args).expect_err("an unknown tag must fail");
    let text = err.to_string();
    assert!(text.contains("expected one of: literal, regex"), "{text}");
}
