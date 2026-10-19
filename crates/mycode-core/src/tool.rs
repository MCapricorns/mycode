//! Tool specification shared between the tool registry and LLM providers.

use serde::{Deserialize, Serialize};

/// Serializable description of a tool, sent to LLM providers.
///
/// Produced by the tool registry (`mycode-tools`) from `schemars`-derived
/// schemas; plugins declare tools in the same shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Tool name, unique within a registry (last registration wins).
    pub name: String,
    /// Human/model-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's arguments (`serde_json::Value`).
    pub params_schema: serde_json::Value,
}

/// How deep `$ref` expansion may go before a cycle is closed.
const SCHEMA_REF_DEPTH: usize = 32;

/// Inlines local `$ref`s and drops `$defs` / `definitions`.
///
/// OpenCode `packages/opencode/src/provider/transform.ts` `schema()` sanitizes
/// per host: `sanitizeMoonshot` keeps a bare `$ref` (Moonshot expands it and
/// rejects sibling keywords), and `sanitizeGemini` copies `$ref` through.
/// Moonshot's coding-plan validator still 400s on schemars `$ref` + `$defs`
/// ("infinite recursion" at `operations.items`). Every provider gets the
/// expanded schema instead. A reference that is already on the stack, or
/// deeper than [`SCHEMA_REF_DEPTH`], becomes `{type: object}` so a real cycle
/// cannot recurse.
pub fn inline_schema_refs(schema: &mut serde_json::Value) {
    // Builtin specs arrive already inlined; providers call this per request,
    // so a schema with nothing to expand is left untouched instead of copied.
    if !has_schema_refs(schema) {
        return;
    }
    let mut defs = serde_json::Map::new();
    collect_defs(schema, &mut defs);
    let mut stack = Vec::new();
    *schema = expand(schema, &defs, &mut stack);
}

fn has_schema_refs(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(has_schema_refs),
        serde_json::Value::Object(object) => {
            object.contains_key("$ref")
                || object.contains_key("$defs")
                || object.contains_key("definitions")
                || object.values().any(has_schema_refs)
        }
        _ => false,
    }
}

fn collect_defs(value: &serde_json::Value, defs: &mut serde_json::Map<String, serde_json::Value>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_defs(item, defs);
            }
        }
        serde_json::Value::Object(object) => {
            for key in ["$defs", "definitions"] {
                let Some(serde_json::Value::Object(bucket)) = object.get(key) else {
                    continue;
                };
                for (name, schema) in bucket {
                    defs.entry(name.clone()).or_insert_with(|| schema.clone());
                    collect_defs(schema, defs);
                }
            }
            for (key, child) in object {
                if key == "$defs" || key == "definitions" {
                    continue;
                }
                collect_defs(child, defs);
            }
        }
        _ => {}
    }
}

fn expand(
    node: &serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
    stack: &mut Vec<String>,
) -> serde_json::Value {
    let serde_json::Value::Object(object) = node else {
        return match node {
            serde_json::Value::Array(items) => serde_json::Value::Array(
                items.iter().map(|item| expand(item, defs, stack)).collect(),
            ),
            other => other.clone(),
        };
    };
    if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
        if stack.iter().any(|seen| seen == reference) || stack.len() >= SCHEMA_REF_DEPTH {
            return terminated(object);
        }
        if let Some(target) = resolve_ref(reference, defs) {
            stack.push(reference.to_owned());
            let mut expanded = expand(target, defs, stack);
            stack.pop();
            keep_description(object, &mut expanded);
            return expanded;
        }
        let mut without = object.clone();
        without.remove("$ref");
        return expand(&serde_json::Value::Object(without), defs, stack);
    }
    let mut out = serde_json::Map::new();
    for (key, value) in object {
        if key == "$defs" || key == "definitions" {
            continue;
        }
        let expanded = match value {
            // Keys of a property map are parameter names, not keywords: a
            // parameter called `definitions` must survive the expansion.
            serde_json::Value::Object(properties)
                if key == "properties" || key == "patternProperties" =>
            {
                serde_json::Value::Object(
                    properties
                        .iter()
                        .map(|(name, schema)| (name.clone(), expand(schema, defs, stack)))
                        .collect(),
                )
            }
            _ => expand(value, defs, stack),
        };
        out.insert(key.clone(), expanded);
    }
    serde_json::Value::Object(out)
}

