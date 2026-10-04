//! Proves `exec` launches a kernel image on each product target.
//!
//! The module is empty off those targets. CI runs `native_image_launches`
//! on Windows x64, Windows ARM64, Linux x86_64 GNU, and macOS Apple Silicon.
#![cfg(any(
    all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
))]

use mycode_core::message::ContentBlock;

use super::{ExecArgs, ExecTool};
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolResult};

#[tokio::test]
async fn native_image_launches() {
    let (program, args, image_prefix, marker) = launch_target();
    let tool = ExecTool::with_default_timeout(30);
    let ctx = ToolCtx::new(std::env::temp_dir());
    let mut out = ToolStream::closed();
    let result = tool
        .execute(
            ExecArgs {
                program,
                args,
                timeout_secs: Some(30),
            },
            &ctx,
            &mut out,
        )
        .await
        .expect("exec should spawn the native image");
    let text = text_of(&result);
    assert!(!result.is_error, "native image failed: {text}");
    let details = result.details.expect("exec details");
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
    let image = if cfg!(target_os = "linux") {
        "elf"
    } else {
        "mach-o"
    };
    (
        "/bin/echo".to_owned(),
        vec!["mycode-exec-ok".to_owned()],
        image,
        Some("mycode-exec-ok"),
    )
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
