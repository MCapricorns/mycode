//! MYCode desktop application entry point.
//
// Release builds run windowed: no console flashes on Windows. Debug builds
// keep the console so eprintln diagnostics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::borrow::Cow;

use mycode_desktop::workspace;

// The global allocator lives in mycode-core's crate root: a bin target's
// own `#[global_allocator]` here would collide with it.

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
            .unwrap_or_else(|| info.to_string());
        if let Some(notice) = mycode_frontend::launch::surface_failure_message(&message) {
            eprintln!("{notice}");
            std::process::exit(1);
        }
        previous(info);
    }));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The self-update helper entry is shared with every `mycode*` binary.
    if let Some(code) = mycode_frontend::launch::apply_update_from_args(&args) {
        std::process::exit(code);
    }
    if let Some(action) = mycode_frontend::launch::early_action(&args) {
        let text = match action {
            mycode_frontend::launch::EarlyAction::Version => {
                format!(
                    "{}\n",
                    mycode_frontend::launch::version_line("mycode-desktop")
                )
            }
            mycode_frontend::launch::EarlyAction::Help => {
                mycode_frontend::launch::help_text("mycode-desktop")
            }
            mycode_frontend::launch::EarlyAction::Unknown(flag) => {
                emit_early_text(&format!(
                    "mycode: unknown option {flag}\n\n{}",
                    mycode_frontend::launch::help_text("mycode-desktop")
                ));
                std::process::exit(1);
            }
        };
        // Release builds are windowed, so stdout is not a console until the
        // parent console is attached. Debug builds already have one.
        emit_early_text(&text);
        std::process::exit(0);
    }
    install_surface_panic_hook();
    let home = match mycode_frontend::launch::home() {
        Ok(home) => home,
        Err(error) => {
            eprintln!("mycode home unavailable: {error}");
            std::process::exit(1);
        }
    };
    // One desktop per home, claimed before the launch steps clear update
    // files a running desktop may be using. The lock lives until exit.
    let _instance = match mycode_desktop::instance::claim(&home) {
        mycode_desktop::instance::Claim::Run(lock) => lock,
        mycode_desktop::instance::Claim::Running => {
            eprintln!(
                "mycode: MYCode is already running for {}",
                home.root().display()
            );
            std::process::exit(0);
        }
    };
    let mycode_frontend::launch::Launch { home, update } =
        mycode_frontend::launch::settle(home, true);
    // A pending payload a previous session staged but never swapped — a
    // crash or a lost helper — re-arms here and this launch exits into it:
    // the process that comes back up is the new version. A payload that
    // already landed is reported once, so the swap is never silent.
    let applied_update = match update {
        mycode_frontend::PendingUpdate::Armed(_) => return,
        mycode_frontend::PendingUpdate::Applied(version) => Some(version),
        mycode_frontend::PendingUpdate::None => None,
    };
    // Relocate a downloaded binary into the install dir: the installed copy
    // is spawned with the source path, this process exits, and the new copy
    // deletes the download and writes the desktop shortcut.
    match mycode_frontend::install::ensure_desktop_installed(&home) {
        mycode_frontend::install::DesktopInstall::Exit => std::process::exit(0),
        mycode_frontend::install::DesktopInstall::Run {
            cleanup: Some(source),
        } => {
            std::thread::spawn(move || mycode_frontend::install::cleanup_installed_source(source));
        }
        mycode_frontend::install::DesktopInstall::Run { cleanup: None } => {}
    }
    gpui_kit::application()
        .with_assets(BrandAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            workspace::open_window(home, applied_update, cx);
        });
}
