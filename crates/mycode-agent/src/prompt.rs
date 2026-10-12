//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

/// Tool-list header. Deliberately not an identity sentence: every caller
/// (session, subagent, compaction) opens its prompt with its own identity,
/// so a second one here would read as a duplication.
const IDENTITY: &str = "\
Complete the task with the tools listed below. Do not invent tools. \
Tool results are the source of truth; do not claim a command ran unless its result is in the conversation.";

/// Names the one shell tool in `tools`, if the process registered one.
fn shell_tool_name(tools: &ToolRegistry) -> Option<String> {
    let names = tools.names();
    ["powershell", "bash", "zsh", "sh", "cmd"]
        .into_iter()
        .find(|name| names.iter().any(|registered| registered == name))
        .map(str::to_owned)
}

fn edit_hint(shell_tool: &str) -> &'static str {
    match shell_tool {
        "powershell" => {
            "For a shell edit, use `mode` `script` and pipe a literal here-string to `python` or `python3` (`@'...'@ | python -`)."
        }
        "bash" | "zsh" | "sh" => {
            "For a shell edit, use `mode` `script` and run Python (`python3` or `python`) with a quoted heredoc or a short script."
        }
        "cmd" => {
            "Prefer `write` and `edit` for file changes. `mode` `script` is cmd.exe, not bash or PowerShell."
        }
        _ => "For a shell edit, use `mode` `script`.",
    }
}

fn tool_calling(shell_tool: &str) -> String {
    let hint = edit_hint(shell_tool);
    format!(
        "<tool_calling>\n\
- Independent calls in one response run together. Do not wait between them.\n\
- Read existing content before changing it. Prefer `read`, `grep`, and `find` to inspect files.\n\
- `write`, `edit`, and `{shell_tool}` can all change files. {hint}\n\
- Use `{shell_tool}` with `mode` `program` for one kernel image, or a direct shebang wrapper, and an explicit argument vector. That path does not parse a shell command string.\n\
- Never use `{shell_tool}` to talk to the user.\n\
- Editing or recalling a chat message does not restore or delete workspace files.\n\
</tool_calling>"
    )
}

fn is_shell_name(name: &str) -> bool {
    matches!(name, "powershell" | "bash" | "zsh" | "sh" | "cmd")
}

/// PTC contract plus the generated `tools.*` SDK. Stable for a registry:
/// tool order is sorted and the Python probe is cached for the process.
fn tools_section(tools: &ToolRegistry) -> Option<String> {
    tools.get("run_code")?;
    let mut section = String::from("<tools>\n");
    if let Some(message) = mycode_tools::python_unavailable_message() {
        section
            .push_str("Python is unavailable. `run_code` cannot execute until it is installed.\n");
        section.push_str(&message);
        section.push_str("\n\n");
    }
    section.push_str(
        "`run_code` is the only tool you can call directly — a tool call naming any other tool fails. Reach every tool the SDK declares below from inside the program.\n\n",
    );
    section.push_str("```python\n");
    section.push_str(
        "# run_code(description, code): description is a short summary shown in the UI; code is the body of an async function. Top-level await and return work.\n",
    );
    section.push_str(
        "# Call tools as await tools.name(arg=value) or await tools.name({...}). A failed call raises ToolCallError (e.toolName, e.message); try/except it to continue.\n",
    );
    section.push_str(
        "# Independent calls MAY overlap under asyncio.gather. read, grep, find, web_search, fetch_content, and agent run concurrently. write, edit, and shell run alone, in submission order.\n",
    );
    section.push_str(
        "# A name that is not a Python identifier is called as await tools[\"name\"](...). MCP tools are named server__tool, for example await tools.docs__lookup(...).\n",
    );
    section.push_str(
        "# Only print(...) and return reach the conversation. Other results stay out, so extract just what you need.\n",
    );
    section.push_str(
        "# Each call starts from fresh globals. Names, imports, and environment edits from an earlier call are not there. The interpreter may already be warm.\n",
    );
    for line in sdk_lines(tools) {
        section.push_str(&line);
        section.push('\n');
    }
    section.push_str("```\n\n");
    section.push_str(
        "Use `tools.shell(...)` to run external programs (build, test, lint, git, package managers, servers, project scripts); `tools.read` / `tools.grep` / `tools.find` / `tools.edit` / `tools.write` for files; `tools.web_search` / `tools.fetch_content` for the web. Never call `subprocess` or `os.system` from Python directly.\n",
    );
    if tools.get("agent").is_some() {
        section.push_str(
            "Prefer `tools.agent` with agent \"scout\" for broad research, vendor docs, and a repository-wide map. Several scout calls may overlap inside `asyncio.gather`; each returns a short map. Do not spend this chat on a long chain of `web_search` and `fetch_content` for that kind of task. A narrow lookup stays in this program. Delegating broad research to scout is your judgment.\n",
        );
    }
    if tools.get("fetch_content").is_some() {
        section.push_str(
            "A `fetch_content` result that reports HTTP 4xx, HTTP 422, or extract_failed is final for that URL. Do not call `fetch_content` again for it; try another URL or continue without that page.\n",
        );
    }
    section.push_str("</tools>");
    Some(section)
}

