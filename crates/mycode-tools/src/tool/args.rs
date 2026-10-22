//! Tool-argument repair: camelCase folding, stringified objects, common
//! aliases, required-field filling, and tagged-union arms, so model-emitted
//! arguments match the snake_case schemas before validation runs.

use serde_json::Value;

/// Accepts camelCase, a JSON object encoded as a string, and a few common
/// aliases so model-emitted tool arguments match the snake_case schemas
/// without failing validation.
pub fn normalize_tool_args(args: Value) -> Value {
    let mut args = unwrap_stringified_object(fold_camel_keys(args));
    let Value::Object(map) = &mut args else {
        return args;
    };
    alias_key(map, "timeout", "timeout_secs");
    alias_key(map, "cmd", "command");
    // Spellings other agents' file tools use. Without them the file
    // preflight fails on a missing `path`, or `edit` reports that no
    // replacement was given, and the model spends a turn on the retry.
    alias_key(map, "file_path", "path");
    alias_key(map, "filepath", "path");
    alias_key(map, "old_str", "old_string");
    alias_key(map, "new_str", "new_string");
    unwrap_echoed_values(map);
    args
}

/// Unwraps arguments that arrive as a JSON object encoded in a string.
///
/// XML tool-call adapters and some gateways stringify the whole argument
/// object; parsing it here keeps every tool on the same path. A string that
/// does not parse as an object is left alone, so validation still reports it.
fn unwrap_stringified_object(args: Value) -> Value {
    let Value::String(text) = &args else {
        return args;
    };
    let trimmed = text.trim();
    if !trimmed.starts_with('{') {
        return args;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ => args,
    }
}

/// Required fields other tools spell differently. Each entry maps the schema
/// name to the spellings seen from other agent frameworks; an alias is only
/// used when the schema requires the schema name, the model did not send it,
/// and the alias is not itself a declared property.
const REQUIRED_ALIASES: &[(&str, &[&str])] = &[
    (
        "path",
        &[
            "file_path",
            "filepath",
            "file",
            "filename",
            "file_name",
            "target_file",
        ],
    ),
    (
        "content",
        &[
            "contents",
            "text",
            "body",
            "data",
            "new_content",
            "file_content",
        ],
    ),
    ("pattern", &["query", "regex", "needle", "search", "glob"]),
    ("command", &["cmd", "script", "shell_command"]),
    (
        "prompt",
        &["instructions", "instruction", "task", "message", "brief"],
    ),
    ("agent", &["name", "role", "agent_name", "subagent"]),
    ("query", &["q", "term", "search"]),
    ("goal", &["objective", "purpose"]),
    ("url", &["link", "href"]),
    ("urls", &["url", "link", "links", "href"]),
];

/// Repairs unambiguous argument shapes that the schema would reject.
///
/// Runs after [`normalize_tool_args`] and before validation, for every tool
/// and for MCP tools that compile the server's schema themselves:
///
/// * a required field sent under another tool's spelling becomes the schema
///   name (`file_path` → `path`, `q` → `query`, `task` → `prompt`);
/// * quoted scalars become the type the schema asks for, at any depth;
/// * an object that is one variant of a tagged union gets the tag its fields
///   imply, and keys the variant does not declare are dropped when the
///   variant rejects unknown fields (`additionalProperties: false`).
///
/// Nothing is invented: an object with no recognizable field, a union
/// without a `const`/single-`enum` tag, and a value that does not parse are
/// left exactly as they arrived, so validation still reports them.
pub fn repair_args(args: &mut Value, schema: &Value) {
    fill_required_aliases(args, schema);
    repair_node(args, schema);
}

