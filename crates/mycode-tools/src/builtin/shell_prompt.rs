//! Model-facing copy for the one shell tool the active interpreter selects.
//!
//! Pi ships `bash` and `powershell` as separate tools and shows the model
//! only the tools that are enabled. Codex, Gemini CLI, and OpenCode keep a
//! single shell tool but describe the real interpreter. This crate follows
//! the stricter rule: one tool, named and documented for that interpreter.

use std::path::Path;

use serde_json::{Value, json};

use super::detect::{DetectedShell, ShellKind};

/// Tool description sent in the tool spec.
#[must_use]
pub(crate) fn tool_description(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => POWERSHELL7_DESCRIPTION,
        ShellKind::WindowsPowerShell => POWERSHELL51_DESCRIPTION,
        ShellKind::Bash => BASH_DESCRIPTION,
        ShellKind::Zsh => ZSH_DESCRIPTION,
        ShellKind::Sh => SH_DESCRIPTION,
        ShellKind::Cmd => CMD_DESCRIPTION,
    }
}

/// One-line system-prompt snippet for the active shell tool.
#[must_use]
pub(crate) fn prompt_snippet(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => POWERSHELL_SNIPPET,
        ShellKind::Bash => BASH_SNIPPET,
        ShellKind::Zsh => ZSH_SNIPPET,
        ShellKind::Sh => SH_SNIPPET,
        ShellKind::Cmd => CMD_SNIPPET,
    }
}

/// Short label used in the environment block.
#[must_use]
pub(crate) fn shell_label(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => "PowerShell 7",
        ShellKind::WindowsPowerShell => "Windows PowerShell 5.1",
        ShellKind::Bash => "bash",
        ShellKind::Zsh => "zsh",
        ShellKind::Sh => "sh",
        ShellKind::Cmd => "cmd.exe",
    }
}

/// Operating rules for the active shell. Inserted once in the system prompt.
#[must_use]
pub(crate) fn guidance(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => POWERSHELL7_GUIDANCE,
        ShellKind::WindowsPowerShell => POWERSHELL51_GUIDANCE,
        ShellKind::Bash => BASH_GUIDANCE,
        ShellKind::Zsh => ZSH_GUIDANCE,
        ShellKind::Sh => SH_GUIDANCE,
        ShellKind::Cmd => CMD_GUIDANCE,
    }
}

/// Schema description for `command`.
#[must_use]
pub(crate) fn command_parameter_doc(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => {
            "PowerShell 7 command. Required when `mode` is `script`. Use cmdlets, pipelines, `$env:NAME`, and PowerShell operators (`&&` is allowed). Do not send bash or cmd. Omit for `program`."
        }
        ShellKind::WindowsPowerShell => {
            "Windows PowerShell 5.1 command. Required when `mode` is `script`. Chain with `;` and `$LASTEXITCODE`. `&&` and `||` are syntax errors. Do not send bash or cmd. Omit for `program`."
        }
        ShellKind::Bash => {
            "Bash command passed to `bash -c`. Required when `mode` is `script`. Use bash quoting, pipes, and `&&`. The path separator is `/`. Omit for `program`."
        }
        ShellKind::Zsh => {
            "Zsh command passed to `zsh -c`. Required when `mode` is `script`. Use zsh quoting, pipes, and `&&`. The path separator is `/`. Omit for `program`."
        }
        ShellKind::Sh => {
            "POSIX sh command passed to `sh -c`. Required when `mode` is `script`. Use POSIX quoting, pipes, and `&&`. The path separator is `/`. Omit for `program`."
        }
        ShellKind::Cmd => {
            "cmd.exe command passed to `/d /s /c`. Required when `mode` is `script`. Use `%VAR%` and cmd quoting. Omit for `program`."
        }
    }
}

/// Schema description for `mode`.
#[must_use]
pub(crate) fn mode_parameter_doc(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Pwsh => {
            "`script` runs `command` in PowerShell 7. `program` spawns one kernel-loadable image with `program` and `args` and does not start PowerShell."
        }
        ShellKind::WindowsPowerShell => {
            "`script` runs `command` in Windows PowerShell 5.1. `program` spawns one kernel-loadable image with `program` and `args` and does not start PowerShell."
        }
        ShellKind::Bash => {
            "`script` runs `command` in bash. `program` spawns one kernel-loadable image with `program` and `args` and does not start a shell."
        }
        ShellKind::Zsh => {
            "`script` runs `command` in zsh. `program` spawns one kernel-loadable image with `program` and `args` and does not start a shell."
        }
        ShellKind::Sh => {
            "`script` runs `command` in POSIX sh. `program` spawns one kernel-loadable image with `program` and `args` and does not start a shell."
        }
        ShellKind::Cmd => {
            "`script` runs `command` in cmd.exe. `program` spawns one kernel-loadable image with `program` and `args` and does not start a shell."
        }
    }
}

