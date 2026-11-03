//! Tool specification shared between the tool registry and LLM providers.

use serde::{Deserialize, Serialize};

/// Serializable description of a tool, sent to LLM providers.
///
/// Produced by the tool registry (`crate::tools`) from `schemars`-derived
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
const MAX_EXPANDED_NODES: usize = 16_384;
const MAX_EXPANDED_BYTES: usize = 1024 * 1024;

type Definitions<'a> = std::collections::BTreeMap<&'a str, &'a serde_json::Value>;

struct ExpansionBudget {
    nodes: usize,
    bytes: usize,
}

impl ExpansionBudget {
    fn bytes(&mut self, count: usize) -> Option<()> {
        self.bytes = self.bytes.checked_sub(count)?;
        Some(())
    }

    fn node(&mut self, node: &serde_json::Value) -> Option<()> {
        self.nodes = self.nodes.checked_sub(1)?;
        // Container syntax and separators are charged conservatively;
        // scalar strings include their actual JSON escape cost.
        let bytes = match node {
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => 2,
            _ => crate::core::encoded_json_len(node)?,
        };
        self.bytes(bytes.saturating_add(1))
    }

    fn key(&mut self, key: &str) -> Option<()> {
        self.bytes(crate::core::encoded_json_len(key)?.saturating_add(2))
    }
}

/// Inlines local `$ref`s and drops `$defs` / `definitions`.
///
/// OpenCode `packages/opencode/src/provider/transform.ts` `schema()` sanitizes
/// per host: `sanitizeMoonshot` keeps a bare `$ref` (Moonshot expands it and
/// rejects sibling keywords), and `sanitizeGemini` copies `$ref` through.
/// Moonshot's coding-plan validator still 400s on schemars `$ref` + `$defs`
/// ("infinite recursion" at `operations.items`). Every provider gets the
/// expanded schema instead. A reference that is already on the stack, or
/// deeper than `SCHEMA_REF_DEPTH`, becomes `{type: object}` so a real cycle
/// cannot recurse. A cumulative node/byte budget also bounds branching
/// expansion. If it is exhausted, the original schema and its references
/// remain intact: validation keeps its constraints, and a provider that does
/// not support refs can reject the schema instead of exhausting the process.
pub fn inline_schema_refs(schema: &mut serde_json::Value) {
    // Builtin specs arrive already inlined; providers call this per request,
    // so a schema with nothing to expand is left untouched instead of copied.
    if !has_schema_refs(schema) {
        return;
    }
    let mut defs = Definitions::new();
    collect_defs(schema, &mut defs);
    let mut stack = Vec::new();
    let mut budget = ExpansionBudget {
        nodes: MAX_EXPANDED_NODES,
        bytes: MAX_EXPANDED_BYTES,
    };
    if let Some(expanded) = expand(schema, &defs, &mut stack, &mut budget) {
        *schema = expanded;
    }
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

fn collect_defs<'a>(value: &'a serde_json::Value, defs: &mut Definitions<'a>) {
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
                    defs.entry(name.as_str()).or_insert(schema);
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
    defs: &Definitions<'_>,
    stack: &mut Vec<String>,
    budget: &mut ExpansionBudget,
) -> Option<serde_json::Value> {
    budget.node(node)?;
    let serde_json::Value::Object(object) = node else {
        return match node {
            serde_json::Value::Array(items) => items
                .iter()
                .map(|item| expand(item, defs, stack, budget))
                .collect::<Option<Vec<_>>>()
                .map(serde_json::Value::Array),
            other => Some(other.clone()),
        };
    };
    if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
        if stack.iter().any(|seen| seen == reference) || stack.len() >= SCHEMA_REF_DEPTH {
            budget.bytes(32)?;
            let mut terminated = serde_json::json!({"type": "object"});
            keep_description(object, &mut terminated, budget)?;
            return Some(terminated);
        }
        if let Some(target) = resolve_ref(reference, defs) {
            stack.push(reference.to_owned());
            let expanded = expand(target, defs, stack, budget);
            stack.pop();
            let mut expanded = expanded?;
            keep_description(object, &mut expanded, budget)?;
            return Some(expanded);
        }
    }
    let mut out = serde_json::Map::new();
    for (key, value) in object {
        if key == "$defs" || key == "definitions" || key == "$ref" {
            continue;
        }
        budget.key(key)?;
        let expanded = match value {
            // Property names are not schema keywords.
            serde_json::Value::Object(properties)
                if key == "properties" || key == "patternProperties" =>
            {
                budget.node(value)?;
                let mut expanded = serde_json::Map::new();
                for (name, schema) in properties {
                    budget.key(name)?;
                    expanded.insert(name.clone(), expand(schema, defs, stack, budget)?);
                }
                serde_json::Value::Object(expanded)
            }
            _ => expand(value, defs, stack, budget)?,
        };
        out.insert(key.clone(), expanded);
    }
    Some(serde_json::Value::Object(out))
}

fn keep_description(
    source: &serde_json::Map<String, serde_json::Value>,
    expanded: &mut serde_json::Value,
    budget: &mut ExpansionBudget,
) -> Option<()> {
    if let Some(description) = source.get("description").filter(|value| value.is_string())
        && let Some(object) = expanded.as_object_mut()
        && !object.contains_key("description")
    {
        budget.key("description")?;
        budget.node(description)?;
        object.insert("description".to_owned(), description.clone());
    }
    Some(())
}

fn resolve_ref<'a>(reference: &str, defs: &Definitions<'a>) -> Option<&'a serde_json::Value> {
    let pointer = reference.strip_prefix("#/")?;
    let mut parts = pointer.split('/');
    let bucket = parts.next()?;
    if bucket != "$defs" && bucket != "definitions" {
        return None;
    }
    let name = unescape_pointer(parts.next()?);
    let mut current = *defs.get(name.as_str())?;
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
    fn branching_refs_keep_the_original_constraints_when_expansion_exceeds_budget() {
        let mut defs = serde_json::Map::new();
        defs.insert(
            "level32".into(),
            json!({"type": "string", "enum": ["allowed"]}),
        );
        for level in (0..32).rev() {
            let reference = format!("#/$defs/level{}", level + 1);
            defs.insert(
                format!("level{level}"),
                json!({
                    "type": "object",
                    "properties": {"left": {"$ref": reference}, "right": {"$ref": reference}},
                    "required": ["left", "right"],
                    "additionalProperties": false
                }),
            );
        }
        let original = json!({"$defs": defs, "$ref": "#/$defs/level0"});
        assert!(original.to_string().len() < 10_000);
        let mut expanded = original.clone();
        inline_schema_refs(&mut expanded);
        assert_eq!(
            expanded, original,
            "never replace the constraints with an unrestricted schema"
        );
    }

    #[test]
    fn repeated_large_values_exhaust_the_byte_budget_before_the_node_budget() {
        let mut properties = serde_json::Map::new();
        for index in 0..256 {
            properties.insert(format!("value{index}"), json!({"$ref": "#/$defs/Long"}));
        }
        let original = json!({
            "$defs": {"Long": {"type": "string", "const": "x".repeat(8192)}},
            "type": "object", "properties": properties,
        });
        let mut expanded = original.clone();
        inline_schema_refs(&mut expanded);
        assert_eq!(expanded, original);
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
