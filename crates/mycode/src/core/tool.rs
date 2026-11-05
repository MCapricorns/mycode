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
/// remain intact. Unresolvable references are retained in the same way:
/// validation keeps its constraints, and a provider that does
/// not support refs can reject the schema instead of exhausting the process.
pub fn inline_schema_refs(schema: &mut serde_json::Value) {
    // Builtin specs arrive already inlined; providers call this per request,
    // so a schema with nothing to expand is left untouched instead of copied.
    if !has_schema_refs(schema) {
        return;
    }
    let mut stack = Vec::new();
    let mut budget = ExpansionBudget {
        nodes: MAX_EXPANDED_NODES,
        bytes: MAX_EXPANDED_BYTES,
    };
    if let Some(expanded) = expand(schema, schema, &mut stack, &mut budget) {
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

fn expand(
    node: &serde_json::Value,
    root: &serde_json::Value,
    stack: &mut Vec<String>,
    budget: &mut ExpansionBudget,
) -> Option<serde_json::Value> {
    budget.node(node)?;
    let serde_json::Value::Object(object) = node else {
        return match node {
            serde_json::Value::Array(items) => items
                .iter()
                .map(|item| expand(item, root, stack, budget))
                .collect::<Option<Vec<_>>>()
                .map(serde_json::Value::Array),
            other => Some(other.clone()),
        };
    };
    if let Some(reference) = object.get("$ref") {
        // A malformed reference is invalid schema, not an absent constraint.
        // Leave it for the provider/validator rather than silently accepting it.
        let reference = reference.as_str()?;
        if stack.iter().any(|seen| seen == reference) || stack.len() >= SCHEMA_REF_DEPTH {
            budget.bytes(32)?;
            let mut terminated = serde_json::json!({"type": "object"});
            keep_description(object, &mut terminated, budget)?;
            return Some(terminated);
        }
        // Resolve against the actual root, not a flattened map of definition
        // names: nested buckets can contain the same name. Unknown/external
        // references keep the entire original schema and its definitions.
        let target = root.pointer(reference.strip_prefix('#')?)?;
        stack.push(reference.to_owned());
        let expanded = expand(target, root, stack, budget);
        stack.pop();
        let mut expanded = expanded?;
        keep_description(object, &mut expanded, budget)?;
        let mut siblings = object.clone();
        siblings.remove("$ref");
        siblings.remove("description");
        siblings.remove("$defs");
        siblings.remove("definitions");
        if !siblings.is_empty() {
            let siblings = expand(&serde_json::Value::Object(siblings), root, stack, budget)?;
            // Sibling constraints apply in addition to the referenced schema.
            // Combining with allOf also preserves overlapping required/enum
            // constraints instead of overwriting either side.
            budget.bytes(32)?;
            expanded = serde_json::json!({"allOf": [expanded, siblings]});
        }
        return Some(expanded);
    }
    let mut out = serde_json::Map::new();
    for (key, value) in object {
        if key == "$defs" || key == "definitions" || key == "$ref" {
            continue;
        }
        budget.key(key)?;
        let expanded = match key.as_str() {
            // Values in these maps are schemas; their keys are user names.
            "properties" | "patternProperties" | "dependentSchemas" | "dependencies" => {
                let properties = value.as_object()?;
                budget.node(value)?;
                let mut expanded = serde_json::Map::new();
                for (name, schema) in properties {
                    budget.key(name)?;
                    let schema = if schema.is_array() && key == "dependencies" {
                        clone_literal(schema, budget)?
                    } else {
                        expand(schema, root, stack, budget)?
                    };
                    expanded.insert(name.clone(), schema);
                }
                serde_json::Value::Object(expanded)
            }
            "items"
            | "prefixItems"
            | "additionalItems"
            | "contains"
            | "additionalProperties"
            | "unevaluatedItems"
            | "unevaluatedProperties"
            | "propertyNames"
            | "allOf"
            | "anyOf"
            | "oneOf"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contentSchema" => expand(value, root, stack, budget)?,
            // const, enum, default, examples and extension values are data.
            // A literal {"$ref": ...} in them must never be expanded/dropped.
            _ => clone_literal(value, budget)?,
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

fn clone_literal(
    value: &serde_json::Value,
    budget: &mut ExpansionBudget,
) -> Option<serde_json::Value> {
    budget.bytes(crate::core::encoded_json_len(value)?)?;
    Some(value.clone())
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
}