/// Replaces parameter descriptions so the schema matches the active shell.
pub(crate) fn apply_parameter_docs(schema: &mut Value, kind: ShellKind) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    set_description(properties.get_mut("command"), command_parameter_doc(kind));
    set_description(properties.get_mut("mode"), mode_parameter_doc(kind));
    set_description(
        properties.get_mut("program"),
        "Executable basename or path. Required when `mode` is `program`. A bare name is searched only in absolute host PATH entries. A path is resolved against the session cwd. Omit for `script`.",
    );
    set_description(
        properties.get_mut("args"),
        "Argument vector for `program` mode, passed verbatim with no shell parsing.",
    );
    set_description(
        properties.get_mut("timeout_secs"),
        "Timeout in seconds for this command (default: 120).",
    );
}

fn set_description(value: Option<&mut Value>, description: &str) {
    let Some(object) = value.and_then(Value::as_object_mut) else {
        return;
    };
    object.insert("description".to_owned(), json!(description));
}

/// Environment plus shell rules. `os` is `std::env::consts::OS`.
#[must_use]
pub fn render_environment_block(os: &str, arch: &str, cwd: &Path, shell: &DetectedShell) -> String {
    format!(
        "<environment>\n\
os: {os} ({arch})\n\
cwd: {cwd}\n\
shell_tool: {tool}\n\
interpreter: {label} ({program})\n\
</environment>\n\n\
<shell>\n\
{guidance}\n\
</shell>",
        cwd = cwd.display(),
        tool = shell.kind.tool_name(),
        label = shell_label(shell.kind),
        program = shell.program.display(),
        guidance = guidance(shell.kind),
    )
}

const POWERSHELL7_DESCRIPTION: &str = "\
Execute a PowerShell 7 command in the session cwd and return stdout and stderr. \
`command` is PowerShell, not bash or cmd: cmdlets (`Get-ChildItem`, `Select-String`), \
pipelines, `$env:NAME`, and PowerShell operators. PowerShell 7 accepts `&&` and `||`. \
Do not use bash syntax (`2>/dev/null`, `$(...)`, `<<` heredocs). Quote paths that \
contain spaces; `\\` is the separator and `/` is also accepted. Each call is a new \
process: `cd` and variable assignments do not persist. To edit a file from this tool, \
pipe a literal here-string to Python (`@'...'@ | python -`). `mode` `program` spawns \
`program` with an explicit `args` vector and does not start PowerShell; only a \
kernel-loadable PE, ELF, or Mach-O image is accepted. `write` and `edit` remain \
available. `read`, `grep`, and `find` stay in-process. Do not use this tool to talk \
to the user. Edits are not undone. Execution is unsandboxed current-user execution. \
Captured stdout/stderr is truncated beyond 50 KiB. A non-zero exit is an error result, \
not a tool failure. Default timeout: 120 s. There is no permission prompt. Output is \
captured as UTF-8.";

const POWERSHELL51_DESCRIPTION: &str = "\
Execute a Windows PowerShell 5.1 command in the session cwd and return stdout and \
stderr. `command` is Windows PowerShell, not bash, not cmd, and not PowerShell 7-only \
syntax. `&&` and `||` are syntax errors; chain with `;` and check `$LASTEXITCODE`. \
Use cmdlets (`Get-ChildItem`, `Select-String`), `$env:NAME`, and `2>$null` instead of \
`2>/dev/null`. Quote paths that contain spaces. Each call is a new process: `cd` and \
variable assignments do not persist. To edit a file, pipe a literal here-string to \
Python (`@'...'@ | python -`). `mode` `program` spawns `program` with an explicit \
`args` vector and does not start PowerShell; only a kernel-loadable PE, ELF, or \
Mach-O image is accepted. `write` and `edit` remain available. `read`, `grep`, and \
`find` stay in-process. Do not use this tool to talk to the user. Edits are not \
undone. Execution is unsandboxed current-user execution. Captured stdout/stderr is \
truncated beyond 50 KiB. A non-zero exit is an error result, not a tool failure. \
Default timeout: 120 s. There is no permission prompt. Output is captured as UTF-8.";

const BASH_DESCRIPTION: &str = "\
Execute a bash command in the session cwd (`bash -c`) and return stdout and stderr. \
Use bash syntax: pipes, `&&` / `||`, redirection, `$VAR`, and `$(...)`. The path \
separator is `/`. Each call is a new process; `cd` and exports do not persist. To \
edit a file, run Python (`python3` or `python`) with a quoted heredoc or a short \
script. `mode` `program` spawns `program` with an explicit `args` vector and does \
not start a shell; only a kernel-loadable PE, ELF, or Mach-O image is accepted. \
`write` and `edit` remain available. `read`, `grep`, and `find` stay in-process. \
Do not use this tool to talk to the user. Edits are not undone. Execution is \
unsandboxed current-user execution. Captured stdout/stderr is truncated beyond \
50 KiB. A non-zero exit is an error result, not a tool failure. Default timeout: \
120 s. There is no permission prompt.";

