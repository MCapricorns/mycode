//! The `Tool` trait. Builtin tools implement it; `ToolDyn` erases the
//! associated argument type for storage in the
//! [`ToolRegistry`](crate::registry::ToolRegistry). See `docs/tools.md`.
//!
//! Single-source schema: `schemars` derives one JSON Schema per tool's
//! `Args`, used both for the LLM tool spec (`ToolSpec::params_schema`) and
//! for runtime argument validation in [`ToolDyn::execute_dyn`].

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use mycode_core::message::{ContentBlock, TextBlock};
use mycode_core::tool::ToolSpec;

use crate::builtin::fs_io::FileAccess;
use crate::builtin::fs_search::SearchAccess;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;

/// The outcome of a tool execution as seen by the model and the UI.
///
/// Mirrors `ToolResultMessage` in `mycode-core`: `content` goes back to the
/// LLM, `details` is UI-only (structured diffs, byte counts, …) and never
/// enters model context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content visible to the model.
    pub content: Vec<ContentBlock>,
    /// A tool-level failure reported *as data* (non-zero exit, failed
    /// match, …). Distinct from `Err(ToolError)`, which signals the
    /// dispatcher should synthesize an error result.
    pub is_error: bool,
    /// Structured information for the UI layer only; not sent to the LLM.
    pub details: Option<Value>,
}

impl ToolResult {
    /// A successful result with a single text block.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextBlock::new(content))],
            is_error: false,
            details: None,
        }
    }

    /// A result whose content reports a failure (`is_error: true`).
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextBlock::new(message))],
            is_error: true,
            details: None,
        }
    }

    /// Attach UI-only details (builder style).
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Appended to every user-interrupt error. The cancelled result is
/// committed to the session ledger and replays verbatim on the next turn,
/// so this note is what tells the model the interrupt did not roll back
/// tool calls that already completed.
pub const INTERRUPTED_NOTE: &str =
    "; interrupted by user — completed tool calls were applied and are not rolled back";

/// Failure modes of a tool invocation. The dispatcher (agent loop)
/// converts these into `is_error` tool results for the model rather than
/// crashing the loop. See `docs/agent.md`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {

    /// Arguments failed schema validation or are semantically invalid.
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// The tool ran but failed (missing file, I/O error, …).
    #[error("{0}")]
    Execution(String),
    /// The tool implementation panicked.
    #[error("plugin trap: {0}")]
    PluginTrap(String),
}

impl From<std::io::Error> for ToolError {
    fn from(err: std::io::Error) -> Self {
        Self::Execution(err.to_string())
    }
}

/// A tool with typed arguments. Builtin tools and Rust-side plugins
/// implement this; the blanket [`ToolDyn`] impl type-erases it.
///
/// `execute` returns an `impl Future` pinned by trait, so implementers
/// write plain `async fn`; `Args` must additionally be `Send` because the
/// future captures the arguments and the object-safe [`ToolDyn`] boxes it.
pub trait Tool: Send + Sync + 'static {
    /// Typed, schema-derived arguments. The schemars-generated schema is
    /// the single source for both the tool spec and runtime validation.
    type Args: DeserializeOwned + JsonSchema + Send;
    /// Typed output payload, reserved for renderer integration.
    type Output: Serialize;

    /// Unique tool name (registry key; last registration wins).
    fn name(&self) -> &str;
    /// What the tool does, sent to the model in the tool spec.
    fn description(&self) -> &str;
    /// Optional usage hint injected into the system prompt (pi's
    /// `promptSnippet`). `None` by default.
    fn prompt_snippet(&self) -> Option<&str> {
        None
    }

    /// JSON Schema sent to the model. The default is the schemars schema of
    /// [`Self::Args`]. The shell tool replaces parameter descriptions so they
    /// match the one interpreter the model is allowed to use.
    fn params_schema(&self) -> Value {
        args_schema::<Self::Args>()
    }

    /// Access mode for a retained local search root.
    ///
    /// Built-in `grep` returns content access and `find` metadata access.
    /// Plugin overrides stay off by default and are not forced through
    /// filesystem preflight.
    fn search_access(&self) -> Option<SearchAccess> {
        None
    }

    /// Whether dispatch should bind a local search root first.
    fn requires_search_preflight(&self) -> bool {
        self.search_access().is_some()
    }

    /// Access mode for a retained local file capability.
    ///
    /// Built-in `read` and `edit` return existing-content access and `write`
    /// returns existing-or-missing access. Plugin overrides stay off by
    /// default and are not forced through filesystem preflight.
    fn file_access(&self) -> Option<FileAccess> {
        None
    }

    /// Whether the tool only reads through the prepared file capability.
    ///
    /// Read-only content tools may anchor absolute paths against
    /// host-published read roots outside the workspace (the skills
    /// catalog); `edit` and `write` never do, so those roots stay
    /// unwritable.
    fn reads_only(&self) -> bool {
        false
    }

    /// Whether dispatch should bind a local file capability first.
    fn requires_file_preflight(&self) -> bool {
        self.file_access().is_some()
    }

    /// Whether calls to this tool may run at the same time as other calls
    /// that are also parallel-safe.
    ///
    /// True only for tools that change nothing: content reads and searches
    /// by default, plus tools that opt in. Anything that writes, spawns a
    /// process, or asks the user runs alone, in the model's order.
    fn parallel_safe(&self) -> bool {
        self.reads_only() || self.search_access().is_some()
    }

    /// Rewrites model-emitted arguments into the schema shape before validation.
    ///
    /// The default leaves arguments unchanged. `ask_user` uses this so XML
    /// tool-call shapes (stringified JSON, choice lists, and `multiple`
    /// aliases) pass the same schema as a native JSON call.
    fn prepare_args(&self, args: Value) -> Value {
        args
    }

    /// Execute the tool. Tools may stream progress through `out`; the returned
    /// result is the terminal outcome claimed by the dispatcher.
    fn execute<'a>(
        &'a self,
        args: Self::Args,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> impl Future<Output = Result<ToolResult, ToolError>> + Send + 'a;
}

