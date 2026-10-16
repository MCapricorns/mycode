use super::{ShellArgs, ShellMode, ShellTool, validate_shell_args};
use crate::registry::ToolRegistry;
use crate::tool::{Tool, ToolDyn, ToolError};
use crate::{ToolCtx, ToolStream, register_builtins};

#[test]
fn schema_advertises_one_tool_and_two_modes() {
    let tool = ShellTool::new();
    let spec = ToolDyn::spec(&tool);
    let expected = super::resolved_shell().kind.tool_name();
    assert_eq!(spec.name, expected);
    assert!(matches!(
        spec.name.as_str(),
        "powershell" | "bash" | "zsh" | "sh" | "cmd"
    ));
    let schema = spec.params_schema.to_string();
    assert!(schema.contains("\"script\""), "{schema}");
    assert!(schema.contains("\"program\""), "{schema}");
    assert!(!schema.contains("\"exec\""), "{schema}");
    let snippet = tool.prompt_snippet().expect("snippet");
    assert!(snippet.contains("script"));
    assert!(snippet.contains("program"));
    assert!(!snippet.contains("exec"));
    assert!(!spec.description.contains("`exec`"));
    assert!(!spec.description.contains("translated"));
    let required = spec
        .params_schema
        .get("required")
        .and_then(|value| value.as_array());
    let mode_required =
        required.is_some_and(|items| items.iter().any(|item| item.as_str() == Some("mode")));
    assert!(
        !mode_required,
        "mode defaults to script, schema was {schema}"
    );
}

#[test]
fn omitted_mode_is_script() {
    let value = serde_json::json!({"command": "echo hi"});
    crate::tool::validate_args::<ShellArgs>(&value).expect("mode is optional");
    let args: ShellArgs = serde_json::from_value(value).expect("decode");
    assert_eq!(args.mode, ShellMode::Script);
    assert_eq!(args.command.as_deref(), Some("echo hi"));
}

#[test]
fn powershell_and_bash_specs_are_mutually_exclusive() {
    let powershell = ShellTool::forcing(super::DetectedShell {
        kind: super::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    });
    let bash = ShellTool::forcing(super::DetectedShell {
        kind: super::ShellKind::Bash,
        program: std::path::PathBuf::from("/bin/bash"),
    });
    let pwsh = ShellTool::forcing(super::DetectedShell {
        kind: super::ShellKind::Pwsh,
        program: std::path::PathBuf::from("pwsh.exe"),
    });
    let ps_spec = ToolDyn::spec(&powershell);
    let bash_spec = ToolDyn::spec(&bash);
    let pwsh_spec = ToolDyn::spec(&pwsh);
    assert_eq!(ps_spec.name, "powershell");
    assert_eq!(pwsh_spec.name, "powershell");
    assert_eq!(bash_spec.name, "bash");
    assert!(ps_spec.description.contains("Windows PowerShell 5.1"));
    assert!(ps_spec.description.contains("&&"));
    assert!(ps_spec.params_schema.to_string().contains("syntax errors"));
    assert!(pwsh_spec.description.contains("PowerShell 7"));
    assert!(pwsh_spec.description.contains("cmdlets"));
    // Listing and search cmdlets are not advertised: `find` and `grep` own those.
    assert!(!pwsh_spec.description.contains("Get-ChildItem"));
    assert!(
        pwsh_spec
            .description
            .contains("do not use this tool for those")
    );
    assert!(!pwsh_spec.description.contains("translated"));
    assert!(bash_spec.description.contains("bash -c"));
    assert!(bash_spec.description.contains("quoted heredoc"));
    assert!(!bash_spec.description.contains("osSandbox"));
    assert!(!bash_spec.description.contains("Get-ChildItem"));
    let registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(powershell));
    let names = registry.names();
    assert_eq!(names, vec!["powershell".to_owned()]);
    assert!(!names.iter().any(|name| name == "bash" || name == "shell"));
}

#[test]
fn builtins_register_one_shell_tool_not_exec() {
    let registry = ToolRegistry::new();
    register_builtins(&registry);
    let names = registry.names();
    let shell_names: Vec<_> = names
        .iter()
        .filter(|name| {
            matches!(
                name.as_str(),
                "powershell" | "bash" | "zsh" | "sh" | "cmd" | "shell"
            )
        })
        .cloned()
        .collect();
    assert_eq!(
        shell_names,
        vec![super::resolved_shell().kind.tool_name().to_owned()]
    );
    assert!(!names.iter().any(|name| name == "exec" || name == "task"));
}

#[test]
fn modes_reject_the_other_paths_fields() {
    let script = ShellArgs {
        mode: ShellMode::Script,
        command: Some("echo hi".into()),
        program: Some("echo".into()),
        args: Vec::new(),
        timeout_secs: None,
    };
    assert!(matches!(
        validate_shell_args(&script),
        Err(ToolError::InvalidArgs(_))
    ));
    let program = ShellArgs {
        mode: ShellMode::Program,
        command: Some("echo hi".into()),
        program: Some("echo".into()),
        args: vec!["ok".into()],
        timeout_secs: None,
    };
    assert!(matches!(
        validate_shell_args(&program),
        Err(ToolError::InvalidArgs(_))
    ));
    let missing = ShellArgs {
        mode: ShellMode::Program,
        command: None,
        program: None,
        args: Vec::new(),
        timeout_secs: None,
    };
    assert!(matches!(
        validate_shell_args(&missing),
        Err(ToolError::InvalidArgs(_))
    ));
}

