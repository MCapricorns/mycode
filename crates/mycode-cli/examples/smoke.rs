//! Headless smoke test against the real backend and the user's real config.
//!
//! Run: `cargo run -p mycode-cli --example smoke`
//!
//! Drives `FrontendDriver` exactly like the TUI would, then prints a
//! step-by-step verdict for each scenario: tool use (pelican.svg), web
//! search, the context7 MCP server, a subagent, a mid-stream interrupt plus
//! a continue that must see the earlier work, and manual compaction.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use mycode_config::{HomeEnv, HomeLayout};
use mycode_frontend::driver::{Effect, FrontendDriver, ToastKind};

fn main() {
    let home = HomeLayout::from_env(HomeEnv::from_process()).expect("home");
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let mut driver = FrontendDriver::start(home, move || {
        let _ = wake_tx.send(());
    });
    let mut effects = Vec::new();
    driver.bootstrap(&mut effects);
    driver.ui_ready();

    // Boot: wait until settings and the session list have landed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut rendered = 0usize;
    loop {
        pump(&mut driver, &wake_rx, &mut effects, &mut rendered);
        if driver.vm().settings.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "settings never arrived");
        std::thread::sleep(Duration::from_millis(50));
    }
    let vm = driver.vm();
    println!(
        "== boot: model={:?} provider={:?} sessions={} project={:?}",
        vm.selected_model, vm.selected_provider, vm.sessions.len(), vm.project_dir
    );

    // A provider can be overridden for the smoke run: `smoke.exe <provider> <model>`.
    let mut extra: Vec<String> = std::env::args().skip(1).collect();
    if extra.len() >= 2 {
        let model = extra.pop().expect("model");
        let provider = extra.pop().expect("provider");
        println!("== switching to {provider} / {model}");
        let mut fx = Vec::new();
        driver.act(
            mycode_frontend::view_model::FrontendAction::ProviderSelected(provider),
            &mut fx,
        );
        driver.act(
            mycode_frontend::view_model::FrontendAction::ModelSelected(model),
            &mut fx,
        );
        driver.save_settings(&mut fx);
        driver.persist_ui_state();
        apply(&mut driver, fx);
        println!(
            "   now: model={:?} provider={:?}",
            driver.vm().selected_model,
            driver.vm().selected_provider
        );
    }

    // Fresh session: keeps the smoke run independent of old ledger tails.
    {
        let mut fx = Vec::new();
        driver.new_session(&mut fx);
        apply(&mut driver, fx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while driver.vm().active.is_none() {
            pump(&mut driver, &wake_rx, &mut Vec::new(), &mut rendered);
            assert!(Instant::now() < deadline, "new session never opened");
            std::thread::sleep(Duration::from_millis(50));
        }
        rendered = 0;
        println!("== fresh session {:?}", driver.open_session_id());
    }

    // A1 file tools.
    let entries_before = transcript_len(&driver);
    turn(
        &mut driver,
        &wake_rx,
        &mut rendered,
        "A1 hello-world.html via write tool",
        "Use the write tool to create hello-world.html containing a minimal \
         hello world page. Then reply with just DONE.",
        Duration::from_secs(120),
    );
    verdict("A1 tool call ran", saw_tool(&driver, entries_before, ""));

    // A2 web search (anysearch backend is enabled in settings).
    let entries_before = transcript_len(&driver);
    turn(
        &mut driver,
        &wake_rx,
        &mut rendered,
        "A2 web_search",
        "Use the web_search tool to look up what 'zenmux' is. \
         Reply with one line on what it is, or NO-TOOL if no such tool exists.",
        Duration::from_secs(120),
    );
    verdict("A2 web tool ran", saw_tool(&driver, entries_before, "web"));

    // A3 MCP: context7 is configured and enabled.
    let entries_before = transcript_len(&driver);
    turn(
        &mut driver,
        &wake_rx,
        &mut rendered,
        "A3 context7 MCP",
        "Use the context7 MCP server tools to look up the Win32 MessageBox \
         API. Reply with one fact about it, or NO-TOOL if unreachable.",
        Duration::from_secs(180),
    );
    // Context7 calls surface under the server's own tool names
    // (resolve-library-id / query-docs), not a "context7" prefix.
    verdict(
        "A3 mcp/context7 ran",
        saw_tool(&driver, entries_before, "resolve-library")
            || saw_tool(&driver, entries_before, "query-docs")
            || saw_tool(&driver, entries_before, "context7")
            || saw_tool(&driver, entries_before, "mcp"),
    );

    // A4 subagent.
    let entries_before = transcript_len(&driver);
    turn(
        &mut driver,
        &wake_rx,
        &mut rendered,
        "A4 subagent",
        "Use the agent tool to delegate this to a subagent: count the .rs \
         files under crates/ in this project and report the number. Reply \
         with just the count, or NO-TOOL if no agent tool exists.",
        Duration::from_secs(300),
    );
    verdict(
        "A4 subagent ran",
        saw_tool(&driver, entries_before, "agent"),
    );

    interrupt(&mut driver, &wake_rx, &mut rendered);

    turn(
        &mut driver,
        &wake_rx,
        &mut rendered,
        "A6 continue after interrupt",
        "In one short line: what were you doing before you were stopped? \
         Do not write anything else.",
        Duration::from_secs(90),
    );

    compact(&mut driver, &wake_rx, &mut rendered);

    turn(
        &mut driver,
        &wake_rx,
        &mut rendered,
        "A8 post-compact context",
        "One line: what did we do in this session before the compaction? \
         Mention 'pelican' if you drew one.",
        Duration::from_secs(90),
    );

    println!("== done ==");
}

