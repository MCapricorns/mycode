//! The `Tool` trait. Builtin tools implement it; `ToolDyn` erases the
//! associated argument type for storage in the
//! [`ToolRegistry`](crate::registry::ToolRegistry). See `docs/tools.md`.
//!
//! Single-source schema: `schemars` derives one JSON Schema per tool's
//! `Args`, used both for the LLM tool spec (`ToolSpec::params_schema`) and
//! for runtime argument validation in [`ToolDyn::execute_dyn`].

use async_trait::async_trait;
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
/// `Args` must additionally be `Send`: the returned boxed future (from
/// `async_trait` on the object-safe [`ToolDyn`]) captures the arguments.
#[async_trait]
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

    /// Whether dispatch should bind a local file capability first.
    fn requires_file_preflight(&self) -> bool {
        self.file_access().is_some()
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
    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError>;
}

/// Object-safe, type-erased view of a [`Tool`] for registry storage and
/// dispatch with raw-JSON arguments.
#[async_trait]
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
    /// Whether dispatch should bind a local file capability first.
    fn requires_file_preflight(&self) -> bool {
        self.file_access().is_some()
    }
    /// Validate `args` against the tool's schema, deserialize, and
    /// execute. Wrong-shaped arguments fail with
    /// [`ToolError::InvalidArgs`].
    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError>;
}

/// Generate the JSON Schema of `A`'s arguments (schemars single source).
///
/// Providers reject a root schema that is `true` or carries a draft
/// `$schema` URL, so the advertised parameters are always an object schema.
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

/// Validate raw `args` against the schemars-generated schema of `A`.
pub(crate) fn validate_args<A: JsonSchema>(args: &Value) -> Result<(), ToolError> {
    let validator = jsonschema::validator_for(&args_schema::<A>())
        .map_err(|err| ToolError::Execution(format!("failed to compile argument schema: {err}")))?;
    let errors: Vec<String> = validator
        .iter_errors(args)
        .map(|err| format!("{err} (at {})", err.instance_path()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(errors.join("; ")))
    }
}

/// Blanket type erasure: every [`Tool`] is a [`ToolDyn`]. Deserializes
/// `Value → Args` after validating it against the schemars-generated
/// schema, so wrong-shaped arguments are rejected with
/// [`ToolError::InvalidArgs`] before the tool runs.

#[async_trait]
impl<T: Tool> ToolDyn for T {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            params_schema: args_schema::<T::Args>(),
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

    fn requires_file_preflight(&self) -> bool {
        Tool::requires_file_preflight(self)
    }

    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let args = self.prepare_args(normalize_tool_args(args));
        validate_args::<T::Args>(&args)?;
        let typed =
            serde_json::from_value(args).map_err(|err| ToolError::InvalidArgs(err.to_string()))?;
        self.execute(typed, ctx, out).await
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
    args
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
