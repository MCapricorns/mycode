//! Proves `shell` program mode launches a kernel image on each product target.
//!
//! The module is empty off those targets: Windows x64, Windows ARM64, and
//! macOS Apple Silicon.
#![cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "macos", target_arch = "aarch64"),
))]

use mycode_providers::message::ContentBlock;

use crate::builtin::shell::{ShellArgs, ShellMode, ShellTool};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolResult};

#[tokio::test]
async fn native_image_launches() {
    let (program, args, image_prefix, marker) = launch_target();
    let tool = ShellTool::with_default_timeout(30);
    let ctx = ToolCtx::new(std::env::temp_dir());
    let mut out = ToolStream::closed();
    let result = tool
        .execute(
            ShellArgs {
                mode: ShellMode::Program,
                command: None,
                program: Some(program),
                args,
                timeout_secs: Some(30),
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("shell program mode should spawn the native image");
    let text = text_of(&result);
    assert!(!result.is_error, "native image failed: {text}");
    let details = result.details.expect("shell program-mode details");
    let image = details["image"].as_str().expect("image kind");
    assert!(
        image.starts_with(image_prefix),
        "image {image} does not start with {image_prefix}: {details}"
    );
    match marker {
        Some(marker) => assert!(
            text.contains(marker),
            "captured stdout is missing {marker}: {text}"
        ),
        None => assert!(
            !text.trim().is_empty(),
            "captured stdout was empty: {text:?}"
        ),
    }
}

/// Script mode launches the platform shell discovery picked. On a Windows
/// host whose only PowerShell 7 is the Microsoft Store build, that shell is
/// the `WindowsApps\pwsh.exe` execution alias.
#[tokio::test]
async fn script_mode_runs_the_resolved_platform_shell() {
    let tool = ShellTool::with_default_timeout(30);
    let ctx = ToolCtx::new(std::env::temp_dir());
    let mut out = ToolStream::closed();
    let result = tool
        .execute(
            ShellArgs {
                mode: ShellMode::Script,
                command: Some("echo mycode-script-ok".to_owned()),
                program: None,
                args: Vec::new(),
                timeout_secs: Some(30),
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("script mode should launch the resolved shell");
    let text = text_of(&result);
    assert!(!result.is_error, "script mode failed: {text}");
    assert!(text.contains("mycode-script-ok"), "{text}");
}

#[cfg(all(windows, any(target_arch = "x86_64", target_arch = "aarch64")))]
fn launch_target() -> (String, Vec<String>, &'static str, Option<&'static str>) {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let program = std::path::Path::new(&root)
        .join("System32")
        .join("whoami.exe");
    let program = program
        .to_str()
        .expect("SystemRoot whoami.exe path is Unicode")
        .to_owned();
    (program, Vec::new(), "pe", None)
}

#[cfg(not(windows))]
fn launch_target() -> (String, Vec<String>, &'static str, Option<&'static str>) {
    (
        "/bin/echo".to_owned(),
        vec!["mycode-shell-ok".to_owned()],
        "mach-o",
        Some("mycode-shell-ok"),
    )
}

#[cfg(not(windows))]
#[tokio::test]
async fn shebang_wrapper_runs_its_interpreter() {
    let dir = std::env::temp_dir().join(format!("mycode-shebang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("wrap.sh");
    std::fs::write(&script, "#!/bin/bash\nprintf 'wrapper-ok %s\\n' \"$1\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    let program = script.to_str().expect("script path").to_owned();
    let tool = ShellTool::with_default_timeout(30);
    let ctx = ToolCtx::new(&dir);
    let mut out = ToolStream::closed();
    let result = tool
        .execute(
            ShellArgs {
                mode: ShellMode::Program,
                command: None,
                program: Some(program),
                args: vec!["shebang-ok".to_owned()],
                timeout_secs: Some(30),
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("shebang wrapper should run");
    let text = text_of(&result);
    assert!(!result.is_error, "{text}");
    assert!(text.contains("wrapper-ok shebang-ok"), "{text}");
    let details = result.details.expect("details");
    assert!(
        details["image"]
            .as_str()
            .unwrap_or("")
            .starts_with("mach-o"),
        "{details}"
    );
    let wrapper = details["wrapper"].as_str().unwrap_or("");
    assert!(wrapper.ends_with("wrap.sh"), "{details}");
    assert!(
        details["program"].as_str().unwrap_or("").contains("bash"),
        "{details}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(windows))]
#[tokio::test]
async fn shebang_command_flag_names_the_next_step() {
    let dir = std::env::temp_dir().join(format!("mycode-shebang-c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("bad.sh");
    std::fs::write(&script, "#!/bin/bash -c\necho hi\n").unwrap();
    let program = script.to_str().expect("script path").to_owned();
    let tool = ShellTool::with_default_timeout(30);
    let ctx = ToolCtx::new(&dir);
    let mut out = ToolStream::closed();
    let error = tool
        .execute(
            ShellArgs {
                mode: ShellMode::Program,
                command: None,
                program: Some(program),
                args: Vec::new(),
                timeout_secs: Some(30),
            },
            &ctx,
            &mut out,
        )
        .await
        .expect_err("shebang -c must be rejected");
    let text = error.to_string();
    assert!(text.contains("-c"), "{text}");
    assert!(text.contains("mode script"), "{text}");
    assert!(!text.contains("kernel-loadable images only"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn text_of(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