fn transcript_len(driver: &FrontendDriver) -> usize {
    driver
        .vm()
        .active
        .as_ref()
        .map(|a| a.entries.len())
        .unwrap_or(0)
}

/// Whether a committed tool call between `since` and now mentions `needle`.
fn saw_tool(driver: &FrontendDriver, since: usize, needle: &str) -> bool {
    let Some(active) = driver.vm().active.as_ref() else {
        return false;
    };
    active.entries[since.min(active.entries.len())..]
        .iter()
        .any(|entry| {
            entry.kind == mycode_app::EntryKind::ToolCall
                && (needle.is_empty()
                    || entry.text.to_lowercase().contains(needle))
        })
}

fn verdict(label: &str, ok: bool) {
    println!("   {} {}", if ok { "PASS" } else { "FAIL" }, label);
}

/// Sends `prompt`, waits for the turn to end, prints new transcript entries.
fn turn(
    driver: &mut FrontendDriver,
    wake: &mpsc::Receiver<()>,
    rendered: &mut usize,
    label: &str,
    prompt: &str,
    budget: Duration,
) {
    println!("== {label}: {prompt}");
    let mut effects = Vec::new();
    driver.submit(prompt, &mut effects);
    apply(&mut *driver, effects);
    wait_turn(driver, wake, rendered, budget, label);
}