const ZSH_DESCRIPTION: &str = "\
Execute a zsh command in the session cwd (`zsh -c`) and return stdout and stderr. \
Use zsh syntax: pipes, `&&` / `||`, redirection, `$VAR`, and `$(...)`. The path \
separator is `/`. Each call is a new process; `cd` and exports do not persist. To \
edit a file, run Python (`python3` or `python`) with a quoted heredoc or a short \
script. `mode` `program` spawns `program` with an explicit `args` vector and does \
not start a shell; only a kernel-loadable PE, ELF, or Mach-O image is accepted. \
`write` and `edit` remain available. `read`, `grep`, and `find` stay in-process. \
Do not use this tool to talk to the user. Edits are not undone. Execution is \
unsandboxed current-user execution. Captured stdout/stderr is truncated beyond \
50 KiB. A non-zero exit is an error result, not a tool failure. Default timeout: \
120 s. There is no permission prompt.";

const SH_DESCRIPTION: &str = "\
Execute a POSIX sh command in the session cwd (`sh -c`) and return stdout and \
stderr. Use POSIX syntax: pipes, `&&` / `||`, redirection, `$VAR`, and `$(...)`. \
Do not rely on bash arrays or `[[ ]]`. The path separator is `/`. Each call is a \
new process; `cd` and exports do not persist. To edit a file, run Python \
(`python3` or `python`) with a quoted heredoc or a short script. `mode` `program` \
spawns `program` with an explicit `args` vector and does not start a shell; only \
a kernel-loadable PE, ELF, or Mach-O image is accepted. `write` and `edit` remain \
available. `read`, `grep`, and `find` stay in-process. Do not use this tool to \
talk to the user. Edits are not undone. Execution is unsandboxed current-user \
execution. Captured stdout/stderr is truncated beyond 50 KiB. A non-zero exit is \
an error result, not a tool failure. Default timeout: 120 s. There is no \
permission prompt.";

const CMD_DESCRIPTION: &str = "\
Execute a cmd.exe command in the session cwd (`cmd.exe /d /s /c`) and return stdout \
and stderr. PowerShell was not found, so this is the last-resort shell. Use cmd \
syntax: `%VAR%`, quoted Windows paths, and `&` or `&&` to chain. \
Each call is a new process. Prefer `write` and `edit` for file changes. `mode` \
`program` spawns `program` with an explicit `args` vector and does not start a \
shell; only a kernel-loadable PE, ELF, or Mach-O image is accepted. `read`, \
`grep`, and `find` stay in-process. Do not use this tool to talk to the user. \
Edits are not undone. Execution is unsandboxed current-user execution. Captured \
stdout/stderr is truncated beyond 50 KiB. A non-zero exit is an error result, \
not a tool failure. Default timeout: 120 s. There is no permission prompt.";

const POWERSHELL_SNIPPET: &str = "\
powershell: mode script runs one PowerShell command (command), including a \
here-string piped to python that edits files; mode program runs one \
kernel-loadable binary with explicit args and no shell. Optional timeout_secs.";

const BASH_SNIPPET: &str = "\
bash: mode script runs one bash command (command), including a Python heredoc \
or short script that edits files; mode program runs one kernel-loadable binary \
with explicit args and no shell. Optional timeout_secs.";

const ZSH_SNIPPET: &str = "\
zsh: mode script runs one zsh command (command), including a Python heredoc \
or short script that edits files; mode program runs one kernel-loadable binary \
with explicit args and no shell. Optional timeout_secs.";

const SH_SNIPPET: &str = "\
sh: mode script runs one POSIX sh command (command), including a Python heredoc \
or short script that edits files; mode program runs one kernel-loadable binary \
with explicit args and no shell. Optional timeout_secs.";

const CMD_SNIPPET: &str = "\
cmd: mode script runs one cmd.exe command (command); mode program runs one \
kernel-loadable binary with explicit args and no shell. Optional timeout_secs.";

const POWERSHELL7_GUIDANCE: &str = "\
The `powershell` tool runs PowerShell 7 (`pwsh`). Write PowerShell, not bash or cmd.
- Each call starts a new process in the session cwd. `cd` and `$env:` assignments do not persist.
- `&&` and `||` are allowed. Read environment variables with `$env:NAME`. Redirect errors with `2>$null`, not `2>/dev/null`.
- The path separator is `\\`. Quote paths that contain spaces. `/` is also accepted.
- Inspect files with `read`, `grep`, and `find` before reaching for the shell.";

