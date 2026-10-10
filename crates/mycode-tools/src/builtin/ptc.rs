//! `run_code` — programmatic tool calling.
//!
//! The model writes one Python program. The program calls the same tools a
//! direct call would, and only `return` / `print` re-enter the conversation.
//! This follows DeepSeek Harness PTC (`run_code` plus `tools.name(args)`,
//! fresh per run, read-only calls may overlap) without hiding the single-call
//! tools. See `docs/tools.md`.

mod eval;
mod parse;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mycode_core::message::ContentBlock;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::builtin::truncate_bytes;
use crate::ctx::ToolCtx;
use crate::registry::ToolCatalog;
use crate::roots::anchor_tool_path;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolDyn, ToolError, ToolResult};

use eval::execute;
use parse::parse;

/// Example `description` embedded in the system prompt. Tests run the
/// matching program so the example cannot drift from the interpreter.
pub const RUN_CODE_EXAMPLE_DESCRIPTION: &str = "List rust files that mention dispatch";

/// Example program body. Top-level `await` and `return`, one grep, a filter.
///
/// This is Python executed by the embedded interpreter. It does not start
/// Node.js or a system Python.
pub const RUN_CODE_EXAMPLE_CODE: &str = "\
hits = await tools.grep(pattern=\"dispatch_tool\", include=\"*.rs\")\n\
return \"\\n\".join([line for line in hits.split(\"\\n\") if \"turn.rs\" in line][:20])\n";

/// Largest program, in characters.
const MAX_CODE_CHARS: usize = 16_000;
/// Largest combined log and return value sent back to the model.
pub(super) const MAX_OUTPUT_BYTES: usize = 8_000;
/// Tool calls inside one program.
const MAX_CALLS: u32 = 48;
/// Read-only calls that may overlap inside `gather`.
pub(super) const MAX_PARALLEL: usize = 8;
/// `print` lines kept.
pub(super) const MAX_LOG_LINES: usize = 32;
/// Wall clock for the program, including tool waits.
const MAX_WALL: Duration = Duration::from_secs(120);

/// The `run_code` builtin.
#[derive(Debug, Default)]
pub struct RunCodeTool;

/// Arguments for [`RunCodeTool`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunCodeArgs {
    /// UI label. The schema text is what the model sees, not this comment.
    #[schemars(
        description = "Clear, concise description of what this program does, in active voice, 5-10 words (shown in the UI). Example: \"List rust files that mention dispatch\"."
    )]
    pub description: String,
    /// Program body. The schema text is what the model sees.
    #[schemars(
        description = "The program: the body of an async Python function, executed inside mycode. Top-level await and return work. No import. Python does not need to be installed. Call tools as await tools.name(arg=value)."
    )]
    pub code: String,
}

#[async_trait]
impl Tool for RunCodeTool {
    type Args = RunCodeArgs;
    type Output = ();

    fn name(&self) -> &str {
        "run_code"
    }

