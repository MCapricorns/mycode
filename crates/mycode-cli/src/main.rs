//! MYCode terminal frontend (`mycode`).
//!
//! Same shared backend and product layer as the desktop: `FrontendDriver`
//! owns the bridge, reducer, and persistence; this binary only renders
//! `driver.vm()` and feeds terminal input back in.

mod app;

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
                format!("{}\n", mycode_frontend::cli::version_line())
            }
            mycode_frontend::cli::EarlyAction::Help => {
                mycode_frontend::cli::help_text("mycode")
            }
            mycode_frontend::cli::EarlyAction::Unknown(flag) => {
                eprintln!(
                    "mycode: unknown option {flag}\n\n{}",
                    mycode_frontend::cli::help_text("mycode")
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
            eprintln!("mycode home unavailable: {error}");
            std::process::exit(1);
        }
    };
    std::process::exit(app::run(home));
}