fn fill_required_aliases(args: &mut Value, schema: &Value) {
    let Some(map) = args.as_object_mut() else {
        return;
    };
    let Some(required) = schema.get("required").and_then(Value::as_array) else {
        return;
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    for key in required.iter().filter_map(Value::as_str) {
        if map.contains_key(key) {
            continue;
        }
        let Some(aliases) = REQUIRED_ALIASES
            .iter()
            .find(|(target, _)| *target == key)
            .map(|(_, aliases)| *aliases)
        else {
            continue;
        };
        for alias in aliases {
            // A schema property with that name is a real field, not a
            // spelling of this one; moving it would change the meaning.
            if properties.is_some_and(|properties| properties.contains_key(*alias)) {
                continue;
            }
            let Some(value) = map.get(*alias).cloned() else {
                continue;
            };
            let Some(target) = properties.and_then(|properties| properties.get(key)) else {
                continue;
            };
            // A single string satisfies a required list of strings; other
            // shapes are left for validation to report.
            let value = if is_string_array(target) && value.is_string() {
                Value::Array(vec![value])
            } else if value.is_string() || (is_string_array(target) && value.is_array()) {
                value
            } else {
                continue;
            };
            map.remove(*alias);
            map.insert(key.to_owned(), value);
            break;
        }
    }
}

fn is_string_array(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("array")
        && schema
            .pointer("/items/type")
            .and_then(Value::as_str)
            .is_some_and(|item| item == "string")
}

fn repair_node(value: &mut Value, schema: &Value) {
    match value {
        Value::Object(map) => {
            let arms = tagged_arms(schema);
            let chosen = if arms.is_empty() {
                None
            } else {
                select_arm(map, &arms)
            };
            if let Some((arm, tag_key, tag_value)) = chosen {
                adopt_arm(map, arm, tag_key, tag_value);
            }
            for (key, child) in map.iter_mut() {
                let property = chosen
                    .and_then(|(arm, _, _)| arm.get("properties"))
                    .and_then(|properties| properties.get(key))
                    .or_else(|| property_schema(schema, key));
                if let Some(property) = property {
                    repair_node(child, property);
                }
            }
        }
        Value::Array(items) => {
            let Some(items_schema) = schema.get("items") else {
                return;
            };
            for item in items {
                repair_node(item, items_schema);
            }
        }
        Value::String(text) => {
            if let Some(coerced) = coerce_scalar_string(text, schema) {
                *value = coerced;
            }
        }
        _ => {}
    }
}

/// The schema of one property, following `anyOf`/`oneOf`/`allOf` arms so a
/// value inside a union variant is still typed.
fn property_schema<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(property) = schema.get("properties").and_then(|map| map.get(key)) {
        return Some(property);
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        let Some(arms) = schema.get(keyword).and_then(Value::as_array) else {
            continue;
        };
        if let Some(property) = arms.iter().find_map(|arm| property_schema(arm, key)) {
            return Some(property);
        }
    }
    None
}

/// One variant of a tagged union: its schema plus the property and value that
/// identify it.
type TaggedArm<'a> = (&'a Value, &'a str, &'a str);

/// `oneOf`/`anyOf` arms that carry a string discriminator (`const` or a
/// single-value `enum`, preferring the property named `type`).
fn tagged_arms(schema: &Value) -> Vec<TaggedArm<'_>> {
    for keyword in ["oneOf", "anyOf"] {
        let Some(list) = schema.get(keyword).and_then(Value::as_array) else {
            continue;
        };
        let arms: Vec<TaggedArm<'_>> = list
            .iter()
            .filter_map(|arm| {
                let properties = arm.get("properties")?.as_object()?;
                let (key, value) = properties
                    .get("type")
                    .and_then(discriminator_value)
                    .map(|value| ("type", value))
                    .or_else(|| {
                        properties.iter().find_map(|(key, property)| {
                            discriminator_value(property).map(|value| (key.as_str(), value))
                        })
                    })?;
                Some((arm, key, value))
            })
            .collect();
        if !arms.is_empty() {
            return arms;
        }
    }
    Vec::new()
}

fn discriminator_value(property: &Value) -> Option<&str> {
    if let Some(value) = property.get("const").and_then(Value::as_str) {
        return Some(value);
    }
    match property.get("enum").and_then(Value::as_array)?.as_slice() {
        [single] => single.as_str(),
        _ => None,
    }
}

/// Picks the variant an object belongs to: an exact tag wins, a tag that
/// only differs in case/spelling is corrected, and otherwise the variant
/// whose fields cover the object best is chosen.
fn select_arm<'a>(
    map: &serde_json::Map<String, Value>,
    arms: &[TaggedArm<'a>],
) -> Option<TaggedArm<'a>> {
    let tag_key = arms.first()?.1;
    if arms.iter().any(|(_, key, _)| *key != tag_key) {
        return None;
    }
    if let Some(sent) = map.get(tag_key).and_then(Value::as_str) {
        let sent = sent.trim();
        if let Some(arm) = arms.iter().find(|(_, _, value)| *value == sent) {
            return Some(*arm);
        }
        if let Some(arm) = arms
            .iter()
            .find(|(_, _, value)| fold_tag(value) == fold_tag(sent))
        {
            return Some(*arm);
        }
    }
    let mut best: Option<(ArmScore, TaggedArm<'a>)> = None;
    for arm in arms {
        let Some(score) = score_arm(map, arm) else {
            continue;
        };
        if best.as_ref().is_none_or(|(current, _)| score < *current) {
            best = Some((score, *arm));
        }
    }
    best.map(|(_, arm)| arm)
}

/// How well an object fits one variant. `unknown` keys it does not declare
/// must not outnumber the keys it does, so a mostly foreign object is never
/// coerced into a variant; ties fall to the earlier arm, which keeps the
/// declaration order meaningful (`literal` before `regex`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ArmScore {
    unknown: usize,
    missing_required: usize,
    /// More declared keys matched is better, so the order is reversed.
    unmatched_declared: std::cmp::Reverse<usize>,
}

