//! The terminal frontend: one shared [`FrontendDriver`], a borderless
//! scrolling transcript, a single bordered composer, and a one-line status
//! bar. The Grok-style layout keeps the transcript frameless — the terminal
//! is already the container — and every picklist or card is a transient
//! overlay above the composer.
//!
//! `/settings` swaps the transcript for a settings page: providers, model
//! and thinking pickers, appearance, toggles for MCP / web / skills, and
//! the update row. Settings edits ride the same `FrontendAction`s and
//! debounced save pipeline the desktop uses; the TUI only owns cursor and
//! overlay state.
//!
//! Key map: `Enter` submits (or accepts the highlighted `/`/`@` row —
//! a partial command like `/se` resolves to `/settings`) and queues while a
//! turn runs, `Ctrl+Enter` stops the turn and sends now, `Ctrl+G` steers the
//! running turn, `Shift+Enter`/`Alt+Enter`/`Ctrl+J` inserts a newline,
//! `Ctrl+V` attaches the clipboard picture, `Ctrl+X` drops the waiting
//! attachments, `Up`/`Down` move through an open `/`/`@` menu or the settings
//! list, `Esc` dismisses the topmost layer, closes the settings page, or
//! cancels a running turn, `Ctrl+C` cancels the turn and quits when idle,
//! `Ctrl+N` starts a new task (the empty session keeps the welcome greeting),
//! `Ctrl+O` folds or expands every thinking trace, `PageUp`/`PageDown` scroll.
//! The settings page lists them from the shared table. A terminal that cannot
//! report `Ctrl+Enter` (common outside Windows) sends `Enter` or `Ctrl+J`
//! instead.
//!
//! The terminal's working directory is the project: once the remembered UI
//! state has loaded, it replaces the folder a previous session left behind,
//! so tools never run in a directory other than the one `mycode-cli`
//! started in.
//!
//! This module owns the driver loop, terminal guard, and shared TUI state.
//! Input and submit handling live in [`input`], the settings page and its
//! pickers in [`settings`] and [`settings_rows`], frame painting in [`ui`],
//! and transcript line construction in [`render`].

mod attachments;
mod clipboard;
mod input;
mod render;
mod settings;
mod settings_rows;
pub(crate) mod theme;
mod ui;

#[cfg(test)]
mod tests;

use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use mycode_app::install::InstallReport;
use mycode_frontend::driver::{Effect, FrontendDriver, ToastKind};
use mycode_frontend::view_model::{FrontendAction, MainView, SettingsSection};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Terminal;
use ratatui::style::Color;
use tui_textarea::TextArea;

use render::new_composer;
use settings::SettingsOverlay;

/// Wake channel payload: the driver thread signals "inbox has work".
type WakeRx = mpsc::Receiver<()>;

/// Info and warning notices disappear after this reading time; errors stay
/// until `Esc` dismisses them.
const NOTICE_TTL: Duration = Duration::from_secs(6);
/// Rows of an open `/`/`@` menu painted above the composer.
const MENU_ROWS: usize = 8;
/// Ferris orange for the crab greeting and thinking text.
const CRAB: Color = Color::Rgb(0xf0, 0x74, 0x20);
/// Rows of the settings picker overlay.
const PICKER_ROWS: usize = 10;

/// One transient notice above the status bar.
struct Notice {
    kind: ToastKind,
    text: String,
    shown: Instant,
}

/// Terminal-side state the shared layer deliberately does not own:
/// keystroke-level text editing, scroll offset, painted notices, and the
/// settings page's cursor/overlay.
struct Tui {
    driver: FrontendDriver,
    composer: TextArea<'static>,
    /// Pictures and files waiting to ride the next send. `Ctrl+V` adds the
    /// clipboard picture, `Ctrl+X` drops them.
    attachments: mycode_core::Attachments,
    /// Ink for the chosen appearance, refreshed from settings every frame so
    /// the Theme row takes effect the moment it changes.
    palette: theme::Palette,
    /// Rows of transcript hidden below the viewport bottom; `0` pins to tail.
    scroll: usize,
    /// Something on screen may have changed since the last frame. Painting
    /// rebuilds the whole transcript, so an idle terminal skips it.
    dirty: bool,
    /// The last frame painted the transcript's first resident line while
    /// scrolled up: the reader reached the top of what is loaded.
    at_top: bool,
    notices: Vec<Notice>,
    /// Highlighted row of the open `/`/`@` menu; `None` follows the shared
    /// preferred row. Reset whenever the draft changes.
    mention_selected: Option<usize>,
    /// The working directory, applied as the project once the remembered
    /// UI state has loaded. `None` after that (or when it is unreadable).
    startup_dir: Option<String>,
    /// Cursor over the settings page's item rows (headers are skipped).
    settings_cursor: usize,
    /// The floating picker on the settings page, if open.
    settings_overlay: Option<SettingsOverlay>,
    /// The view last painted — a change into Settings resets the cursor.
    was_settings: bool,
    /// Last frame time while a turn streams: the status line's elapsed
    /// seconds tick once a second without repainting at full poll speed.
    stream_paint: Instant,
    quit: bool,
}

