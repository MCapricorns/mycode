//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

/// Tool-list header. Deliberately not an identity sentence: every caller
/// (session, subagent, compaction) opens its prompt with its own identity,
/// so a second one here would read as a duplication.
const HEADER: &str = "\
Complete the task with the tools listed below. Do not invent tools. \
Support claims with inspected sources or tool results. Distinguish source inspection, \
checks actually run, and unresolved gaps; do not claim a command ran without its result.";

/// Names the one shell tool in `tools`, if the process registered one.
pub fn shell_tool_name(tools: &ToolRegistry) -> Option<String> {
    tools
        .names()
        .into_iter()
        .find(|name| mycode_tools::ShellKind::is_tool_name(name))
}

/// Calling rules follow the exposed registry. Child roles may expose tools
/// directly, or expose only code mode without a shell.
fn tool_calling(tools: &ToolRegistry, code_mode: bool) -> String {
    let mut rules = String::from("<tool_calling>\n");
    if code_mode {
        rules.push_str(
            "- Only names under Available tools are directly callable. Tools under \
Tools inside run_code are called from JavaScript as `call(\"name\", { argument: value })`; \
use their argument schemas below. `script` accepts JavaScript only. It runs in embedded \
QuickJS, with no Node.js, require, fs, fetch, filesystem or network globals. No imports, \
async/await or Promise job execution are supported.\n\
- Batch related operations in one script; calls execute synchronously in order. \
A `call` returns `{ content, is_error }`; text blocks use `result.content[0].Text`. Dispatch refusals also include `__error__`. \
Inspect errors before continuing dependent work; \
use `complete(value)` to return the result and `log(value)` for output and progress.\n\
- Use listed native names and schemas directly. When remote tools are available, \
retrieve their schemas through the registered discovery tools before calling them.\n\
- Scripts can reach only registered tools; they cannot add a shell or permissions.\n\
- A `run_code` script cannot call `run_code` or `wait_code` itself.\n",
        );
    }
    if ["find", "grep", "read"]
        .iter()
        .all(|name| tools.get(name).is_some())
    {
        rules.push_str(
            "- Use the dedicated tools first: `find` lists and locates files and directories \
(pattern `*` with `max_depth` 1 lists one directory), `grep` searches contents, `read` opens files.\n",
        );
    }
    if tools.get("write").is_some() && tools.get("edit").is_some() {
        if code_mode {
            rules.push_str(
                "- Prefer in-process JavaScript for routine file/data transformations: read with \
`call(\"read\", ...)`, transform objects/arrays/strings, then use `call(\"write\", ...)` or \
`call(\"edit\", ...)`.\n",
            );
        }
        let names = if code_mode {
            "`call(\"write\", …)` and `call(\"edit\", …)`"
        } else {
            "the registered `write` and `edit` tools"
        };
        let _ = writeln!(
            rules,
            "- Read existing content before changing it. Change files by {names}."
        );
    }
    if let Some(shell_tool) = shell_tool_name(tools) {
        let _ = writeln!(
            rules,
            "- Use `{shell_tool}` only for what the dedicated tools cannot do: builds, tests, \
git, package managers, and running programs (including the user's Python projects). \
Never use it to list directories, read files, or search text.\n\
- A `{shell_tool}` call waits for its command to exit. Do not start one that waits for \
input or never exits (a dev server, a watcher, an interactive prompt): it holds the turn \
until the timeout. Pass non-interactive flags instead.\n\
- On Windows, keep console work in the background with captured output. Do not open \
terminal windows or detach ordinary commands with Start-Process. The registered shell \
manages process lifetime and cancellation.\n\
- `{shell_tool}` `mode` `script` runs a command in the active shell. `mode` `program` \
runs one executable with an explicit `args` vector and no shell parsing. Prefer it when a command string would need fragile quoting.\n\
- Never use `{shell_tool}` to talk to the user."
        );
    }
    rules.push_str(
        "- Editing or recalling a chat message, or switching conversation branches, does not restore or delete workspace files. \
Inspect relevant files before resuming from earlier context.\n</tool_calling>",
    );
    rules
}

