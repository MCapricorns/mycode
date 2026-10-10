//! MYCode desktop application entry point.
//
// Release builds run windowed: no console flashes on Windows. Debug builds
// keep the console so eprintln diagnostics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::borrow::Cow;

use mycode_config::{HomeEnv, HomeLayout};
use mycode_desktop::workspace;

/// Kit icons plus the embedded application mark used in the title bar.
struct BrandAssets;

impl gpui_kit::AssetSource for BrandAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if path == "brand/icon.ico" {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/icon.ico"))));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        gpui_kit::assets::AllAssets.list(path)
    }
}

fn emit_early_text(text: &str) {
    #[cfg(windows)]
    {
        write_parent_console(text);
    }
    #[cfg(not(windows))]
    {
        print!("{text}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// Attaches the parent console and writes `text` there.
///
/// A `windows_subsystem = "windows"` binary starts with no stdout. `println!`
/// would not reach the terminal that launched `--version` or `--help`.
#[cfg(windows)]
fn write_parent_console(text: &str) {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    // SAFETY: AttachConsole borrows no pointers. ATTACH_PARENT_PROCESS asks
    // for the console of the process that started this one. Failure leaves
    // the process without a console; the WriteFile path then falls back.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    // SAFETY: STD_OUTPUT_HANDLE is the documented constant. The returned
    // handle is borrowed from the process and is not closed here.
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let invalid = handle.is_null() || (handle as isize) == -1;
    if invalid {
        print!("{text}");
        return;
    }
    let bytes = text.as_bytes();
    let mut written = 0_u32;
    // SAFETY: `bytes` is readable for this length and `written` is a live
    // u32. A null OVERLAPPED means a synchronous write.
    let wrote = unsafe {
        WriteFile(
            handle,
            bytes.as_ptr().cast(),
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            &raw mut written,
            std::ptr::null_mut(),
        )
    };
    if wrote == 0 {
        print!("{text}");
    }
}

fn install_surface_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .map(str::to_owned)
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        if let Some(notice) = mycode_desktop::cli::surface_failure_message(&message) {
            eprintln!("{notice}");
            std::process::exit(1);
        }
        previous(info);
    }));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match mycode_app::parse_apply_update_args(&args) {
        Ok(Some(request)) => {
            let code = match mycode_app::run_apply_update_helper(&request) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("updater: {error}");
                    1
                }
            };
            std::process::exit(code);
        }
        Err(error) => {
            eprintln!("updater: {error}");
            std::process::exit(1);
        }
        Ok(None) => {}
    }
    if let Some(action) = mycode_desktop::cli::early_action(&args) {
        let text = match action {
            mycode_desktop::cli::EarlyAction::Version => {
                format!("{}\n", mycode_desktop::cli::version_line())
            }
            mycode_desktop::cli::EarlyAction::Help => mycode_desktop::cli::help_text(),
        };
        // Release builds are windowed, so stdout is not a console until the
        // parent console is attached. Debug builds already have one.
        emit_early_text(&text);
        std::process::exit(0);
    }
    #[cfg(target_os = "linux")]
    if !mycode_desktop::cli::linux_display_available(
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
    ) {
        eprintln!("{}", mycode_desktop::cli::missing_display_message());
        std::process::exit(1);
    }
    install_surface_panic_hook();
    // Remove staging directories left behind by earlier self-updates.
    // The apply helper returns before this, so it does not delete the
    // staged binary it is installing.
    mycode_app::cleanup_stale_stages();
    let home = match HomeLayout::from_env(HomeEnv::from_process()) {
        Ok(home) => home,
        Err(error) => {
            eprintln!("mycode home unavailable: {error}");
            std::process::exit(1);
        }
    };
    gpui_kit::application()
        .with_assets(BrandAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            workspace::open_window(home, cx);
        });
}