/// Object-safe, type-erased view of a [`Tool`] for registry storage and
/// dispatch with raw-JSON arguments.
///
/// `execute_dyn` returns a boxed future so the trait stays object-safe;
/// implementers write `Box::pin(async move { … })`.
pub trait ToolDyn: Send + Sync {
    /// The tool spec (name, description, JSON Schema of arguments) as
    /// sent to LLM providers.
    fn spec(&self) -> ToolSpec;
    /// Returns the optional compact usage hint through type erasure.
    fn prompt_snippet_dyn(&self) -> Option<&str> {
        None
    }
    /// Access mode for a retained local search root.
    fn search_access(&self) -> Option<SearchAccess> {
        None
    }
    /// Whether dispatch should bind a local search root first.
    fn requires_search_preflight(&self) -> bool {
        self.search_access().is_some()
    }
    /// Access mode for a retained local file capability.
    fn file_access(&self) -> Option<FileAccess> {
        None
    }
    /// Whether the tool only reads the prepared file.
    fn reads_only(&self) -> bool {
        false
    }
    /// Whether dispatch should bind a local file capability first.
    fn requires_file_preflight(&self) -> bool {
        self.file_access().is_some()
    }
    /// Whether calls may run alongside other parallel-safe calls.
    fn parallel_safe(&self) -> bool {
        false
    }
    /// Validate `args` against the tool's schema (`Self::spec().params_schema`),
    /// deserialize, and execute. Wrong-shaped arguments fail with
    /// [`ToolError::InvalidArgs`].
    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>>;
}

/// Generate the JSON Schema of `A`'s arguments (schemars single source).
///
/// Providers reject a root schema that is `true` or carries a draft
/// `$schema` or `$id`. Boolean `true` becomes an object schema. A root
/// object with no `type`, `oneOf`, `anyOf`, or `allOf` is marked
/// `type: object` (`$ref` cannot survive inlining). Any other root shape is
/// left as schemars emitted it, after `$schema` and `$id` are removed.
pub(crate) fn args_schema<A: JsonSchema>() -> Value {
    let mut value = serde_json::to_value(schemars::schema_for!(A))
        .expect("schemars schemas always serialize to JSON");
    normalize_tool_schema(&mut value);
    value
}

fn normalize_tool_schema(value: &mut Value) {
    if value.as_bool() == Some(true) {
        *value = serde_json::json!({"type": "object", "additionalProperties": true});
        return;
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
        object.remove("$id");
    }
    // Same expansion the wire adapters apply, so a builtin spec and the
    // request body agree. See `mycode_core::inline_schema_refs`.
    mycode_core::inline_schema_refs(value);
    if let Some(object) = value.as_object_mut() {
        object.remove("$schema");
        if !object.contains_key("type")
            && !object.contains_key("oneOf")
            && !object.contains_key("anyOf")
            && !object.contains_key("allOf")
        {
            object.insert("type".to_owned(), Value::String("object".to_owned()));
        }
    }
}

/// Validate raw `args` against an arbitrary JSON Schema value.
pub(crate) fn validate_args_with_schema(schema: &Value, args: &Value) -> Result<(), ToolError> {
    let mut normalized = schema.clone();
    normalize_tool_schema(&mut normalized);
    let validator = jsonschema::validator_for(&normalized)
        .map_err(|err| ToolError::Execution(format!("failed to compile argument schema: {err}")))?;
    let errors: Vec<String> = validator
        .iter_errors(args)
        .map(|err| render_schema_error(&err, &normalized))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(errors.join("; ")))
    }
}