/// Native and bridge schemas hidden from Request.tools still reach the
/// model. Remote catalogs have discovery tools and stay on demand.
fn push_code_tools(prompt: &mut String, tools: &ToolRegistry) {
    let hidden: Vec<_> = tools
        .names()
        .into_iter()
        .filter(|name| !tools.is_exposed(name))
        .filter(|name| !matches!(name.as_str(), "run_code" | "wait_code"))
        .collect();
    if hidden.is_empty() {
        return;
    }
    prompt.push_str("\n\nTools inside run_code (use call(name, args)):");
    for name in hidden {
        let Some(tool) = tools.get(&name) else {
            continue;
        };
        if tool.schema_on_demand() {
            continue;
        }
        let spec = tool.spec();
        let _ = write!(
            prompt,
            "\n- {name}: {description}\n  Arguments JSON Schema: {schema}",
            description = spec.description,
            schema = spec.params_schema,
        );
    }
}

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(HEADER);
    prompt.push_str("\n\nAvailable tools:");

    let entries = tools.prompt_entries();
    if entries.is_empty() {
        prompt.push_str("\n(none)");
    } else {
        for (name, snippet) in entries {
            write!(prompt, "\n- {name}").expect("writing to a String cannot fail");
            let snippet = snippet
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty());
            if let Some(detail) = snippet {
                let detail = detail
                    .strip_prefix(&name)
                    .and_then(|text| text.strip_prefix(':'))
                    .map(str::trim)
                    .unwrap_or(detail);
                write!(prompt, ": {detail}").expect("writing to a String cannot fail");
            }
        }
    }

    let code_mode = tools.get("run_code").is_some() && tools.is_exposed("run_code");
    if !tools.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&tool_calling(tools, code_mode));
    }
    if code_mode {
        push_code_tools(&mut prompt, tools);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use mycode_tools::{
        DetectedShell, ShellKind, ShellTool, Tool, ToolRegistry, register_builtins,
    };

    use super::build_system_prompt;

    fn registry_with(shell: ShellKind, program: &str) -> Arc<ToolRegistry> {
        let registry = Arc::new(ToolRegistry::new());
        let builtins = ToolRegistry::new();
        register_builtins(&builtins);
        for name in builtins.names() {
            if !ShellKind::is_tool_name(&name) {
                registry.register(builtins.get(&name).unwrap());
            }
        }
        registry.register(Arc::new(ShellTool::forcing(DetectedShell {
            kind: shell,
            program: PathBuf::from(program),
        })));
        expose_code_mode(&registry, &[shell.tool_name(), "run_code", "wait_code"]);
        registry
    }

    fn expose_code_mode(registry: &Arc<ToolRegistry>, exposed: &[&str]) {
        let cells = Arc::new(mycode_tools::CodeCells::new());
        registry.register(Arc::new(mycode_tools::RunCodeTool::new(
            Arc::clone(registry),
            Arc::clone(&cells),
            "prompt-test".to_owned(),
        )));
        registry.register(Arc::new(mycode_tools::WaitCodeTool::new(
            cells,
            "prompt-test".to_owned(),
        )));
        registry.expose_only(exposed);
    }

    #[test]
    fn code_mode_carries_hidden_tool_schemas_without_exposing_them_to_the_provider() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let prompt = build_system_prompt(&registry);
        let (direct, hidden) = prompt
            .split_once("\n\nTools inside run_code")
            .expect("hidden catalog");
        assert!(direct.contains("\n- run_code:"));
        assert!(!direct.contains("\n- read:"));
        assert!(
            !direct.contains("ask_user"),
            "unregistered tools must not be advertised"
        );
        assert!(!registry.specs().iter().any(|spec| spec.name == "read"));
        for name in ["read", "write", "edit", "find", "grep"] {
            let spec = registry.get(name).unwrap().spec();
            assert!(hidden.contains(&format!("\n- {name}:")));
            assert!(hidden.contains(&format!("Arguments JSON Schema: {}", spec.params_schema)));
        }
        assert!(direct.contains("`script` accepts JavaScript only"));
        assert!(direct.contains("no Node.js"));
        assert!(
            direct.contains("Prefer in-process JavaScript for routine file/data transformations")
        );
        assert!(!direct.contains("Rhai"));
        assert!(!direct.contains("pipe a literal here-string to `python`"));
        assert!(direct.contains("Inspect errors before continuing dependent work"));
    }

    struct DiscoveredTool(String);

    impl mycode_tools::ToolDyn for DiscoveredTool {
        fn spec(&self) -> mycode_tools::ToolSpec {
            mycode_tools::ToolSpec {
                name: self.0.clone(),
                description: "discovered remote capability".into(),
                params_schema: serde_json::json!({
                    "type": "object", "description": "remote_schema".repeat(300)
                }),
            }
        }

        fn schema_on_demand(&self) -> bool {
            true
        }

        fn execute_dyn<'a>(
            &'a self,
            _: serde_json::Value,
            _: &'a mycode_tools::ToolCtx,
            _: &'a mut mycode_tools::ToolStream,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<mycode_tools::ToolResult, mycode_tools::ToolError>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { unreachable!("prompt construction never executes a tool") })
        }
    }

    #[test]
    fn ready_remote_catalogs_do_not_expand_the_fixed_prompt() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let before = build_system_prompt(&registry);
        for index in 0..100 {
            registry.register(Arc::new(DiscoveredTool(format!("remote_{index}"))));
        }
        assert_eq!(build_system_prompt(&registry), before);
        assert!(
            registry.get("remote_0").is_some(),
            "discovery keeps dispatch available"
        );
    }

    #[test]
    fn shellless_code_mode_still_explains_call_and_the_read_schema() {
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(mycode_tools::builtin::ReadTool));
        expose_code_mode(&registry, &["run_code", "wait_code"]);
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("call(\"name\", { argument: value })"));
        assert!(prompt.contains("\n- read:"));
        assert!(prompt.contains("Arguments JSON Schema:"));
        assert!(!prompt.contains("`powershell`"));
        assert!(!prompt.contains("`bash`"));
        assert!(!prompt.contains("`agent`"));
        for unavailable in [
            "call(\"write\"",
            "call(\"edit\"",
            "Change files by",
            "routine file/data transformations",
        ] {
            assert!(
                !prompt.contains(unavailable),
                "unavailable capability {unavailable}:\n{prompt}"
            );
        }
        assert!(prompt.contains("calls execute synchronously in order"));
        assert!(prompt.contains("No imports, async/await or Promise job execution are supported"));
        assert!(prompt.contains("Scripts can reach only registered tools"));
        assert!(prompt.contains("Use listed native names and schemas directly"));
        assert!(prompt.contains("retrieve their schemas through the registered discovery tools"));
        assert!(prompt.contains("switching conversation branches"));
        assert!(prompt.contains("does not restore or delete workspace files"));
    }

    #[test]
    fn directly_exposed_tools_do_not_teach_an_unregistered_code_tool() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("\n- read:"));
        assert!(!prompt.contains("run_code"));
        assert!(!prompt.contains("call(\"write\""));
    }

    #[test]
    fn prompt_names_the_active_shell_tool_and_not_exec_or_task() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        let shell = mycode_tools::resolved_shell().kind.tool_name();
        assert!(prompt.contains(&format!("`{shell}`")), "{prompt}");
        assert!(prompt.contains("`program`"));
        assert!(prompt.contains("`script`"));
        assert!(!prompt.contains("`exec`"));
        assert!(!prompt.contains("`task`"));
        assert!(!prompt.contains("translated"));
    }

    #[test]
    fn powershell_51_prompt_forbids_bash_chaining() {
        let registry = registry_with(ShellKind::WindowsPowerShell, "powershell.exe");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("`powershell`"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(prompt.contains("in-process JavaScript"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        assert!(
            !prompt.contains("`shell`"),
            "the model must not be told to call `shell`:\n{prompt}"
        );
    }

    #[test]
    fn bash_prompt_prefers_javascript_for_file_transforms() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("`bash`"), "{prompt}");
        assert!(prompt.contains("in-process JavaScript"), "{prompt}");
        assert!(
            prompt.contains("including the user's Python projects"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Change files by `call(\"write\""),
            "write and edit are the file-change tools reached through call, got:\n{prompt}"
        );
        assert!(
            prompt.contains("Use `bash` only for what the dedicated tools cannot do"),
            "the shell is the fallback, got:\n{prompt}"
        );
        assert!(
            prompt.contains("Never use it to list directories, read files, or search text"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`max_depth` 1"),
            "directory listing belongs to find, got:\n{prompt}"
        );
        assert!(
            prompt.contains("`mode` `program`"),
            "external programs keep a direct invocation path, got:\n{prompt}"
        );
        assert!(
            !prompt.contains("only for pipelines"),
            "shell must not be limited to pipelines:\n{prompt}"
        );
        assert!(
            prompt.contains("does not restore or delete workspace files"),
            "chat rewind must be documented as chat-only:\n{prompt}"
        );
        assert!(!prompt.contains("here-string"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        let shell = ShellTool::forcing(DetectedShell {
            kind: ShellKind::Bash,
            program: PathBuf::from("/bin/bash"),
        });
        let description = shell.description();
        assert!(
            !description.contains("run Python"),
            "shell descriptions must not direct routine rewrites to Python, got:\n{description}"
        );
        assert!(
            description.contains("`mode` `program`"),
            "program mode stays on the shell tool, got:\n{description}"
        );
        assert!(
            !description.contains("`exec`"),
            "shell description must not name a separate exec tool, got:\n{description}"
        );
    }

    #[test]
    fn powershell_7_prompt_allows_native_chaining() {
        let registry = registry_with(ShellKind::Pwsh, "pwsh.exe");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("powershell:"), "{prompt}");
        assert!(prompt.contains("in-process JavaScript"), "{prompt}");
        let shell = ShellTool::forcing(DetectedShell {
            kind: ShellKind::Pwsh,
            program: PathBuf::from("pwsh.exe"),
        });
        assert!(shell.description().contains("PowerShell 7"));
        assert!(!shell.description().contains("syntax errors"));
    }

    #[test]
    fn agent_snippet_defers_role_names_to_the_dispatch_section() {
        struct Host;

        impl mycode_tools::builtin::AgentHost for Host {
            fn run_subagent<'a>(
                &'a self,
                _request: mycode_tools::builtin::SubagentRequest,
                _progress: &'a mycode_tools::ToolStream,
                _cancel: &'a tokio_util::sync::CancellationToken,
                _call_id: &'a str,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<String, mycode_tools::ToolError>>
                        + Send
                        + 'a,
                >,
            > {
                Box::pin(async { Err(mycode_tools::ToolError::Execution("unused".to_owned())) })
            }
        }

        let registry = ToolRegistry::new();
        register_builtins(&registry);
        registry.register(Arc::new(mycode_tools::builtin::AgentTool::new(Arc::new(
            Host,
        ))));
        let prompt = build_system_prompt(&registry);
        assert!(
            prompt.contains("agent: hand one self-contained brief to a role from <dispatch>"),
            "the agent snippet must point at the dispatch section:\n{prompt}"
        );
        for role in ["scout", "artisan", "sentinel", "skeptic"] {
            assert!(
                !prompt.contains(role),
                "role {role} must come from the dynamic dispatch section only:\n{prompt}"
            );
        }
    }
}