fn sdk_lines(tools: &ToolRegistry) -> Vec<String> {
    let mut lines = Vec::new();
    let mut shell_spec = None;
    let mut shell_name = None;
    for name in tools.names() {
        if name == "run_code" {
            continue;
        }
        let Some(tool) = tools.get(&name) else {
            continue;
        };
        let spec = tool.spec();
        if is_shell_name(&name) {
            if shell_spec.is_none() {
                shell_name = Some(name);
                shell_spec = Some(spec);
            }
            continue;
        }
        lines.push(sdk_entry(&name, &spec.description, &spec.params_schema));
    }
    if let Some(spec) = shell_spec {
        let name = shell_name.as_deref().unwrap_or("shell");
        let hint = match name {
            "powershell" => "here-string edits go to python",
            "bash" | "zsh" | "sh" => "quoted heredoc edits go to Python",
            "cmd" => "prefer tools.write and tools.edit",
            _ => "script or program mode",
        };
        let blurb =
            format!("mode `script` runs `{name}` ({hint}); mode `program` launches one executable");
        lines.push(sdk_entry("shell", &blurb, &spec.params_schema));
    }
    lines.sort();
    lines
}

fn sdk_entry(name: &str, description: &str, schema: &serde_json::Value) -> String {
    let comment = first_sentence(description).replace('\n', " ");
    format!("# {name}: {comment}\n{}", python_signature(name, schema))
}

fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = flat.find(". ").map(|index| index + 1).unwrap_or(flat.len());
    flat.chars().take(cut.min(180)).collect()
}

fn is_python_attribute(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return false;
    }
    !is_python_keyword(name)
}

fn is_python_keyword(name: &str) -> bool {
    matches!(
        name,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    )
}

fn python_signature(name: &str, schema: &serde_json::Value) -> String {
    let required = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut params = Vec::new();
    if let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) {
        for (field, field_schema) in properties {
            let (ty, nullable) = python_type(field_schema);
            let optional = nullable || !required.iter().any(|name| name == field);
            if optional {
                params.push(format!("{field}: {ty} | None = None"));
            } else {
                params.push(format!("{field}: {ty}"));
            }
        }
    }
    if !is_python_attribute(name) {
        let args = if params.is_empty() {
            String::new()
        } else {
            params
                .iter()
                .map(|param| param.split(':').next().unwrap_or(param).trim())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return format!("# await tools[{name:?}]({args})");
    }
    if params.is_empty() {
        format!("async def {name}() -> str: ...")
    } else {
        format!("async def {name}(*, {}) -> str: ...", params.join(", "))
    }
}

fn python_type(schema: &serde_json::Value) -> (String, bool) {
    if schema.get("enum").is_some() {
        return ("str".to_owned(), false);
    }
    if let Some(types) = schema.get("type").and_then(|value| value.as_array()) {
        let mut nullable = false;
        let mut ty = None;
        for item in types {
            if item.as_str() == Some("null") {
                nullable = true;
                continue;
            }
            if ty.is_none()
                && let Some(name) = item.as_str()
            {
                ty = Some(scalar_type(name));
            }
        }
        return (ty.unwrap_or_else(|| "object".to_owned()), nullable);
    }
    if let Some(options) = schema
        .get("anyOf")
        .and_then(|value| value.as_array())
        .filter(|items| !items.is_empty())
    {
        let mut nullable = false;
        let mut ty = None;
        for option in options {
            if option.get("type").and_then(|value| value.as_str()) == Some("null") {
                nullable = true;
                continue;
            }
            if ty.is_none() {
                ty = Some(python_type(option).0);
            }
        }
        return (ty.unwrap_or_else(|| "object".to_owned()), nullable);
    }
    let ty = scalar_type(
        schema
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("object"),
    );
    (ty, false)
}

fn scalar_type(name: &str) -> String {
    match name {
        "string" => "str",
        "integer" => "int",
        "number" => "float",
        "boolean" => "bool",
        "array" => "list",
        "object" => "dict",
        "null" => "None",
        _ => "object",
    }
    .to_owned()
}