/// One validation failure rendered for the model. The stock Display of a
/// `oneOf`/`anyOf` failure embeds the entire offending value — with edit
/// operations that is hundreds of bytes of multiline source text — so it
/// is replaced by the allowed `type` variants, or by "missing field `type`"
/// when every branch's complaint agrees the tag is absent.
fn render_schema_error(err: &jsonschema::ValidationError<'_>, schema: &Value) -> String {
    let path = err.instance_path().to_string();
    let path = if path.is_empty() { "/" } else { &path };
    match err.kind() {
        jsonschema::error::ValidationErrorKind::OneOfNotValid { context }
        | jsonschema::error::ValidationErrorKind::AnyOf { context } => {
            let variants = tag_variants(schema, err);
            let missing_tag = !context.is_empty()
                && context.iter().all(|arm| {
                    arm.iter().any(|sub| {
                        matches!(
                            sub.kind(),
                            jsonschema::error::ValidationErrorKind::Required { property }
                                if property.as_str() == Some("type")
                        )
                    })
                });
            if missing_tag {
                if variants.is_empty() {
                    format!("value at {path} is missing required field `type`")
                } else {
                    format!(
                        "value at {path} is missing required field `type` (expected one of: {})",
                        variants.join(", ")
                    )
                }
            } else if variants.is_empty() {
                let detail = context
                    .iter()
                    .flatten()
                    .next()
                    .map(|sub| format!(": {}", truncate_error(&sub.to_string())))
                    .unwrap_or_default();
                format!("value at {path} does not match any allowed shape{detail}")
            } else {
                format!(
                    "value at {path} does not match any allowed shape (expected one of: {})",
                    variants.join(", ")
                )
            }
        }
        _ => format!("{} (at {path})", truncate_error(&err.to_string())),
    }
}

/// `const`/`enum` tag values of a `oneOf`/`anyOf` arm list, resolved
/// through `err.schema_path` (which points at the failing keyword).
fn tag_variants(schema: &Value, err: &jsonschema::ValidationError<'_>) -> Vec<String> {
    let Some(arms) = schema
        .pointer(&err.schema_path().to_string())
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    arms.iter()
        .filter_map(|arm| {
            arm.pointer("/properties/type/const")
                .and_then(Value::as_str)
                .or_else(|| {
                    arm.pointer("/properties/type/enum/0")
                        .and_then(Value::as_str)
                })
                .map(str::to_owned)
        })
        .collect()
}

/// Kinds like `enum`/`const`/`pattern` embed the whole instance in their
/// Display; cap each rendered error so a multi-kilobyte argument cannot
/// drown the actual complaint.
fn truncate_error(message: &str) -> String {
    const MAX_ERROR_BYTES: usize = 320;
    if message.len() <= MAX_ERROR_BYTES {
        message.to_owned()
    } else {
        let mut end = MAX_ERROR_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &message[..end])
    }
}

/// Validate raw `args` against the schemars-generated schema of `A`.
#[cfg(test)]
pub(crate) fn validate_args<A: JsonSchema>(args: &Value) -> Result<(), ToolError> {
    validate_args_with_schema(&args_schema::<A>(), args)
}

/// Blanket type erasure: every [`Tool`] is a [`ToolDyn`]. Deserializes
/// `Value → Args` after validating it against `self.params_schema()`,
/// so wrong-shaped arguments are rejected with
/// [`ToolError::InvalidArgs`] before the tool runs.
impl<T: Tool> ToolDyn for T {
    fn spec(&self) -> ToolSpec {
        let mut params_schema = self.params_schema();
        normalize_tool_schema(&mut params_schema);
        ToolSpec {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            params_schema,
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Tool::prompt_snippet(self)
    }

    fn search_access(&self) -> Option<SearchAccess> {
        Tool::search_access(self)
    }

    fn requires_search_preflight(&self) -> bool {
        Tool::requires_search_preflight(self)
    }

    fn file_access(&self) -> Option<FileAccess> {
        Tool::file_access(self)
    }

    fn reads_only(&self) -> bool {
        Tool::reads_only(self)
    }

    fn requires_file_preflight(&self) -> bool {
        Tool::requires_file_preflight(self)
    }

    fn parallel_safe(&self) -> bool {
        Tool::parallel_safe(self)
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let args = self.prepare_args(normalize_tool_args(args));
            let schema = self.params_schema();
            validate_args_with_schema(&schema, &args)?;
            let typed = serde_json::from_value(args)
                .map_err(|err| ToolError::InvalidArgs(err.to_string()))?;
            self.execute(typed, ctx, out).await
        })
    }
}

/// Accepts camelCase and a few common aliases so model-emitted tool
/// arguments match the snake_case schemas without failing validation.
pub(crate) fn normalize_tool_args(args: Value) -> Value {
    let mut args = fold_camel_keys(args);
    let Value::Object(map) = &mut args else {
        return args;
    };
    alias_key(map, "timeout", "timeout_secs");
    alias_key(map, "cmd", "command");
    unwrap_echoed_values(map);
    args
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

#[cfg(test)]
mod tests {
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
}