fn score_arm(map: &serde_json::Map<String, Value>, arm: &TaggedArm<'_>) -> Option<ArmScore> {
    let (schema, tag_key, _) = *arm;
    let properties = schema.get("properties")?.as_object()?;
    let mut hits = 0;
    let mut unknown = 0;
    for key in map.keys() {
        if key == tag_key {
            continue;
        }
        if properties.contains_key(key) {
            hits += 1;
        } else {
            unknown += 1;
        }
    }
    if hits == 0 || unknown > hits {
        return None;
    }
    let missing_required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|key| *key != tag_key)
        .filter(|key| !map.contains_key(*key))
        .count();
    Some(ArmScore {
        unknown,
        missing_required,
        unmatched_declared: std::cmp::Reverse(hits),
    })
}

fn adopt_arm(
    map: &mut serde_json::Map<String, Value>,
    arm: &Value,
    tag_key: &str,
    tag_value: &str,
) {
    map.insert(tag_key.to_owned(), Value::String(tag_value.to_owned()));
    let Some(properties) = arm.get("properties").and_then(Value::as_object) else {
        return;
    };
    // Never drop a key while a required field of the variant is still
    // missing: the operation is incomplete, and validation should say so
    // with the original arguments intact.
    let required_ok = arm
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|key| *key != tag_key)
        .all(|key| map.contains_key(key));
    if !required_ok || arm.get("additionalProperties") != Some(&Value::Bool(false)) {
        return;
    }
    let kept: serde_json::Map<String, Value> = std::mem::take(map)
        .into_iter()
        .filter(|(key, _)| properties.contains_key(key))
        .collect();
    *map = kept;
}

/// `Regexp` and `regex` name the same variant.
fn fold_tag(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

/// Turns a string such as `"10"` or `"true"` into the value the schema asks
/// for at this position.
///
/// XML tool calls carry every parameter as text, and some JSON-calling
/// models quote scalars too; without this every `limit`, `timeout_secs`, or
/// `overwrite` they send fails validation. A position that also accepts a
/// string is never touched, and text that does not parse stays as it was so
/// validation still names the bad value.
fn coerce_scalar_string(text: &str, schema: &Value) -> Option<Value> {
    let types = scalar_types(schema);
    if types.is_empty() || types.contains(&"string") {
        return None;
    }
    let text = text.trim();
    if types.contains(&"integer") {
        if let Ok(number) = text.parse::<i64>() {
            return Some(Value::from(number));
        }
        if let Ok(number) = text.parse::<u64>() {
            return Some(Value::from(number));
        }
    }
    if types.contains(&"number")
        && let Ok(number) = text.parse::<f64>()
        && number.is_finite()
    {
        return Some(Value::from(number));
    }
    if types.contains(&"boolean") {
        return match text.to_ascii_lowercase().as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        };
    }
    None
}

/// The JSON `type` names a property schema allows, including those of
/// `anyOf`/`oneOf` arms (schemars spells `Option<T>` either way).
fn scalar_types(property: &Value) -> Vec<&str> {
    let mut types = Vec::new();
    match property.get("type") {
        Some(Value::String(name)) => types.push(name.as_str()),
        Some(Value::Array(names)) => types.extend(names.iter().filter_map(Value::as_str)),
        _ => {}
    }
    for keyword in ["anyOf", "oneOf"] {
        for arm in property
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            types.extend(scalar_types(arm));
        }
    }
    types
}

/// Unwraps `{key: {key: value}}` into `{key: value}`.
///
/// Models occasionally echo the parameter name inside the value — the
/// observed case was shell `mode={"mode":"program"}`, which schema
/// validation reports as an opaque `oneOf` failure. A value that is an
/// object holding exactly the same single key with a non-object value is
/// the echoed shape; anything else passes through untouched.
fn unwrap_echoed_values(map: &mut serde_json::Map<String, Value>) {
    for (key, value) in map.iter_mut() {
        let echoed = value.as_object().is_some_and(|inner| {
            inner.len() == 1
                && inner.iter().next().is_some_and(|(inner_key, inner_value)| {
                    inner_key.as_str() == key.as_str() && !inner_value.is_object()
                })
        });
        if !echoed {
            continue;
        }
        let unwrapped = value
            .as_object_mut()
            .and_then(|inner| inner.values_mut().next().map(std::mem::take));
        if let Some(unwrapped) = unwrapped {
            *value = unwrapped;
        }
    }
}

fn alias_key(map: &mut serde_json::Map<String, Value>, from: &str, to: &str) {
    if map.contains_key(to) {
        return;
    }
    if let Some(value) = map.remove(from) {
        map.insert(to.to_owned(), value);
    }
}

fn fold_camel_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                let snake = camel_to_snake(&key);
                let folded = fold_camel_keys(child);
                if !out.contains_key(&snake) {
                    out.insert(snake, folded);
                } else if snake != key {
                    // An explicit snake_case field already occupies the slot.
                    // Keep the original camelCase key instead of dropping it.
                    out.entry(key).or_insert(folded);
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(fold_camel_keys).collect()),
        other => other,
    }
}

fn camel_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