fn web_section(tools: &ToolRegistry) -> Option<&'static str> {
    let search = tools.get("web_search").is_some();
    let fetch = tools.get("fetch_content").is_some();
    if !search && !fetch {
        return None;
    }
    Some(
        "<web>\n\
`web_search` returns compact hits: title, URL, and a snippet of at most 240 characters. Duplicate URLs are dropped. An identical query is cached for 10 minutes. Snippets are leads, not evidence.\n\
`fetch_content` reads pages. Pass `goal` (the fact you need) and only matching excerpts return, about 1200 characters per page, marked `(excerpts for goal)`. Without `goal`, each page is capped at 8000 characters and marked `(truncated)` when cut. HTTP 4xx, HTTP 422, extract_failed, or Unable to extract is permanent for that URL; do not call `fetch_content` again for it.\n\
</web>",
    )
}

/// How hard a task is, for the choice eval. The prompt guides this; it does
/// not require one answer when both are reasonable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskWeight {
    /// One edit or one new file. `tools.edit` or `tools.write`, not scout or shell.
    Direct,
    /// Several lookups or edits in one program. Not scout, and not a shell script.
    GroupInline,
    /// One search. `tools.grep`, `tools.find`, or `tools.read`. Not scout or shell.
    Lookup,
    /// Broad research. `scout` or a non-empty inline `run_code` are both fine.
    Broad,
    /// Load one skill by reading its `SKILL.md` path inside the program.
    Skill,
    /// One named SDK call (`server__tool`, or brackets when the name is not an identifier).
    SdkCall,
    /// A build, test, or git command via `tools.shell`, not `tools.bash`.
    Process,
    /// Two or more `tools.agent` calls with agent `"scout"`. Not artisan or web fetches.
    Scout,
}

/// One scripted task the choice eval scores.
#[derive(Clone, Copy, Debug)]
pub struct ChoiceScenario {
    /// Stable id for reports.
    pub id: &'static str,
    /// User task, shown to a model when a live eval runs.
    pub task: &'static str,
    /// What the prompt should make reasonable.
    pub weight: TaskWeight,
    /// Live model eval skips this unless `MYCODE_PTC_EVAL_SLOW=1`.
    pub slow: bool,
    /// `tools.*` names the program must call, besides the weight's own rule.
    pub require: &'static [&'static str],
    /// `tools.*` names the program must not call.
    pub forbid: &'static [&'static str],
    /// Substring the program must contain outside comments, or empty.
    pub needle: &'static str,
}