/// Installs the terminal, runs the event loop, restores the terminal.
/// Returns the process exit code.
pub fn run(home: mycode_config::HomeLayout, install: InstallReport) -> i32 {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let mut driver = FrontendDriver::start(home, move || {
        let _ = wake_tx.send(());
    });
    let mut effects = Vec::new();
    driver.bootstrap(&mut effects);
    driver.ui_ready();

    let mut terminal = match TerminalGuard::enter() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("mycode-cli: cannot start the terminal UI: {error}");
            return 1;
        }
    };
    let mut app = Tui::new(driver);
    app.note_install(&install);
    app.apply_effects(std::mem::take(&mut effects));
    app.event_loop(&mut terminal.0, &wake_rx)
}

/// Restores cooked mode and the normal screen on drop, including panic paths.
struct TerminalGuard(Terminal<CrosstermBackend<Stdout>>);

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        // A panic message printed while the alternate screen is up vanishes
        // when the guard restores the screen during unwinding. Restore first,
        // then let the previous hook print where the user can read it.
        // Only the UI thread's panic ends the frontend; a background reply
        // thread dying must not drop the live screen out of raw mode.
        let ui_thread = std::thread::current().id();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == ui_thread {
                restore_terminal(&mut io::stdout());
            }
            previous(info);
        }));
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            SetTitle(format!("MYCode Cli {}", env!("CARGO_PKG_VERSION")))
        )?;
        // Best effort: where the terminal reports pastes as one event, a
        // multi-line paste cannot submit line by line. Consoles that type a
        // paste out as keys are covered by the burst check in `on_key`.
        let _ = execute!(stdout, EnableBracketedPaste);
        let backend = CrosstermBackend::new(stdout);
        Ok(Self(Terminal::new(backend)?))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal(self.0.backend_mut());
    }
}

/// Cooked mode, no bracketed paste, normal screen. Idempotent, so the panic
/// hook and the guard's drop may both run it.
fn restore_terminal(out: &mut impl io::Write) {
    let _ = disable_raw_mode();
    let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen);
}