fn terminated(object: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "type".to_owned(),
        serde_json::Value::String("object".to_owned()),
    );
    if let Some(description) = object.get("description").filter(|value| value.is_string()) {
        out.insert("description".to_owned(), description.clone());
    }
    serde_json::Value::Object(out)
}

fn keep_description(
    source: &serde_json::Map<String, serde_json::Value>,
    expanded: &mut serde_json::Value,
) {
    let Some(description) = source.get("description").filter(|value| value.is_string()) else {
        return;
    };
    if let Some(object) = expanded.as_object_mut() {
        object
            .entry("description".to_owned())
            .or_insert_with(|| description.clone());
    }
}

fn resolve_ref<'a>(
    reference: &str,
    defs: &'a serde_json::Map<String, serde_json::Value>,
) -> Option<&'a serde_json::Value> {
    let pointer = reference.strip_prefix("#/")?;
    let mut parts = pointer.split('/');
    let bucket = parts.next()?;
    if bucket != "$defs" && bucket != "definitions" {
        return None;
    }
    let name = unescape_pointer(parts.next()?);
    let mut current = defs.get(&name)?;
    for part in parts {
        let part = unescape_pointer(part);
        current = match current {
            serde_json::Value::Object(map) => map.get(&part)?,
            serde_json::Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn unescape_pointer(part: &str) -> String {
    let mut decoded = String::with_capacity(part.len());
    let mut chars = part.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.peek() {
                Some('0') => {
                    decoded.push('~');
                    chars.next();
                }
                Some('1') => {
                    decoded.push('/');
                    chars.next();
                }
                _ => decoded.push('~'),
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::inline_schema_refs;
    use serde_json::json;

    fn text_of(value: &serde_json::Value) -> String {
        value.to_string()
    }

    #[test]
    fn local_refs_are_inlined_and_defs_are_dropped() {
        let mut schema = json!({
            "type": "object",
            "$defs": {
                "EditOp": {
                    "type": "object",
                    "properties": {
                        "occurrence": { "$ref": "#/$defs/Occurrence", "description": "which match" }
                    }
                },
                "Occurrence": { "type": "string", "enum": ["unique", "all", "nth"] }
            },
            "properties": {
                "operations": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/EditOp" }
                }
            }
        });
        inline_schema_refs(&mut schema);
        let text = text_of(&schema);
        assert!(!text.contains("$ref"), "{text}");
        assert!(!text.contains("$defs"), "{text}");
        assert_eq!(
            schema["properties"]["operations"]["items"]["properties"]["occurrence"]["enum"][0],
            "unique"
        );
        assert_eq!(
            schema["properties"]["operations"]["items"]["properties"]["occurrence"]["description"],
            "which match"
        );
    }

    #[test]
    fn a_parameter_named_like_a_def_bucket_survives() {
        let mut schema = json!({
            "type": "object",
            "$defs": { "Name": { "type": "string" } },
            "properties": {
                "definitions": { "type": "array", "items": { "$ref": "#/$defs/Name" } }
            }
        });
        inline_schema_refs(&mut schema);
        assert_eq!(
            schema["properties"]["definitions"]["items"]["type"],
            "string"
        );
        assert!(schema.get("$defs").is_none());

        let plain = json!({"type": "object", "properties": {"path": {"type": "string"}}});
        let mut untouched = plain.clone();
        inline_schema_refs(&mut untouched);
        assert_eq!(untouched, plain);
    }

    #[test]
    fn a_reference_cycle_terminates_without_a_ref() {
        let mut schema = json!({
            "$defs": {
                "A": {
                    "type": "object",
                    "properties": { "child": { "$ref": "#/$defs/B" } }
                },
                "B": {
                    "type": "object",
                    "properties": { "parent": { "$ref": "#/$defs/A" } }
                }
            },
            "$ref": "#/$defs/A"
        });
        inline_schema_refs(&mut schema);
        let text = text_of(&schema);
        assert!(!text.contains("$ref"), "{text}");
        assert!(!text.contains("$defs"), "{text}");
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["child"]["type"], "object");
        assert_eq!(
            schema["properties"]["child"]["properties"]["parent"]["type"],
            "object"
        );
        assert!(
            schema["properties"]["child"]["properties"]["parent"]
                .get("properties")
                .is_none()
        );
    }
}
