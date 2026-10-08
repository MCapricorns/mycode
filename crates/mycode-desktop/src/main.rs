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