#[tokio::test]
async fn invalid_mode_mix_does_not_spawn() {
    let tool = ShellTool::new();
    let ctx = ToolCtx::new(std::env::temp_dir());
    let mut out = ToolStream::closed();
    let error = tool
        .execute(
            ShellArgs {
                mode: ShellMode::Script,
                command: Some("echo hi".into()),
                program: None,
                args: vec!["nope".into()],
                timeout_secs: Some(1),
            },
            &ctx,
            &mut out,
        )
        .await
        .expect_err("mixed script arguments");
    assert!(matches!(error, ToolError::InvalidArgs(_)));
}

fn result_text(result: &crate::tool::ToolResult) -> String {
    match result.content.first() {
        Some(mycode_core::message::ContentBlock::Text(block)) => block.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[test]
fn powershell_launch_keeps_the_command_and_does_not_translate_bash() {
    let shell = super::DetectedShell {
        kind: super::ShellKind::Pwsh,
        program: std::path::PathBuf::from("pwsh.exe"),
    };
    let args = super::script_launch_args(&shell, "ls -la").expect("encode");
    assert!(args.iter().any(|arg| arg == "-EncodedCommand"));
    let encoded = args.last().expect("payload");
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .expect("base64");
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut offset = 0;
    while offset + 1 < bytes.len() {
        units.push(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]));
        offset += 2;
    }
    let script = String::from_utf16(&units).expect("utf-16");
    assert!(script.contains("ls -la"), "{script}");
    assert!(!script.contains("Get-ChildItem"), "{script}");
    assert!(script.contains("[Console]::OutputEncoding"), "{script}");
    assert!(
        !script.contains("translated"),
        "launch script must not announce a rewrite: {script}"
    );

    let windows_ps = super::DetectedShell {
        kind: super::ShellKind::WindowsPowerShell,
        program: std::path::PathBuf::from("powershell.exe"),
    };
    let ps_args = super::script_launch_args(&windows_ps, "Get-ChildItem").expect("encode");
    assert_eq!(
        ps_args[..super::POWERSHELL_ARGUMENTS.len()],
        super::POWERSHELL_ARGUMENTS
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect::<Vec<_>>()
    );

    let bash = super::DetectedShell {
        kind: super::ShellKind::Bash,
        program: std::path::PathBuf::from("bash"),
    };
    assert_eq!(
        super::script_launch_args(&bash, "ls -la").unwrap(),
        vec!["-c".to_owned(), "ls -la".to_owned()]
    );
}

#[test]
fn result_text_does_not_announce_a_translation() {
    let plain = super::format_result(
        None,
        "Get-ChildItem -Force",
        "pwsh.exe",
        crate::builtin::process::CapturedStream::default(),
        crate::builtin::process::CapturedStream::default(),
        12,
        false,
        None,
    );
    assert_eq!(result_text(&plain), "");
    let details = plain.details.as_ref().expect("details");
    assert!(details.get("translated_command").is_none());
    assert!(!result_text(&plain).contains("translated"));
}

#[test]
fn powershell_prologue_accepts_non_ascii_statements_without_slicing_inside_a_scalar() {
    for body in [
        "\"\u{4e2d}\u{1f4a1}\"",
        "\u{76ee}\u{5f55} -Name x",
        "\u{1f680}abc",
    ] {
        let script = super::powershell_script_for(body);
        assert!(script.contains(body));
        assert!(script.find(super::POWERSHELL_UTF8_PRELUDE).unwrap() < script.find(body).unwrap());
    }
}

#[test]
fn powershell_prologue_preserves_attributes_block_comments_using_and_param() {
    let command = "\u{feff}#Requires -Version 5.0\r\n\
<#\r\n\
header (comment)\r\n\
#>\r\n\
using namespace System.IO; using namespace System.Text\r\n\
[CmdletBinding()]\r\n\
[OutputType([string])]\r\n\
param\r\n\
(\r\n\
    [string]$x = '('')', # (comment paren\r\n\
    [string]$y = \"`\")\" <# (block paren #>\r\n\
)\r\n\
Write-Output \"$x $y\"";
    let script = super::powershell_script_for(command);
    let prelude_pos = script
        .find(super::POWERSHELL_UTF8_PRELUDE)
        .expect("prelude inserted");
    let param_end = script.find("Write-Output").expect("body present");
    assert!(!script.starts_with('\u{feff}'), "{script:?}");
    assert!(
        prelude_pos < param_end,
        "prelude must precede the script body: {script}"
    );
    let before = &script[..prelude_pos];
    assert!(before.contains("using namespace System.Text"), "{before}");
    assert!(before.contains("[CmdletBinding()]"), "{before}");
    assert!(before.contains("[OutputType([string])]"), "{before}");
    assert!(before.contains("[string]$y = \"`\")\""), "{before}");

    // A leading type expression `[math]::Abs(-1)` is not a param attribute
    // and must stay after the UTF-8 prelude.
    let type_expr = super::powershell_script_for("[math]::Abs(-1)\nWrite-Output 'ok'");
    let expr_prelude = type_expr.find(super::POWERSHELL_UTF8_PRELUDE).unwrap();
    let expr_body = type_expr.find("[math]::Abs(-1)").unwrap();
    assert!(expr_prelude < expr_body, "{type_expr}");

    // Semicolon-chained `using` on the same line as the body splits right
    // after the `using` statement.
    let inline_using = super::powershell_script_for(
        "using namespace System.IO; Write-Output ([Path]::GetFileName('a/b.txt'))",
    );
    let inline_prelude = inline_using.find(super::POWERSHELL_UTF8_PRELUDE).unwrap();
    let inline_using_pos = inline_using.find("using namespace System.IO;").unwrap();
    let inline_body_pos = inline_using.find("Write-Output").unwrap();
    assert!(
        inline_using_pos < inline_prelude && inline_prelude < inline_body_pos,
        "{inline_using}"
    );
}