/// Scripted tasks. Simple work stays inline; broad research may go to scout.
///
/// The first three are the default live set. The rest are marked [`ChoiceScenario::slow`]
/// so a configured model run stays small unless `MYCODE_PTC_EVAL_SLOW=1`.
#[must_use]
pub fn choice_scenarios() -> &'static [ChoiceScenario] {
    &[
        ChoiceScenario {
            id: "narrow-search",
            task: "In src/turn.rs only, which lines contain the exact identifier dispatch_tool? Return path:line. This is one file; do the lookup in the current program.",
            weight: TaskWeight::Lookup,
            slow: false,
            require: &[],
            forbid: &[],
            needle: "",
        },
        ChoiceScenario {
            id: "one-edit",
            task: "In src/a.rs, replace the single typo recieve with receive. That is the entire change. Do it with the edit tool inside one program.",
            weight: TaskWeight::Direct,
            slow: false,
            require: &[],
            forbid: &[],
            needle: "",
        },
        ChoiceScenario {
            id: "broad-research",
            task: "Before any edit, map how authentication works across every crate in this repository and check the vendor's current documentation for token refresh. Return a short map.",
            weight: TaskWeight::Broad,
            slow: false,
            require: &[],
            forbid: &[],
            needle: "",
        },
        ChoiceScenario {
            id: "grouped-reads",
            task: "Read src/a.rs and src/b.rs in this program. Call tools.read once per file (asyncio.gather is fine). Do not call tools.agent or tools.shell.",
            weight: TaskWeight::GroupInline,
            slow: true,
            require: &["read"],
            forbid: &["agent", "shell"],
            needle: "",
        },
        ChoiceScenario {
            id: "grouped-edits",
            task: "In src/a.rs and src/b.rs, replace the typo recieve with receive. Do both edits in this program with tools.edit. Do not use tools.shell or tools.agent.",
            weight: TaskWeight::GroupInline,
            slow: true,
            require: &["edit"],
            forbid: &["shell", "agent"],
            needle: "recieve",
        },
        ChoiceScenario {
            id: "one-write",
            task: "Create notes/flag.txt containing exactly ok. That is the entire change. Use tools.write inside this program. Do not use tools.shell, tools.bash, or tools.agent.",
            weight: TaskWeight::Direct,
            slow: true,
            require: &["write"],
            forbid: &["shell", "edit"],
            needle: "notes/flag.txt",
        },
        ChoiceScenario {
            id: "skill-review",
            task: "The review skill matches this task. Its file is .agents/skills/review/SKILL.md. Load that SKILL.md with tools.read inside this program and return the body. Do not call read directly, do not use tools.shell, and do not edit the skill.",
            weight: TaskWeight::Skill,
            slow: true,
            require: &["read"],
            forbid: &["shell", "edit", "write", "skill"],
            needle: ".agents/skills/review/SKILL.md",
        },
        ChoiceScenario {
            id: "mcp-docs-lookup",
            task: "The docs MCP server is already in the SDK as docs__lookup. Inside this program call docs__lookup with query \"token refresh\" and return the text. Do not call search_tool, use_tool, shell, or the bare name lookup.",
            weight: TaskWeight::SdkCall,
            slow: true,
            require: &["docs__lookup"],
            forbid: &["search_tool", "use_tool", "shell", "lookup"],
            needle: "token refresh",
        },
        ChoiceScenario {
            id: "mcp-get-page",
            task: "Call the MCP tool whose Python name is docs__get_page (the server tool get-page, sanitized). Inside this program use await tools.docs__get_page(query=\"install\") or tools[\"docs__get_page\"]. Do not call get-page, docs.get-page, search_tool, or shell.",
            weight: TaskWeight::SdkCall,
            slow: true,
            require: &["docs__get_page"],
            forbid: &["search_tool", "use_tool", "shell"],
            needle: "install",
        },
        ChoiceScenario {
            id: "keyword-class",
            task: "The tool name class is a Python keyword. Inside this program call await tools[\"class\"](query=\"ptc\") or await tools['class'](query=\"ptc\"). Do not write tools.class(...), and do not use shell, agent, search_tool, or use_tool.",
            weight: TaskWeight::SdkCall,
            slow: true,
            require: &["class"],
            forbid: &["shell", "agent", "search_tool", "use_tool"],
            needle: "ptc",
        },
        ChoiceScenario {
            id: "scout-gather",
            task: "Map shutdown for Tokio, async-std, and smol. These are three independent briefs. Inside this program, call tools.agent with agent \"scout\" for each, using asyncio.gather so they overlap. Do not call artisan, web_search, or fetch_content.",
            weight: TaskWeight::Scout,
            slow: true,
            require: &["agent"],
            forbid: &["web_search", "fetch_content", "shell"],
            needle: "asyncio.gather",
        },
        ChoiceScenario {
            id: "cargo-test",
            task: "Run cargo test -p mycode-agent and return whether it passed. Use tools.shell inside this program. Do not call bash, powershell, edit, or write.",
            weight: TaskWeight::Process,
            slow: true,
            require: &["shell"],
            forbid: &["bash", "powershell", "edit", "write", "agent"],
            needle: "cargo test -p mycode-agent",
        },
        ChoiceScenario {
            id: "lookup-no-refetch",
            task: "fetch_content already reported HTTP 404 for https://example.invalid/auth. Do not call fetch_content or web_search again. In src/auth.rs only, find the identifier refresh_token with tools.grep or tools.read inside this program.",
            weight: TaskWeight::Lookup,
            slow: true,
            require: &[],
            forbid: &["fetch_content", "web_search", "agent", "shell"],
            needle: "refresh_token",
        },
    ]
}