    fn description(&self) -> &str {
        "Use this instead of several direct calls when one step reads, searches, \
         or edits more than one file, or fetches more than one page. A rename \
         or the same edit across files is one program here, not one `edit` per \
         file. One obvious call stays direct (a single read, grep, or edit). \
         Takes two required arguments: `description` (5-10 words, active voice, \
         shown in the UI) and `code` (the body of an async Python function; \
         top-level `await` and `return` work; no import). The interpreter is \
         inside mycode: Node.js is not used, and Python does not need to be \
         installed. Call tools as `await tools.name(arg=value)`. \
         `await gather(...)` overlaps read-only calls. Only what you `return` \
         or `print` comes back. Do not call `run_code` from inside the program. \
         Stops after 48 tool calls, 20000 steps, or 120 seconds. Output cap \
         8000 bytes."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "run_code: pass description and code. Use it when one step needs several reads, greps, finds, edits, or page fetches, including a rename across files. only print and return come back. One obvious call stays direct.",
        )
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let description = args.description.trim();
        if description.is_empty() {
            return Err(ToolError::InvalidArgs("description is required".to_owned()));
        }
        if description.chars().count() > 200 {
            return Err(ToolError::InvalidArgs(
                "description exceeds 200 characters".to_owned(),
            ));
        }
        let code = args.code.trim();
        if code.is_empty() {
            return Err(ToolError::InvalidArgs("code is required".to_owned()));
        }
        if code.chars().count() > MAX_CODE_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "code exceeds {MAX_CODE_CHARS} characters"
            )));
        }
        let Some(catalog) = ctx.catalog.clone() else {
            return Err(ToolError::Execution(
                "run_code has no tool catalog".to_owned(),
            ));
        };
        let notes = Arc::new(Mutex::new(Vec::new()));
        let limits = Limits {
            cancel: ctx.cancel.clone(),
            started: Instant::now(),
            calls: Arc::new(AtomicU32::new(0)),
            notes: Arc::clone(&notes),
        };
        let parsed = match parse(code) {
            Ok(program) => program,
            Err(message) => {
                return Ok(failure("syntax", &message, &[], &notes, description));
            }
        };
        let outcome = execute(&parsed, &catalog, ctx, out, &limits).await;
        match outcome {
            Ok(done) => Ok(success(description, done.logs, done.value, &notes)),
            Err(failed) => Ok(failure(
                failed.thrown.kind,
                &failed.thrown.message,
                &failed.logs,
                &notes,
                description,
            )),
        }
    }
}

#[derive(Clone)]
pub(super) struct Limits {
    cancel: tokio_util::sync::CancellationToken,
    started: Instant,
    calls: Arc<AtomicU32>,
    notes: Arc<Mutex<Vec<CallNote>>>,
}