impl Tui {
    fn new(driver: FrontendDriver) -> Self {
        Self {
            driver,
            composer: new_composer(TextArea::default()),
            attachments: mycode_core::Attachments::default(),
            palette: theme::Palette::DARK,
            scroll: 0,
            dirty: true,
            at_top: false,
            notices: Vec::new(),
            mention_selected: None,
            startup_dir: std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned()),
            settings_cursor: 0,
            settings_overlay: None,
            was_settings: false,
            stream_paint: Instant::now(),
            quit: false,
        }
    }

    /// Surfaces the self-install outcome as a one-shot notice.
    fn note_install(&mut self, report: &InstallReport) {
        if let Some(warning) = &report.warning {
            self.push_notice(ToastKind::Warning, format!("install: {warning}"));
        } else if report.path_added {
            self.push_notice(
                ToastKind::Info,
                format!(
                    "installed to {} and added to PATH — open a new terminal and run mycode-cli",
                    report.target.display()
                ),
            );
        } else if report.copied {
            self.push_notice(
                ToastKind::Info,
                format!("installed to {}", report.target.display()),
            );
        }
    }

    /// Poll → effects → paint → keys, until the driver or the user quits.
    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        wake: &WakeRx,
    ) -> i32 {
        let mut effects = Vec::new();
        loop {
            // Drain driver work deposited since the last frame: replies and
            // timers through `poll`, streamed core events through
            // `next_event` — a frontend that skips either never sees the
            // turn finish.
            while wake.try_recv().is_ok() {
                self.dirty = true;
            }
            // A reader who paged up keeps their place while the stream grows.
            self.driver.set_conversation_at_tail(self.scroll == 0);
            self.driver.poll(&mut effects);
            while let Some(event) = self.driver.next_event() {
                self.driver.on_event(event, &mut effects);
            }
            // The status line's ctx% meter and model label read the catalog
            // on every chat paint, so it is a surface that needs the load.
            self.driver.ensure_catalog();
            self.adopt_startup_dir(&mut effects);
            if !effects.is_empty() {
                self.dirty = true;
                self.apply_effects(std::mem::take(&mut effects));
            }
            // Edit-and-resend, skill inserts, and the welcome send leave
            // their draft here rather than in an effect.
            if let Some(text) = self.driver.take_composer_prefill() {
                self.dirty = true;
                self.set_composer_text(&text);
            }
            // Entering Settings starts the cursor at the first row and drops
            // a picker left open by whatever closed the page before.
            let in_settings = self.driver.vm().view == MainView::Settings;
            if in_settings != self.was_settings {
                self.dirty = true;
                if in_settings {
                    self.settings_cursor = 0;
                    self.settings_overlay = None;
                    // The model picker and update row read the catalog.
                    self.driver.act(
                        FrontendAction::ShowSettingsSection(SettingsSection::Models),
                        &mut effects,
                    );
                    self.driver.ensure_catalog();
                    self.apply_effects(std::mem::take(&mut effects));
                }
            }
            self.was_settings = in_settings;
            self.expire_notices();
            // A live turn repaints once a second so the elapsed label in
            // the status line ticks between deltas.
            if self.driver.vm().sending && self.stream_paint.elapsed() >= Duration::from_secs(1) {
                self.dirty = true;
            }
            if self.dirty {
                self.dirty = false;
                if terminal.draw(|frame| self.draw(frame)).is_err() {
                    return 1;
                }
                self.stream_paint = Instant::now();
            }
            // Paging up past the resident window pulls the next older page;
            // the tail-relative offset keeps the reader on the same rows.
            if self.at_top && self.scroll > 0 {
                self.driver.request_older(&mut effects);
            }
            if self.quit {
                self.driver.shutdown();
                return 0;
            }
            match event::poll(Duration::from_millis(60)) {
                Ok(true) => {
                    // Keys, pastes, resizes, and focus changes all repaint.
                    self.dirty = true;
                    match event::read() {
                        Ok(Event::Key(key)) => self.on_key(key),
                        Ok(Event::Paste(text)) => self.on_paste(&text),
                        Ok(_) => {}
                        Err(_) => return 1,
                    }
                }
                Ok(false) => {}
                Err(_) => return 1,
            }
        }
    }

    /// Makes the working directory the project, once, after the remembered
    /// UI state can no longer overwrite it.
    fn adopt_startup_dir(&mut self, effects: &mut Vec<Effect>) {
        if !self.driver.ui_state_loaded() {
            return;
        }
        let Some(dir) = self.startup_dir.take() else {
            return;
        };
        self.driver
            .act(FrontendAction::ProjectOpened(dir.clone()), effects);
        self.driver
            .act(FrontendAction::WorkspaceRootAdded(dir), effects);
    }

    /// Maps shared effects onto terminal-side state.
    fn apply_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Toast { kind, text } => self.push_notice(kind, text),
                Effect::ComposerSet { text, focus: _ } => self.set_composer_text(&text),
                Effect::AttachmentsRestored(attachments) => self.restore_attachments(attachments),
                // Focus, window chrome, and paging anchors are GPUI
                // concerns; a terminal frame is always focused.
                Effect::Quit => self.quit = true,
                Effect::ScrollToTail => self.scroll = 0,
                // The offset counts rows from the tail, so a prepended page
                // needs no anchor; release the hold so paging can continue.
                Effect::OlderPagePrepended { .. } => self.driver.note_scroll_hold(false),
                _ => {}
            }
        }
    }

    fn push_notice(&mut self, kind: ToastKind, text: String) {
        self.notices.push(Notice {
            kind,
            text,
            shown: Instant::now(),
        });
    }

    /// Drops info and warning notices past their reading time. Runs every
    /// frame, not only when new effects arrive.
    fn expire_notices(&mut self) {
        let before = self.notices.len();
        self.notices.retain(|notice| {
            notice.kind == ToastKind::Error || notice.shown.elapsed() < NOTICE_TTL
        });
        if self.notices.len() != before {
            self.dirty = true;
        }
    }
}