fn program_code(arguments: &serde_json::Value) -> &str {
    arguments
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

const SHELL_ALIASES: &[&str] = &["bash", "powershell", "zsh", "sh", "cmd"];

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Bytes that are real code. Comments are always masked. String literals are
/// masked only when `blank_strings` is set, so a path inside quotes still
/// counts while `tools.grep` inside a comment or a string does not.
fn region_mask(code: &str, blank_strings: bool) -> Vec<bool> {
    let chars: Vec<(usize, char)> = code.char_indices().collect();
    let mut mask = vec![true; code.len()];
    let mut index = 0;
    while index < chars.len() {
        let (start, ch) = chars[index];
        if ch == '#' {
            index = blank_comment(&mut mask, &chars, index, start);
            continue;
        }
        if ch == '"' || ch == '\'' {
            index = skip_string(&mut mask, &chars, index, blank_strings);
            continue;
        }
        index += 1;
    }
    mask
}

fn blank(mask: &mut [bool], start: usize, end: usize) {
    for slot in &mut mask[start..end] {
        *slot = false;
    }
}

fn blank_comment(mask: &mut [bool], chars: &[(usize, char)], index: usize, start: usize) -> usize {
    let mut next = index + 1;
    while next < chars.len() && chars[next].1 != '\n' {
        next += 1;
    }
    let end = chars.get(next).map(|(byte, _)| *byte).unwrap_or(mask.len());
    blank(mask, start, end);
    next
}

fn skip_string(
    mask: &mut [bool],
    chars: &[(usize, char)],
    index: usize,
    blank_strings: bool,
) -> usize {
    let quote = chars[index].1;
    let start = chars[index].0;
    let triple =
        index + 2 < chars.len() && chars[index + 1].1 == quote && chars[index + 2].1 == quote;
    let mut next = index + if triple { 3 } else { 1 };
    while next < chars.len() {
        if !triple && chars[next].1 == '\\' {
            next += 1;
            if next < chars.len() {
                next += 1;
            }
            continue;
        }
        if triple
            && next + 2 < chars.len()
            && chars[next].1 == quote
            && chars[next + 1].1 == quote
            && chars[next + 2].1 == quote
        {
            next += 3;
            break;
        }
        if !triple && chars[next].1 == quote {
            next += 1;
            break;
        }
        next += 1;
    }
    if blank_strings {
        let end = chars.get(next).map(|(byte, _)| *byte).unwrap_or(mask.len());
        blank(mask, start, end);
    }
    next
}

fn in_code(mask: &[bool], start: usize, end: usize) -> bool {
    mask.get(start..end)
        .is_some_and(|slots| slots.iter().all(|slot| *slot))
}

fn each_tool_call<'a>(code: &'a str, mut visit: impl FnMut(&'a str)) {
    let mask = region_mask(code, true);
    let marker = "tools";
    let mut offset = 0;
    while let Some(rel) = code[offset..].find(marker) {
        let index = offset + rel;
        let after = index + marker.len();
        let before_ok = index == 0
            || code[..index]
                .chars()
                .next_back()
                .is_some_and(|ch| !is_ident_char(ch));
        if before_ok
            && in_code(&mask, index, after)
            && let Some(name) = tool_name_at(&code[after..])
        {
            visit(name);
        }
        offset = after;
    }
}

fn tool_name_at(rest: &str) -> Option<&str> {
    let trimmed = rest.trim_start();
    if let Some(rest) = trimmed.strip_prefix('.') {
        let name = take_ident(rest)?;
        return Some(name);
    }
    let rest = trimmed.strip_prefix('[')?.trim_start();
    let name = quoted_name(rest)?;
    if name.is_empty() { None } else { Some(name) }
}

fn take_ident(text: &str) -> Option<&str> {
    let end = text
        .char_indices()
        .find(|(_, ch)| !is_ident_char(*ch))
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    if end == 0 { None } else { Some(&text[..end]) }
}

fn quoted_name(text: &str) -> Option<&str> {
    let quote = text.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = &text[quote.len_utf8()..];
    let end = body.find(quote)?;
    Some(&body[..end])
}

/// True when `code` calls `name` as `tools.name` or `tools["name"]`.
///
/// The character before `tools` must not be an identifier character, so
/// `mytools.grep` and `tools.grep_extra` do not count. Comments and string
/// literals do not count. Bracket calls may include whitespace:
/// `tools[ "name" ]`.
fn calls_tool(code: &str, name: &str) -> bool {
    let mut found = false;
    each_tool_call(code, |called| {
        if called == name {
            found = true;
        }
    });
    found
}

fn tool_call_count(code: &str, name: &str) -> usize {
    let mut count = 0;
    each_tool_call(code, |called| {
        if called == name {
            count += 1;
        }
    });
    count
}

fn any_tool_call(code: &str) -> bool {
    let mut found = false;
    each_tool_call(code, |_| found = true);
    found
}

fn calls_shell_alias(code: &str) -> bool {
    SHELL_ALIASES.iter().any(|name| calls_tool(code, name))
}

fn calls_shell_family(code: &str) -> bool {
    calls_tool(code, "shell") || calls_shell_alias(code)
}

fn file_calls(code: &str) -> usize {
    ["grep", "find", "read", "edit", "write"]
        .into_iter()
        .map(|name| tool_call_count(code, name))
        .sum()
}

/// `agent="scout"`, `agent = 'scout'`, or `"agent": "scout"` in real code.
fn assigns_string(code: &str, field: &str, value: &str) -> bool {
    let mask = region_mask(code, true);
    let mut offset = 0;
    while let Some(rel) = code[offset..].find(field) {
        let index = offset + rel;
        let after = index + field.len();
        let before_ok = index == 0
            || code[..index]
                .chars()
                .next_back()
                .is_some_and(|ch| !is_ident_char(ch));
        let after_ok = code[after..]
            .chars()
            .next()
            .is_none_or(|ch| !is_ident_char(ch));
        if before_ok && after_ok && in_code(&mask, index, after) {
            let rest = code[after..].trim_start();
            if let Some(rest) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) {
                let rest = rest.trim_start();
                if rest.starts_with(&format!("\"{value}\""))
                    || rest.starts_with(&format!("'{value}'"))
                {
                    return true;
                }
            }
        }
        offset = after;
    }
    false
}

fn outside_comments(code: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mask = region_mask(code, false);
    let mut offset = 0;
    while let Some(rel) = code[offset..].find(needle) {
        let index = offset + rel;
        let end = index + needle.len();
        if in_code(&mask, index, end) {
            return true;
        }
        offset = index + needle.len().max(1);
    }
    false
}

