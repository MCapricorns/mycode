//! MYCode terminal frontend (`mycode-cli`).
//!
//! Same shared backend and product layer as the desktop: `FrontendDriver`
//! owns the bridge, reducer, and persistence; this binary only renders
//! `driver.vm()` and feeds terminal input back in.

mod app;
mod reply;

use mycode_config::{HomeEnv, HomeLayout};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The self-update helper entry is shared with every `mycode*` binary.
    if let Some(code) = mycode_frontend::cli::apply_update_from_args(&args) {
        std::process::exit(code);
    }
    if let Some(action) = mycode_frontend::cli::early_action(&args) {
        let text = match action {
            mycode_frontend::cli::EarlyAction::Version => {
                format!("{}\n", mycode_frontend::cli::version_line("mycode-cli"))
            }
            mycode_frontend::cli::EarlyAction::Help => {
                mycode_frontend::cli::help_text("mycode-cli")
            }
            mycode_frontend::cli::EarlyAction::Unknown(flag) => {
                eprintln!(
                    "mycode-cli: unknown option {flag}\n\n{}",
                    mycode_frontend::cli::help_text("mycode-cli")
                );
                std::process::exit(1);
            }
        };
        print!("{text}");
        std::process::exit(0);
    }
    mycode_app::cleanup_stale_stages();
    mycode_app::cleanup_previous_binary();
    let home = match HomeLayout::from_env(HomeEnv::from_process()) {
        Ok(home) => home,
        Err(error) => {
            eprintln!("mycode-cli: home unavailable: {error}");
            std::process::exit(1);
        }
    };
    // A pending payload a previous session staged but never swapped —
    // a crash or a lost helper — re-arms here. The terminal keeps
    // opening (a second window must never bounce); the swap lands when
    // this run exits and the next `mycode-cli` is the new version.
    match mycode_app::reconcile_pending_update(&home, false) {
        mycode_app::PendingUpdate::Armed(version) => {
            println!("mycode-cli: update v{version} is staged — it installs when this run exits.");
        }
        mycode_app::PendingUpdate::Applied(version) => {
            println!("mycode-cli: updated to v{version}.");
        }
        mycode_app::PendingUpdate::None => {}
    }
    let install = mycode_app::install::ensure_cli_installed(&home);
    announce_install(&install);
    std::process::exit(app::run(home, install));
}

/// Tells the user, before the TUI takes the screen, that `mycode-cli` now
/// runs from any new terminal. The first install (PATH just written) waits
/// for Enter, so a console opened by double-clicking the download does not
/// flash past it.
fn announce_install(report: &mycode_app::install::InstallReport) {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};

    if let Some(warning) = &report.warning {
        eprintln!("mycode-cli: install skipped: {warning}");
        return;
    }
    if !report.copied && !report.path_added {
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "MYCode CLI installed: {}", report.target.display());
    if report.path_added {
        let _ = writeln!(
            out,
            "Its folder was added to your PATH. Open a new terminal and run: mycode-cli"
        );
    } else {
        let _ = writeln!(out, "Run it from any terminal with: mycode-cli");
    }
    if report.path_added && std::io::stdin().is_terminal() {
        let _ = write!(out, "Press Enter to continue...");
        let _ = out.flush();
        let _ = std::io::stdin().lock().read_line(&mut String::new());
    }
}