/// Interrupts a long turn mid-stream, then verifies the alert and ledger.
fn interrupt(driver: &mut FrontendDriver, wake: &mpsc::Receiver<()>, rendered: &mut usize) {
    println!("== A5 interrupt mid-turn");
    let mut effects = Vec::new();
    driver.submit(
        "Write a 2000 word essay on the history of terminal emulators. Long prose, no tools.",
        &mut effects,
    );
    apply(&mut *driver, effects);
    // Give the model a moment to actually start streaming.
    let started = Instant::now();
    loop {
        pump(driver, wake, &mut Vec::new(), rendered);
        let streaming = driver
            .vm()
            .active
            .as_ref()
            .and_then(|a| a.streaming.as_ref())
            .map(|s| s.text.len())
            .unwrap_or(0);
        if streaming > 40 || !driver.vm().sending || started.elapsed() > Duration::from_secs(20) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut effects = Vec::new();
    driver.cancel_chat(&mut effects);
    apply(&mut *driver, effects);
    wait_turn(driver, wake, rendered, Duration::from_secs(30), "A5 cancel");
    let alert = driver.vm().turn_alert.clone();
    println!("   turn_alert = {alert:?}");
    verdict("A5 interrupt alert", alert.is_some());
}

/// Manual compaction, then wait for the card to land.
fn compact(driver: &mut FrontendDriver, wake: &mpsc::Receiver<()>, rendered: &mut usize) {
    println!("== A7 /compact");
    let before = transcript_len(driver);
    let mut effects = Vec::new();
    driver.compact(&mut effects);
    apply(&mut *driver, effects);
    wait_turn(driver, wake, rendered, Duration::from_secs(120), "compact");
    let after = transcript_len(driver);
    println!("   entries {before} -> {after}");
}

/// Drains the driver until `sending` drops, printing entries as they land.
fn wait_turn(
    driver: &mut FrontendDriver,
    wake: &mpsc::Receiver<()>,
    rendered: &mut usize,
    budget: Duration,
    label: &str,
) {
    let deadline = Instant::now() + budget;
    // A submitted turn may queue behind nothing yet — wait one beat for
    // `sending` to rise before watching it fall.
    std::thread::sleep(Duration::from_millis(300));
    let mut beat = Instant::now();
    loop {
        pump(driver, wake, &mut Vec::new(), rendered);
        let vm = driver.vm();
        if !vm.sending && !vm.compacting {
            if let Some(error) = &vm.error {
                println!("   [{label}] error: {error}");
            }
            break;
        }
        assert!(Instant::now() < deadline, "{label}: turn never finished");
        if beat.elapsed() > Duration::from_secs(10) {
            beat = Instant::now();
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            println!("   [{label}] still running, {left}s left");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    pump(driver, wake, &mut Vec::new(), rendered);
}

/// Polls the driver, applies effects, prints new transcript entries.
fn pump(
    driver: &mut FrontendDriver,
    wake: &mpsc::Receiver<()>,
    effects: &mut Vec<Effect>,
    rendered: &mut usize,
) {
    while wake.try_recv().is_ok() {}
    driver.poll(effects);
    while let Some(event) = driver.next_event() {
        driver.on_event(event, effects);
    }
    if !effects.is_empty() {
        apply(driver, std::mem::take(effects));
    }
    let Some(active) = driver.vm().active.as_ref() else {
        return;
    };
    while *rendered < active.entries.len() {
        let entry = &active.entries[*rendered];
        let kind = match entry.kind {
            mycode_app::EntryKind::UserMessage => "USER",
            mycode_app::EntryKind::AssistantMessage => "ASST",
            mycode_app::EntryKind::ToolCall => "TOOL",
            mycode_app::EntryKind::ToolResult => "RES ",
        };
        let text = entry.text.lines().next().unwrap_or("");
        let text: String = text.chars().take(110).collect();
        println!("   [{kind}] {text}");
        *rendered += 1;
    }
    if let Some(stream) = &active.streaming
        && !stream.status.is_empty()
    {
        let status = stream.status.lines().next().unwrap_or("");
        let changed = LAST_STATUS
            .with(|cell| cell.replace(Some(status.to_owned())))
            .as_deref()
            != Some(status);
        if changed {
            println!("   [....] {status}");
        }
    }
}

thread_local! {
    static LAST_STATUS: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn apply(_driver: &mut FrontendDriver, effects: Vec<Effect>) {
    for effect in &effects {
        if let Effect::Toast { kind, text } = effect {
            let mark = match kind {
                ToastKind::Error => "ERR",
                ToastKind::Warning => "WARN",
                ToastKind::Info => "info",
            };
            println!("   [toast:{mark}] {text}");
        }
        if let Effect::Quit = effect {
            println!("   [effect] Quit");
        }
    }
}