fn calls_required(code: &str, name: &str) -> bool {
    if is_python_attribute(name) {
        return calls_tool(code, name);
    }
    // Keywords and other non-identifiers are only valid as tools["name"].
    let mut found = false;
    let mask = region_mask(code, true);
    let marker = "tools";
    let mut offset = 0;
    while let Some(rel) = code[offset..].find(marker) {
        let index = offset + rel;
        let after = index + marker.len();
        let before_ok = index == 0
            || code[..index]
                .chars()
                .next_back()
                .is_some_and(|ch| !is_ident_char(ch));
        if before_ok && in_code(&mask, index, after) {
            let trimmed = code[after..].trim_start();
            if let Some(rest) = trimmed.strip_prefix('[')
                && quoted_name(rest.trim_start()) == Some(name)
            {
                found = true;
                break;
            }
        }
        offset = after;
    }
    found
}

/// Whether a model's first tool call matches PTC-only judgment for `weight`.
///
/// Every task starts with `run_code` and a non-empty program. A lookup calls
/// `tools.grep`, `tools.find`, or `tools.read`. One edit calls `tools.edit`
/// or `tools.write`. A group calls at least two file tools. Broad research
/// may call `tools.agent` for scout or stay inline. Shell aliases
/// (`tools.bash` and the other interpreter names) count as shell.
#[must_use]
pub fn accept_choice(weight: TaskWeight, tool: &str, arguments: &serde_json::Value) -> bool {
    if tool != "run_code" {
        return false;
    }
    let code = program_code(arguments);
    if code.trim().is_empty() {
        return false;
    }
    match weight {
        TaskWeight::Direct => {
            (calls_tool(code, "edit") || calls_tool(code, "write"))
                && !calls_tool(code, "agent")
                && !calls_shell_family(code)
        }
        TaskWeight::Lookup => {
            (calls_tool(code, "grep") || calls_tool(code, "find") || calls_tool(code, "read"))
                && !calls_tool(code, "agent")
                && !calls_shell_family(code)
        }
        TaskWeight::GroupInline => {
            file_calls(code) >= 2 && !calls_tool(code, "agent") && !calls_shell_family(code)
        }
        TaskWeight::Broad => true,
        TaskWeight::Skill => {
            calls_tool(code, "read")
                && outside_comments(code, "SKILL.md")
                && !calls_shell_family(code)
                && !calls_tool(code, "edit")
                && !calls_tool(code, "write")
                && !calls_tool(code, "agent")
                && !calls_tool(code, "skill")
                && !calls_tool(code, "skills")
        }
        TaskWeight::SdkCall => {
            any_tool_call(code)
                && !calls_shell_family(code)
                && !calls_tool(code, "agent")
                && !calls_tool(code, "search_tool")
                && !calls_tool(code, "use_tool")
        }
        TaskWeight::Process => {
            calls_tool(code, "shell")
                && !calls_shell_alias(code)
                && !calls_tool(code, "agent")
                && !calls_tool(code, "edit")
                && !calls_tool(code, "write")
        }
        TaskWeight::Scout => {
            tool_call_count(code, "agent") >= 2
                && assigns_string(code, "agent", "scout")
                && !assigns_string(code, "agent", "artisan")
                && !calls_tool(code, "web_search")
                && !calls_tool(code, "fetch_content")
                && !calls_shell_family(code)
        }
    }
}

/// Weight rule plus the scenario's required names, forbidden names, and needle.
#[must_use]
pub fn accept_scenario(
    scenario: &ChoiceScenario,
    tool: &str,
    arguments: &serde_json::Value,
) -> bool {
    if !accept_choice(scenario.weight, tool, arguments) {
        return false;
    }
    let code = program_code(arguments);
    if !outside_comments(code, scenario.needle) {
        return false;
    }
    scenario
        .require
        .iter()
        .all(|name| calls_required(code, name))
        && scenario.forbid.iter().all(|name| !calls_tool(code, name))
}