const POWERSHELL51_GUIDANCE: &str = "\
The `powershell` tool runs Windows PowerShell 5.1 (`powershell.exe`), not PowerShell 7 and not bash.
- Do not use `&&` or `||`. They are syntax errors on 5.1. Chain with `;` and test `$LASTEXITCODE`.
- Do not use bash heredocs, `$(...)`, or `2>/dev/null`. Use `$env:NAME` and `2>$null`.
- The ternary operator and `&&` pipelines are PowerShell 7 only.
- Each call starts a new process in the session cwd. `cd` and `$env:` assignments do not persist.
- The path separator is `\\`. Quote paths that contain spaces.
- Inspect files with `read`, `grep`, and `find` before reaching for the shell.";

const BASH_GUIDANCE: &str = "\
The `bash` tool runs bash (`bash -c`). Write bash, not PowerShell or cmd.
- Each call starts a new process in the session cwd. `cd` and `export` do not persist.
- Pipes, `&&`, `||`, `$VAR`, and `$(...)` are bash.
- The path separator is `/`. Quote paths that contain spaces.
- Inspect files with `read`, `grep`, and `find` before reaching for the shell.";

const ZSH_GUIDANCE: &str = "\
The `zsh` tool runs zsh (`zsh -c`). Write zsh, not bash-only syntax, PowerShell, or cmd.
- Each call starts a new process in the session cwd. `cd` and `export` do not persist.
- Pipes, `&&`, `||`, `$VAR`, and `$(...)` are zsh. The path separator is `/`.
- Inspect files with `read`, `grep`, and `find` before reaching for the shell.";

const SH_GUIDANCE: &str = "\
The `sh` tool runs POSIX sh (`sh -c`). Write POSIX sh, not bash arrays, `[[ ]]`, PowerShell, or cmd.
- Each call starts a new process in the session cwd. `cd` and `export` do not persist.
- Pipes, `&&`, `||`, `$VAR`, and `$(...)` are POSIX. The path separator is `/`.
- Inspect files with `read`, `grep`, and `find` before reaching for the shell.";

const CMD_GUIDANCE: &str = "\
The `cmd` tool runs `cmd.exe /d /s /c` because PowerShell was not found.
- Use `%VAR%` and cmd quoting. This is not bash and not PowerShell.
- Each call starts a new process in the session cwd.
- Prefer `read`, `grep`, `find`, `write`, and `edit` over cmd for file work.";

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{guidance, render_environment_block};
    use crate::builtin::shell::{DetectedShell, ShellKind};

    #[test]
    fn environment_block_matches_the_interpreter() {
        let cwd = Path::new("/work");
        let ps51 = DetectedShell {
            kind: ShellKind::WindowsPowerShell,
            program: PathBuf::from("powershell.exe"),
        };
        let block = render_environment_block("windows", "x86_64", cwd, &ps51);
        assert!(block.contains("shell_tool: powershell"), "{block}");
        assert!(block.contains("Windows PowerShell 5.1"), "{block}");
        assert!(block.contains("Do not use `&&`"), "{block}");
        assert!(!block.contains("translated"), "{block}");
        assert!(
            guidance(ShellKind::WindowsPowerShell).contains("syntax errors")
                || block.contains("syntax errors")
        );

        let pwsh = DetectedShell {
            kind: ShellKind::Pwsh,
            program: PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"),
        };
        let block = render_environment_block("windows", "x86_64", cwd, &pwsh);
        assert!(block.contains("`&&` and `||` are allowed"), "{block}");
        assert!(!block.contains("syntax errors"), "{block}");
        assert!(block.contains("shell_tool: powershell"), "{block}");

        let bash = DetectedShell {
            kind: ShellKind::Bash,
            program: PathBuf::from("/bin/bash"),
        };
        let block = render_environment_block("linux", "x86_64", cwd, &bash);
        assert!(block.contains("shell_tool: bash"), "{block}");
        assert!(block.contains("Write bash, not PowerShell"), "{block}");
        assert!(block.contains("The path separator is `/`."), "{block}");
        assert!(!block.contains("Git Bash"), "{block}");

        let zsh = DetectedShell {
            kind: ShellKind::Zsh,
            program: PathBuf::from("/bin/zsh"),
        };
        let block = render_environment_block("macos", "aarch64", cwd, &zsh);
        assert!(block.contains("shell_tool: zsh"), "{block}");
        assert!(block.contains("Write zsh"), "{block}");
        assert!(block.contains("The path separator is `/`."), "{block}");

        let sh = DetectedShell {
            kind: ShellKind::Sh,
            program: PathBuf::from("/bin/sh"),
        };
        let block = render_environment_block("linux", "x86_64", cwd, &sh);
        assert!(block.contains("shell_tool: sh"), "{block}");
        assert!(block.contains("POSIX"), "{block}");
        assert!(!block.contains("translated"), "{block}");
    }
}