impl Limits {
    fn check(&self) -> Result<(), Thrown> {
        if self.cancel.is_cancelled() {
            return Err(Thrown::abort("program cancelled"));
        }
        if self.started.elapsed() > MAX_WALL {
            return Err(Thrown::timeout("program exceeded 120 seconds"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(super) struct Thrown {
    pub tool_name: Option<String>,
    pub message: String,
    pub kind: &'static str,
}

impl Thrown {
    fn script(message: impl Into<String>) -> Self {
        Self {
            tool_name: None,
            message: message.into(),
            kind: "exception",
        }
    }

    fn tool(name: &str, message: impl Into<String>) -> Self {
        Self {
            tool_name: Some(name.to_owned()),
            message: message.into(),
            kind: "exception",
        }
    }

    fn budget(message: impl Into<String>) -> Self {
        Self {
            tool_name: None,
            message: message.into(),
            kind: "budget",
        }
    }

    fn abort(message: impl Into<String>) -> Self {
        Self {
            tool_name: None,
            message: message.into(),
            kind: "abort",
        }
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self {
            tool_name: None,
            message: message.into(),
            kind: "timeout",
        }
    }

    fn catchable(&self) -> bool {
        self.kind == "exception"
    }
}

#[derive(Clone, Debug)]
struct CallNote {
    ordinal: u32,
    tool: String,
    ok: bool,
    bytes: usize,
}

pub(super) fn is_concurrency_safe(name: &str) -> bool {
    matches!(
        name,
        "read" | "grep" | "find" | "web_search" | "fetch_content"
    )
}

pub(super) async fn invoke_tool(
    catalog: &ToolCatalog,
    parent: &ToolCtx,
    out: &ToolStream,
    limits: &Limits,
    name: &str,
    args: Value,
) -> Result<String, Thrown> {
    limits.check()?;
    let ordinal = limits.calls.fetch_add(1, Ordering::Relaxed);
    if ordinal >= MAX_CALLS {
        return Err(Thrown::budget(format!(
            "program exceeded {MAX_CALLS} tool calls"
        )));
    }
    if name == "run_code" {
        return Err(Thrown::tool(
            name,
            "run_code cannot be called from inside a program",
        ));
    }
    let Some(tool) = catalog.get(name) else {
        let available = catalog
            .names()
            .into_iter()
            .filter(|registered| registered != "run_code")
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Thrown::tool(
            name,
            format!("unknown tool {name}; available: {available}"),
        ));
    };
    let args = crate::tool::normalize_tool_args(args);
    let target = mycode_core::tool_target(name, &args);
    let _ = out.progress(format!("{name} {target}").trim().to_owned());
    let ctx = match child_ctx(parent, tool.as_ref(), &args, name).await {
        Ok(ctx) => ctx,
        Err(thrown) => {
            record(limits, ordinal, name, false, 0);
            return Err(thrown);
        }
    };
    let mut stream = ToolStream::closed();
    let result = match tool.execute_dyn(args, &ctx, &mut stream).await {
        Ok(result) => result,
        Err(error) => {
            record(limits, ordinal, name, false, 0);
            return Err(Thrown::tool(name, error.to_string()));
        }
    };
    let text = script_text(name, &result);
    record(limits, ordinal, name, !result.is_error, text.len());
    if result.is_error {
        Err(Thrown::tool(name, text))
    } else {
        Ok(text)
    }
}

fn record(limits: &Limits, ordinal: u32, tool: &str, ok: bool, bytes: usize) {
    limits
        .notes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(CallNote {
            ordinal,
            tool: tool.to_owned(),
            ok,
            bytes,
        });
}

async fn child_ctx(
    parent: &ToolCtx,
    tool: &dyn ToolDyn,
    args: &Value,
    name: &str,
) -> Result<ToolCtx, Thrown> {
    let mut ctx = ToolCtx::new(parent.cwd.clone())
        .with_cancel(parent.cancel.clone())
        .with_call_id(format!("{}:ptc", parent.call_id))
        .with_extra_roots(parent.extra_roots.clone());
    if let Some(access) = tool.search_access() {
        let path = args.get("path").and_then(Value::as_str).map(str::to_owned);
        let (cwd, path) = anchored_search(&parent.cwd, &parent.extra_roots, path.as_deref());
        let prepared =
            crate::prepare_search_async_with_access(cwd, path, parent.cancel.clone(), access)
                .await
                .map_err(|error| Thrown::tool(name, error.to_string()))?;
        ctx = ctx.with_prepared_search(Arc::new(prepared));
    } else if let Some(access) = tool.file_access() {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Thrown::tool(name, "file tool is missing a path argument"))?;
        let (cwd, path) = anchored_file(&parent.cwd, &parent.extra_roots, path);
        let prepared = crate::prepare_file_async(cwd, path, parent.cancel.clone(), access)
            .await
            .map_err(|error| Thrown::tool(name, error.to_string()))?;
        ctx = ctx.with_prepared_file(Arc::new(prepared));
    }
    Ok(ctx)
}

fn anchored_search(cwd: &Path, extras: &[PathBuf], raw: Option<&str>) -> (PathBuf, Option<String>) {
    let Some(raw) = raw else {
        return (cwd.to_path_buf(), None);
    };
    let (root, relative) = anchor_tool_path(cwd, extras, raw);
    if root == cwd {
        return (cwd.to_path_buf(), Some(raw.to_owned()));
    }
    if relative.is_empty() {
        (root, None)
    } else {
        (root, Some(relative))
    }
}

fn anchored_file(cwd: &Path, extras: &[PathBuf], raw: &str) -> (PathBuf, String) {
    let (root, relative) = anchor_tool_path(cwd, extras, raw);
    if root == cwd {
        (cwd.to_path_buf(), raw.to_owned())
    } else if relative.is_empty() {
        (root, ".".to_owned())
    } else {
        (root, relative)
    }
}

fn result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Text the program sees. `read` keeps the revision in UI details; the
/// script gets the file bytes without the trailing `[revision ...]` tag.
fn script_text(name: &str, result: &ToolResult) -> String {
    let text = result_text(result);
    if name == "read" {
        strip_revision_tag(&text)
    } else {
        text
    }
}

fn strip_revision_tag(text: &str) -> String {
    let Some(index) = text.rfind("\n[revision ") else {
        if revision_tag_line(text) {
            return String::new();
        }
        return text.to_owned();
    };
    let tail = &text[index + 1..];
    if revision_tag_line(tail) {
        text[..index].to_owned()
    } else {
        text.to_owned()
    }
}

fn revision_tag_line(text: &str) -> bool {
    text.starts_with("[revision ") && text.ends_with(']') && !text.contains('\n')
}

fn success(
    description: &str,
    logs: Vec<String>,
    value: Option<String>,
    notes: &Mutex<Vec<CallNote>>,
) -> ToolResult {
    let mut body = String::new();
    if !logs.is_empty() {
        body.push_str(&logs.join("\n"));
    }
    if let Some(value) = value.filter(|text| text != "null") {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&value);
    }
    if body.is_empty() {
        body.push_str("(run_code completed with no output)");
    }
    let (body, truncated) = cap_output(body);
    ToolResult::text(body).with_details(details(description, notes, truncated))
}

fn failure(
    kind: &str,
    message: &str,
    logs: &[String],
    notes: &Mutex<Vec<CallNote>>,
    description: &str,
) -> ToolResult {
    let mut body = String::new();
    if !logs.is_empty() {
        body.push_str(&logs.join("\n"));
        body.push('\n');
    }
    body.push_str(&format!("Error: code run failed ({kind}): {message}"));
    let (body, truncated) = cap_output(body);
    ToolResult::error(body).with_details(details(description, notes, truncated))
}

fn cap_output(text: String) -> (String, bool) {
    let (cut, truncated) = truncate_bytes(&text, MAX_OUTPUT_BYTES.saturating_sub(80));
    if !truncated {
        return (text, false);
    }
    let mut body = cut;
    body.push_str("\n(output truncated; return a smaller value)");
    (body, true)
}

fn details(description: &str, notes: &Mutex<Vec<CallNote>>, truncated: bool) -> Value {
    let mut calls = notes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    calls.sort_by_key(|note| note.ordinal);
    json!({
        "description": description,
        "truncated": truncated,
        "calls": calls.into_iter().map(|note| json!({
            "tool": note.tool,
            "ok": note.ok,
            "bytes": note.bytes,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::RunCodeTool;
    use crate::builtin::{EditTool, FindTool, GrepTool, ReadTool};
    use crate::ctx::ToolCtx;
    use crate::registry::{ToolCatalog, ToolRegistry};
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolResult};

    fn text_of(result: &ToolResult) -> String {
        super::result_text(result)
    }

    fn catalog(tools: Vec<Arc<dyn crate::tool::ToolDyn>>) -> ToolCatalog {
        let registry = ToolRegistry::new();
        for tool in tools {
            registry.register(tool);
        }
        ToolCatalog::from_registry(&registry)
    }

    async fn run(dir: &std::path::Path, tools: ToolCatalog, code: &str) -> ToolResult {
        let ctx = ToolCtx::new(dir).with_catalog(tools);
        let mut stream = ToolStream::closed();
        RunCodeTool
            .execute(
                super::RunCodeArgs {
                    description: "test".to_owned(),
                    code: code.to_owned(),
                },
                &ctx,
                &mut stream,
            )
            .await
            .expect("run_code")
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mycode-ptc-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("temp");
        root
    }

    #[tokio::test]
    async fn filters_grep_hits_so_only_the_return_value_comes_back() {
        let root = temp_dir("grep");
        std::fs::write(
            root.join("turn.rs"),
            "fn dispatch_tool() {}\nfn other() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("skip.rs"), "fn unrelated() {}\n").unwrap();
        let tools = catalog(vec![Arc::new(GrepTool)]);
        let result = run(&root, tools, super::RUN_CODE_EXAMPLE_CODE).await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("turn.rs"), "{text}");
        assert!(text.contains("dispatch_tool"), "{text}");
        assert!(!text.contains("unrelated"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn promise_all_reads_two_files_and_returns_both() {
        let root = temp_dir("all");
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("b.txt"), "beta\n").unwrap();
        let tools = catalog(vec![Arc::new(ReadTool)]);
        let result = run(
            &root,
            tools,
            "\
a, b = await gather(tools.read(path=\"a.txt\", limit=1), tools.read(path=\"b.txt\", limit=1))\n\
return a + \"\\n\" + b\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.contains("alpha"), "{text}");
        assert!(text.contains("beta"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn edit_inside_the_program_publishes_once() {
        let root = temp_dir("edit");
        std::fs::write(root.join("a.txt"), "foo\n").unwrap();
        let tools = catalog(vec![Arc::new(EditTool), Arc::new(ReadTool)]);
        let result = run(
            &root,
            tools,
            "\
await tools.edit(path=\"a.txt\", old_string=\"foo\", new_string=\"bar\")\n\
return \"edited\"\n",
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "bar\n"
        );
        assert_eq!(text, "edited");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_missing_file_is_catchable() {
        let root = temp_dir("catch");
        let tools = catalog(vec![Arc::new(ReadTool)]);
        let result = run(
            &root,
            tools,
            r#"try:
    await tools.read(path="missing.txt")
    return "no"
except Exception as e:
    return e.toolName + ": " + e.message
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(text.starts_with("read:"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn nested_run_code_is_rejected() {
        let root = temp_dir("nest");
        let tools = catalog(vec![Arc::new(RunCodeTool), Arc::new(ReadTool)]);
        let result = run(
            &root,
            tools,
            "await tools.run_code(description=\"inner\", code=\"return 1\")\n",
        )
        .await;
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("cannot be called"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_read_only_catalog_cannot_edit() {
        let root = temp_dir("ro");
        std::fs::write(root.join("a.txt"), "keep\n").unwrap();
        let tools = catalog(vec![
            Arc::new(ReadTool),
            Arc::new(GrepTool),
            Arc::new(FindTool),
        ]);
        let result = run(
            &root,
            tools,
            "await tools.edit(path=\"a.txt\", old_string=\"keep\", new_string=\"no\")\n",
        )
        .await;
        assert!(result.is_error, "{}", text_of(&result));
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "keep\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn syntax_errors_name_the_subset() {
        let root = temp_dir("syn");
        let result = run(&root, catalog(vec![]), "import os\n").await;
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("import"), "{text}");
        assert!(text.contains("does not need to be installed"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn common_python_covers_unpack_assign_and_repeat() {
        let root = temp_dir("py");
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(root.join("b.txt"), "beta\n").unwrap();
        let tools = catalog(vec![Arc::new(ReadTool)]);
        let result = run(
            &root,
            tools,
            r#"a, b = await gather(tools.read(path="a.txt", limit=1), tools.read(path="b.txt", limit=1))
seen = {}
seen["a"] = a.strip()
items = ["x", "y"]
items[0] = "z"
count = 0
count += 2
label = "S" * 4
pairs = []
for i, item in enumerate(items):
    pairs.append(str(int(i)) + item)
ordered = sorted(pairs, reverse=True)
return a.strip() + "|" + b.strip() + "|" + seen.get("a") + "|" + seen.get("missing", "no") + "|" + label + "|" + str(count) + "|" + "|".join(ordered) + "|" + str(sum([1, 2])) + "|" + min(["b", "a"]) + "|" + ("yes" if count > 1 else "no")
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert!(
            !text.contains("[revision "),
            "read results inside run_code stay free of the revision tag:\n{text}"
        );
        assert_eq!(text, "alpha|beta|alpha|no|SSSS|2|1y|0z|3|a|yes");
        let printed = run(
            &root,
            catalog(vec![]),
            "print(\"before\")\nraise Exception(\"boom\")\n",
        )
        .await;
        let printed_text = text_of(&printed);
        assert!(printed.is_error, "{printed_text}");
        assert!(
            printed_text.starts_with("before\nError:"),
            "prints stay in front of the error:\n{printed_text}"
        );
        assert!(printed_text.contains("boom"), "{printed_text}");
        let imported = run(&root, catalog(vec![]), "import os\n").await;
        let imported_text = text_of(&imported);
        assert!(imported.is_error, "{imported_text}");
        assert!(
            imported_text.contains("import is not available"),
            "{imported_text}"
        );
        assert!(
            !imported_text.contains("is not supported. Embedded"),
            "import errors stay one sentence the model can act on:\n{imported_text}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn len_range_and_str_follow_python() {
        let root = temp_dir("len");
        let result = run(
            &root,
            catalog(vec![]),
            r#"parts = "a\nb".split("\n")
total = 0
for i in range(len(parts)):
    total = total + len(parts[i])
return str(total)
"#,
        )
        .await;
        let text = text_of(&result);
        assert!(!result.is_error, "{text}");
        assert_eq!(text, "2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_hot_loop_stops_on_the_step_budget() {
        let root = temp_dir("loop");
        let result = run(&root, catalog(vec![]), "while True:\n    pass\n").await;
        let text = text_of(&result);
        assert!(result.is_error, "{text}");
        assert!(text.contains("budget"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn grouped_reads_return_fewer_bytes_than_four_full_files() {
        let root = temp_dir("save");
        let mut baseline = 0usize;
        for index in 0..4 {
            let mut body = String::new();
            for line in 0..80 {
                body.push_str(&format!("fn item_{index}_{line}() {{ return {line}; }}\n"));
            }
            body.push_str(&format!("fn marker_{index}() {{ return 1; }}\n"));
            let path = root.join(format!("file{index}.rs"));
            std::fs::write(&path, &body).unwrap();
            let tools = catalog(vec![Arc::new(ReadTool)]);
            let ctx = ToolCtx::new(&root).with_catalog(tools);
            let mut stream = ToolStream::closed();
            let read = crate::builtin::ReadTool
                .execute(
                    crate::builtin::read::ReadArgs {
                        path: format!("file{index}.rs"),
                        offset: None,
                        limit: None,
                    },
                    &ctx,
                    &mut stream,
                )
                .await
                .expect("read");
            baseline += text_of(&read).len();
        }
        let tools = catalog(vec![Arc::new(GrepTool)]);
        let grouped = run(
            &root,
            tools,
            "\
hits = await tools.grep(pattern=\"fn marker_\", include=\"*.rs\")\n\
return \"\\n\".join([line for line in hits.split(\"\\n\") if \"marker_\" in line])\n",
        )
        .await;
        let grouped_text = text_of(&grouped);
        assert!(!grouped.is_error, "{grouped_text}");
        let grouped_bytes = grouped_text.len();
        let saved = baseline.saturating_sub(grouped_bytes);
        let tokens_before = baseline / 4;
        let tokens_after = grouped_bytes / 4;
        eprintln!(
            "savings multi-file map: baseline {baseline} bytes (~{tokens_before} tokens), grouped {grouped_bytes} bytes (~{tokens_after} tokens), saved {saved} bytes"
        );
        assert!(
            grouped_bytes * 5 < baseline,
            "expected at least 80% savings, baseline {baseline}, grouped {grouped_bytes}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn description_names_both_required_arguments() {
        let tool = RunCodeTool;
        let spec = crate::tool::ToolDyn::spec(&tool);
        assert!(spec.description.contains("two required"));
        assert!(spec.description.contains("`description`"));
        assert!(spec.description.contains("`code`"));
        assert!(
            spec.description
                .contains("Python does not need to be installed")
        );
        assert!(spec.description.contains("Node.js is not used"));
        assert!(spec.description.contains("8000 bytes"));
        assert!(
            !spec.description.contains("TypeScript"),
            "the runtime is embedded Python, not TypeScript:\n{}",
            spec.description
        );
        assert!(
            !spec.description.to_lowercase().contains("image"),
            "run_code does not attach images"
        );
        let schema = spec.params_schema.to_string();
        assert!(schema.contains("5-10 words"), "{schema}");
        let snippet = tool.prompt_snippet().expect("snippet");
        assert!(snippet.contains("print"), "{snippet}");
        assert!(
            !snippet.contains("console.log"),
            "the tool list must not teach JavaScript:\n{snippet}"
        );
        assert!(schema.contains("async Python"), "{schema}");
        assert!(schema.contains("does not need to be installed"), "{schema}");
        let description_key = schema.find("\"description\"").expect("description");
        let code_key = schema.find("\"code\"").expect("code");
        assert!(
            description_key < code_key,
            "description is the first parameter so a model does not emit code alone"
        );
    }
}