/// Whether `prompt` states the judgment for `weight` without forbidding the
/// other reasonable choice.
#[must_use]
pub fn prompt_guides(prompt: &str, weight: TaskWeight) -> bool {
    let forces_scout = prompt.contains("Do not do that research inline")
        || prompt.contains("must dispatch")
        || prompt.contains("only `scout`");
    if forces_scout {
        return false;
    }
    match weight {
        TaskWeight::Direct => {
            prompt.contains("run_code")
                && (prompt.contains("tools.edit") || prompt.contains("tools.write"))
        }
        TaskWeight::Lookup => prompt.contains("tools.grep") || prompt.contains("tools.find"),
        TaskWeight::GroupInline => {
            prompt.contains("run_code")
                && (prompt.contains("asyncio.gather") || prompt.contains("tools.edit"))
        }
        TaskWeight::Broad => {
            prompt.contains("scout")
                && (prompt.contains("judgment")
                    || prompt.contains("your judgment")
                    || prompt.contains("You decide"))
                && prompt.contains("run_code")
        }
        TaskWeight::Skill => {
            (prompt.contains("tools.read") || prompt.contains("await tools.read"))
                && (prompt.contains("SKILL.md") || prompt.contains("<skills>"))
        }
        TaskWeight::SdkCall => prompt.contains("server__tool") || prompt.contains("tools[\""),
        TaskWeight::Process => prompt.contains("tools.shell"),
        TaskWeight::Scout => {
            prompt.contains("scout")
                && prompt.contains("tools.agent")
                && prompt.contains("asyncio.gather")
        }
    }
}

/// The `<tools>` block: the PTC rule plus the SDK for this registry.
#[must_use]
pub fn grouped_execution_block(prompt: &str) -> Option<&str> {
    let start = prompt.find("<tools>")?;
    let end = prompt[start..].find("</tools>")? + "</tools>".len();
    Some(&prompt[start..start + end])
}

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(IDENTITY);
    prompt.push_str("\n\nAvailable tools:");

    let entries = tools.prompt_entries();
    if tools.get("run_code").is_some() {
        prompt.push_str("\n- run_code: the only directly callable tool.");
        if let Some(section) = tools_section(tools) {
            prompt.push_str("\n\n");
            prompt.push_str(&section);
        }
    } else if entries.is_empty() {
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

    if let Some(section) = web_section(tools) {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    if tools.get("run_code").is_none()
        && let Some(shell_tool) = shell_tool_name(tools)
    {
        prompt.push_str("\n\n");
        prompt.push_str(&tool_calling(&shell_tool));
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

    fn registry_with(shell: ShellKind, program: &str) -> ToolRegistry {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(ShellTool::forcing(DetectedShell {
            kind: shell,
            program: PathBuf::from(program),
        })));
        registry
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
        assert!(prompt.contains("here-string"), "{prompt}");
        assert!(!prompt.contains("quoted heredoc"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        assert!(
            !prompt.contains("`shell`"),
            "the model must not be told to call `shell`:\n{prompt}"
        );
    }

    #[test]
    fn bash_prompt_teaches_a_quoted_heredoc() {
        let registry = registry_with(ShellKind::Bash, "/bin/bash");
        let prompt = build_system_prompt(&registry);
        assert!(prompt.contains("`bash`"), "{prompt}");
        assert!(prompt.contains("quoted heredoc"), "{prompt}");
        assert!(
            prompt.contains("`write`, `edit`, and `bash`"),
            "write and edit stay alongside bash, got:\n{prompt}"
        );
        assert!(
            prompt.contains("`mode` `script`"),
            "file edits belong on script mode, got:\n{prompt}"
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
            description.contains("quoted heredoc"),
            "bash description must allow Python heredocs, got:\n{description}"
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
        assert!(prompt.contains("here-string"), "{prompt}");
        let shell = ShellTool::forcing(DetectedShell {
            kind: ShellKind::Pwsh,
            program: PathBuf::from("pwsh.exe"),
        });
        assert!(shell.description().contains("PowerShell 7"));
        assert!(!shell.description().contains("syntax errors"));
    }

    #[test]
    fn ptc_block_lists_only_the_registry_and_forbids_direct_calls() {
        let parent = ToolRegistry::new();
        register_builtins(&parent);
        let child = ToolRegistry::new();
        child.register(Arc::new(mycode_tools::builtin::ReadTool));
        child.register(Arc::new(mycode_tools::builtin::GrepTool));
        child.register(Arc::new(mycode_tools::builtin::RunCodeTool));
        let parent_prompt = build_system_prompt(&parent);
        let child_prompt = build_system_prompt(&child);
        let parent_block = super::grouped_execution_block(&parent_prompt).expect("parent block");
        let child_block = super::grouped_execution_block(&child_prompt).expect("child block");
        assert!(
            parent_block.contains("only tool you can call directly"),
            "{parent_block}"
        );
        assert!(child_block.contains("only tool you can call directly"));
        assert!(parent_block.contains("async def read("));
        assert!(parent_block.contains("async def shell("), "{parent_block}");
        assert!(child_block.contains("async def grep("));
        assert!(!child_block.contains("async def shell("), "{child_block}");
        assert!(!child_block.contains("async def write("));
        assert!(parent_block.contains("tools.shell"));
        assert!(parent_block.contains("Never call `subprocess`"));
        assert!(!parent_block.contains("does not need to be installed"));
        assert!(!parent_block.contains("one obvious call stays direct"));
        assert!(!parent_prompt.contains("console.log"));
        let again = build_system_prompt(&parent);
        assert_eq!(
            parent_prompt, again,
            "the tools section must stay byte-stable"
        );
    }

    #[test]
    fn choice_prompts_guide_difficulty_without_forcing_scout() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        let prompt = build_system_prompt(&registry);
        assert!(
            super::prompt_guides(&prompt, super::TaskWeight::Direct),
            "{prompt}"
        );
        assert!(
            super::prompt_guides(&prompt, super::TaskWeight::GroupInline),
            "{prompt}"
        );
        assert!(
            super::prompt_guides(&prompt, super::TaskWeight::Process),
            "{prompt}"
        );
        assert!(
            super::prompt_guides(&prompt, super::TaskWeight::SdkCall),
            "{prompt}"
        );
        assert!(
            !super::prompt_guides(&prompt, super::TaskWeight::Scout),
            "without agent, scout gather is not guided:\n{prompt}"
        );
        assert!(
            !super::prompt_guides(&prompt, super::TaskWeight::Skill),
            "the tools block does not list the skill catalog:\n{prompt}"
        );
        assert!(super::prompt_guides(
            "<skills>\nload it inside `run_code` with `await tools.read(path=\"...\")` on that SKILL.md path\n</skills>",
            super::TaskWeight::Skill
        ));
        for scenario in super::choice_scenarios() {
            if scenario.weight == super::TaskWeight::Broad {
                assert!(
                    !super::prompt_guides(&prompt, scenario.weight),
                    "without agent, broad research is not aimed at scout:\n{prompt}"
                );
            }
        }
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "grep",
            &serde_json::json!({"pattern": "dispatch_tool"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find dispatch", "code": "hits = await tools.grep(pattern='dispatch_tool')\nreturn hits"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find dispatch", "code": "hits = await tools[\"grep\"](pattern='dispatch_tool')\nreturn hits"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Delegate", "code": "return await tools.agent(agent='scout', prompt='find dispatch')"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "bash",
            &serde_json::json!({"command": "rg dispatch"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::GroupInline,
            "run_code",
            &serde_json::json!({"description": "Find dispatch", "code": "return 1"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::GroupInline,
            "run_code",
            &serde_json::json!({"description": "Read both", "code": "a, b = await asyncio.gather(tools.read(path='src/a.rs'), tools.read(path='src/b.rs'))\nreturn a"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::GroupInline,
            "agent",
            &serde_json::json!({"agent": "scout"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "edit",
            &serde_json::json!({"path": "src/a.rs"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "await tools.edit(path='src/a.rs')"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "return 1"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "await tools.shell(command='sed -i s/recieve/receive/ src/a.rs')"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Broad,
            "run_code",
            &serde_json::json!({"description": "Map auth", "code": "return await tools.agent(agent='scout', prompt='map auth')"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Broad,
            "run_code",
            &serde_json::json!({"description": "Map auth inline", "code": "return 1"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Broad,
            "web_search",
            &serde_json::json!({"query": "auth"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Broad,
            "run_code",
            &serde_json::json!({"description": "Map auth", "code": "   "})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "# await tools.grep(pattern='dispatch_tool')\nreturn 'src/turn.rs:1'"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "note = \"await tools.grep(pattern='dispatch_tool')\"\nreturn note"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "hits = await tools.grep(pattern=\"tools.shell\")\nreturn hits"})
        ));
        assert!(super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "hits = await tools[ \"grep\" ](pattern='dispatch_tool')\nreturn hits"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "return await mytools.grep(pattern='dispatch_tool')"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "return await tools.grep_extra(pattern='dispatch_tool')"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Direct,
            "run_code",
            &serde_json::json!({"description": "One edit", "code": "await tools.edit(path='src/a.rs')\nawait tools.bash(command='true')"})
        ));
        assert!(!super::accept_choice(
            super::TaskWeight::Lookup,
            "run_code",
            &serde_json::json!({"description": "Find", "code": "await tools.grep(pattern='dispatch_tool')\nawait tools.shell(command='rg dispatch')"})
        ));
    }

    #[test]
    fn scout_guidance_appears_when_agent_is_registered() {
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
            super::prompt_guides(&prompt, super::TaskWeight::Scout),
            "{prompt}"
        );
        assert!(prompt.contains("agent \"scout\""), "{prompt}");
    }

    #[test]
    fn sdk_quotes_names_that_are_not_python_identifiers() {
        let keyword = super::python_signature("class", &serde_json::json!({"type": "object"}));
        assert!(
            keyword.contains("tools[\"class\"]"),
            "keywords need quoted access: {keyword}"
        );
        let mcp = super::python_signature(
            "docs__lookup",
            &serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
        );
        assert!(
            mcp.contains("async def docs__lookup(*, query: str)"),
            "{mcp}"
        );
    }
}
