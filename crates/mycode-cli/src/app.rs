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
//! a partial command like `/se` resolves to `/settings`),
//! `Alt+Enter`/`Ctrl+J` inserts a newline, `Up`/`Down` move through an open
//! `/`/`@` menu or the settings list, `Esc` dismisses the topmost layer,
//! closes the settings page, or cancels a running turn, `Ctrl+C` cancels
//! the turn and quits when idle, `Ctrl+N` starts a new task, `Ctrl+O`
//! folds or expands every thinking trace,
//! `PageUp`/`PageDown` scroll.
//!
//! The terminal's working directory is the project: once the remembered UI
//! state has loaded, it replaces the folder a previous session left behind,
//! so tools never run in a directory other than the one `mycode-cli`
//! started in.

use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use mycode_app::install::InstallReport;
use mycode_app::{ConversationEntry, EntryKind};
use mycode_config::{VALID_FONT_SIZES, VALID_LANGUAGES, VALID_THEMES};
use mycode_frontend::driver::{Effect, FrontendDriver, ToastKind};
use mycode_frontend::picker;
use mycode_frontend::view_model::{
    FrontendAction, MainView, MentionKind, ProxyTestState, SettingsSection, UpdateState,
    WorkspaceState, elapsed_label, model_display_name, preferred_slash_index,
    provider_display_name, selected_reasoning_level, selected_reasoning_levels,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::{Frame, Terminal};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tui_textarea::{CursorMove, Input, Key as TextareaKey, TextArea};

use crate::reply::push_reply;

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

/// A picker floating over the settings page.
enum SettingsOverlay {
    /// Two-step provider → model pick. `provider=None` is the provider
    /// step; `Some(id)` lists that provider's models.
    Model {
        provider: Option<String>,
        cursor: usize,
        filter: String,
    },
    /// Single-line input for one provider's API key (masked).
    ApiKey { provider: String, buffer: String },
    /// Single-line input for one provider's base URL.
    BaseUrl { index: usize, buffer: String },
    /// Single-line input for the manual proxy URL.
    ProxyUrl { buffer: String },
}

/// One selectable row on the settings page.
#[derive(Clone)]
enum SettingItem {
    /// Opens the provider → model picker.
    Model,
    /// Cycles the session's reasoning effort.
    Thinking,
    /// Folds every thinking trace to one line, or shows the full text.
    ThinkingTrace,
    /// Cycles `dark`/`light`.
    Theme,
    /// Cycles `auto`/`en`/`zh`.
    Language,
    /// Cycles `s`/`m`/`l`/`xl`.
    FontSize,
    /// Enter edits the manual proxy URL; Left/Right switch between
    /// following the system and the manual URL.
    Proxy,
    /// Fetches google.com through the proxy setting and shows the result.
    ProxyTest,
    /// Toggles automatic update checks.
    AutoUpdate,
    /// Checks, downloads, or installs an update depending on state.
    Update,
    /// Provider row index; Enter toggles, `a` edits the API key, `u` the
    /// base URL.
    Provider(usize),
    /// MCP server row index; Enter toggles (enable runs the probe), `a`
    /// edits its `mcp-<id>` key.
    Mcp(usize),
    /// Web backend row index; Enter toggles (enable runs the probe), `a`
    /// edits its `web-<id>` key.
    Web(usize),
    /// Skill row index into `vm.skills`; Enter toggles it.
    Skill(usize),
}

/// A settings line: a dim section header or a selectable item row.
enum SettingRow {
    Header(String),
    Item {
        item: SettingItem,
        label: String,
        value: String,
        dim: bool,
    },
}

/// Terminal-side state the shared layer deliberately does not own:
/// keystroke-level text editing, scroll offset, painted notices, and the
/// settings page's cursor/overlay.
struct Tui {
    driver: FrontendDriver,
    composer: TextArea<'static>,
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

    /// A bracketed paste lands as text, newlines included, never as Enter.
    fn on_paste(&mut self, text: &str) {
        // On the settings page a paste belongs to the open single-line
        // input (an API key is nearly always pasted), never to the hidden
        // composer.
        if self.driver.vm().view == MainView::Settings {
            let line: String = text.chars().filter(|ch| !ch.is_control()).collect();
            match &mut self.settings_overlay {
                Some(
                    SettingsOverlay::ApiKey { buffer, .. }
                    | SettingsOverlay::BaseUrl { buffer, .. }
                    | SettingsOverlay::ProxyUrl { buffer },
                ) => buffer.push_str(line.trim()),
                Some(SettingsOverlay::Model { filter, cursor, .. }) => {
                    filter.push_str(&line);
                    *cursor = 0;
                }
                None => {}
            }
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.composer.insert_str(text);
        let mut effects = Vec::new();
        self.sync_draft(&mut effects);
        self.apply_effects(effects);
    }

    /// Mirrors the textarea into the shared draft so `/` and `@` menus and
    /// the queue see what is on screen.
    fn sync_draft(&mut self, effects: &mut Vec<Effect>) {
        let draft = self.composer.lines().join("\n");
        if draft != self.driver.vm().composer_draft {
            self.mention_selected = None;
            self.driver
                .act(FrontendAction::ComposerChanged(draft), effects);
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.driver.set_conversation_at_tail(self.scroll == 0);
        let mut effects = Vec::new();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // On the settings page every key routes to the list/pickers; an open
        // ask card (a running turn's question) still wins Esc and Enter.
        if self.driver.vm().view == MainView::Settings && self.driver.vm().pending_ask.is_none() {
            self.on_settings_key(key, ctrl, alt, &mut effects);
            self.apply_effects(effects);
            return;
        }
        match (key.code, ctrl, alt) {
            (KeyCode::Char('c'), true, false) => self.interrupt_or_quit(&mut effects),
            // Innermost first: an ask card, then an open `/`/`@` menu, and
            // only then the running turn, so closing a menu mid-turn never
            // stops the agent.
            (KeyCode::Esc, false, false) => {
                if self.driver.vm().pending_ask.is_some() {
                    self.driver.dismiss_ask(&mut effects);
                } else if self.driver.dismiss_floating() {
                    self.mention_selected = None;
                } else if self.turn_busy() {
                    self.driver.cancel_chat(&mut effects);
                } else if !self.notices.is_empty() {
                    // Errors never expire on their own.
                    self.notices.clear();
                } else {
                    // Nothing floating: reset scroll to the tail.
                    self.scroll = 0;
                }
            }
            // A console that types a paste out as keys delivers its lines as
            // Enter presses with more input already waiting; those become
            // newlines instead of one submit per pasted line.
            (KeyCode::Enter, false, false) if !matches!(event::poll(Duration::ZERO), Ok(true)) => {
                self.submit_current(&mut effects);
            }
            // Newline: terminals that report Alt/Ctrl+Enter land here; the
            // rest use Ctrl+J, which crossterm maps from LF.
            (KeyCode::Enter, _, _) | (KeyCode::Char('j'), true, false) => {
                self.composer.input(Input {
                    key: TextareaKey::Enter,
                    ..Input::default()
                });
                self.sync_draft(&mut effects);
            }
            (KeyCode::Up | KeyCode::Down, false, false) if self.menu_len() > 0 => {
                let len = self.menu_len();
                let current = self.menu_highlight();
                self.mention_selected = Some(if key.code == KeyCode::Down {
                    (current + 1) % len
                } else {
                    (current + len - 1) % len
                });
            }
            (KeyCode::PageUp, _, _) => self.scroll = self.scroll.saturating_add(10),
            (KeyCode::PageDown, _, _) => self.scroll = self.scroll.saturating_sub(10),
            (KeyCode::Char('n'), true, false) => {
                self.driver.new_session(&mut effects);
            }
            (KeyCode::Char('o'), true, false) => {
                self.driver.toggle_thinking_collapsed(&mut effects);
            }
            _ => {
                self.composer.input(Input::from(key));
                self.sync_draft(&mut effects);
            }
        }
        self.apply_effects(effects);
    }

    /// Rows in the open `/`/`@` menu, `0` when none is open.
    fn menu_len(&self) -> usize {
        self.driver
            .vm()
            .mention
            .as_ref()
            .map_or(0, |mention| mention.items.len())
    }

    /// The row Enter accepts: the keyboard pick, else the shared preferred
    /// row (the exact slash command typed so far).
    fn menu_highlight(&self) -> usize {
        let Some(mention) = &self.driver.vm().mention else {
            return 0;
        };
        let preferred = if mention.kind == MentionKind::Command {
            preferred_slash_index(&mention.fragment, &mention.items)
        } else {
            0
        };
        self.mention_selected
            .unwrap_or(preferred)
            .min(mention.items.len().saturating_sub(1))
    }

    /// Enter: an ask card consumes the text as the row's answer, an open
    /// `/`/`@` menu accepts its highlighted row, otherwise the draft goes
    /// through the shared submit path (welcome, queue, send).
    ///
    /// The composer is cleared by the driver's own `ComposerSet` effects, so
    /// a draft the driver refuses (no folder yet, a full queue) stays put.
    fn submit_current(&mut self, effects: &mut Vec<Effect>) {
        let text = self.composer.lines().join("\n");
        if let Some(asks) = &self.driver.vm().pending_ask {
            let index = self.driver.ask_index().min(asks.len().saturating_sub(1));
            // Choice numbers pick those choices (several, `1,3`, on a
            // multi-select row); anything else is free text.
            let picks = asks.get(index).and_then(|row| {
                choice_numbers(&text, row.choices.len())
                    .filter(|numbers| row.multiple || numbers.len() == 1)
                    .map(|numbers| {
                        numbers
                            .into_iter()
                            .map(|number| row.choices[number - 1].clone())
                            .collect::<Vec<_>>()
                    })
            });
            match picks {
                Some(answers) => {
                    for answer in answers {
                        self.driver.pick_ask_choice(index, answer, false, effects);
                    }
                }
                None => self.driver.ask_text_typed(index, text, effects),
            }
            self.driver.ask_next(effects);
            self.clear_composer();
            return;
        }
        if self.menu_len() > 0 {
            let index = self.menu_highlight();
            self.mention_selected = None;
            self.driver.accept_mention(index, effects);
            return;
        }
        self.driver.submit(&text, effects);
        self.scroll = 0;
    }

    /// Rebuilds the textarea so cursor, undo, and scroll all reset; the
    /// cursor lands at the end of the text.
    fn set_composer_text(&mut self, text: &str) {
        let mut composer = if text.is_empty() {
            TextArea::default()
        } else {
            // `split('\n')` keeps a trailing empty line, unlike `lines()`.
            TextArea::from(text.split('\n').map(str::to_owned).collect::<Vec<_>>())
        };
        composer.move_cursor(CursorMove::Bottom);
        composer.move_cursor(CursorMove::End);
        self.composer = new_composer(composer);
        self.mention_selected = None;
    }

    fn clear_composer(&mut self) {
        self.set_composer_text("");
    }

    // ---- settings page ----

    /// All key input while `vm.view == Settings`.
    fn on_settings_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        _alt: bool,
        effects: &mut Vec<Effect>,
    ) {
        if self.settings_overlay.is_some() {
            self.on_overlay_key(key, ctrl, effects);
            return;
        }
        let items = settings_items(self.driver.vm());
        match (key.code, ctrl) {
            (KeyCode::Char('c'), true) => self.interrupt_or_quit(effects),
            (KeyCode::Char('o'), true) => self.driver.toggle_thinking_collapsed(effects),
            (KeyCode::Char('q'), false) if !self.turn_busy() => self.quit = true,
            (KeyCode::Esc, false) => {
                self.driver.show_main_view(MainView::Chat, effects);
            }
            (KeyCode::Up, false) => {
                self.settings_cursor = self.settings_cursor.saturating_sub(1);
            }
            (KeyCode::Down, false) => {
                if !items.is_empty() {
                    self.settings_cursor = (self.settings_cursor + 1).min(items.len() - 1);
                }
            }
            (KeyCode::PageUp, _) => {
                self.settings_cursor = self.settings_cursor.saturating_sub(10);
            }
            (KeyCode::PageDown, _) => {
                if !items.is_empty() {
                    self.settings_cursor = (self.settings_cursor + 10).min(items.len() - 1);
                }
            }
            (KeyCode::Home, false) => self.settings_cursor = 0,
            (KeyCode::End, false) => {
                self.settings_cursor = items.len().saturating_sub(1);
            }
            (KeyCode::Enter, false) | (KeyCode::Char(' '), false) => {
                match items.get(self.settings_cursor) {
                    // Enter types the manual URL; committing it switches mode.
                    Some(SettingItem::Proxy) => {
                        let buffer = manual_proxy(self.driver.vm()).unwrap_or_else(|| {
                            mycode_frontend::forms::DEFAULT_PROXY_URL.to_owned()
                        });
                        self.settings_overlay = Some(SettingsOverlay::ProxyUrl { buffer });
                    }
                    Some(item) => {
                        let item = item.clone();
                        self.activate_setting(item, 1, effects);
                    }
                    None => {}
                }
            }
            (KeyCode::Left, false) | (KeyCode::Right, false) => {
                let direction = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(item) = items.get(self.settings_cursor) {
                    let item = item.clone();
                    self.activate_setting(item, direction, effects);
                }
            }
            (KeyCode::Char('a'), false) => {
                if let Some(item) = items.get(self.settings_cursor) {
                    self.open_key_editor(item);
                }
            }
            (KeyCode::Char('u'), false) => {
                if let Some(SettingItem::Provider(index)) = items.get(self.settings_cursor)
                    && let Some(provider) = self
                        .driver
                        .vm()
                        .settings
                        .as_ref()
                        .and_then(|settings| settings.providers.get(*index))
                {
                    self.settings_overlay = Some(SettingsOverlay::BaseUrl {
                        index: *index,
                        buffer: provider.base_url.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// A running turn or compaction.
    fn turn_busy(&self) -> bool {
        self.driver.vm().sending || self.driver.vm().compacting
    }

    /// `Ctrl+C`: cancel a running turn first, quit only when idle.
    fn interrupt_or_quit(&mut self, effects: &mut Vec<Effect>) {
        if self.turn_busy() {
            self.driver.cancel_chat(effects);
        } else {
            self.quit = true;
        }
    }

    /// Key input while a settings overlay (model pick, key input) is open.
    fn on_overlay_key(&mut self, key: KeyEvent, ctrl: bool, effects: &mut Vec<Effect>) {
        if ctrl && key.code == KeyCode::Char('c') {
            self.interrupt_or_quit(effects);
            return;
        }
        // Other control chords (Ctrl+V, Ctrl+A, …) are not text.
        if ctrl && matches!(key.code, KeyCode::Char(_)) {
            return;
        }
        match self.settings_overlay.take() {
            Some(SettingsOverlay::Model {
                provider,
                cursor,
                filter,
            }) => self.on_picker_key(key, provider, cursor, filter, effects),
            Some(SettingsOverlay::ApiKey {
                provider,
                mut buffer,
            }) => match edit_line(&key, &mut buffer) {
                LineKey::Cancel => {}
                LineKey::Submit => self.submit_api_key(provider, &buffer),
                LineKey::Edited => {
                    self.settings_overlay = Some(SettingsOverlay::ApiKey { provider, buffer });
                }
            },
            Some(SettingsOverlay::BaseUrl { index, mut buffer }) => {
                match edit_line(&key, &mut buffer) {
                    LineKey::Cancel => {}
                    LineKey::Submit => self.submit_base_url(index, &buffer, effects),
                    LineKey::Edited => {
                        self.settings_overlay = Some(SettingsOverlay::BaseUrl { index, buffer });
                    }
                }
            }
            Some(SettingsOverlay::ProxyUrl { mut buffer }) => match edit_line(&key, &mut buffer) {
                LineKey::Cancel => {}
                LineKey::Submit if self.submit_proxy_url(&buffer, effects) => {}
                LineKey::Submit | LineKey::Edited => {
                    self.settings_overlay = Some(SettingsOverlay::ProxyUrl { buffer });
                }
            },
            None => {}
        }
    }

    /// The provider → model pick overlay.
    fn on_picker_key(
        &mut self,
        key: KeyEvent,
        provider: Option<String>,
        mut cursor: usize,
        mut filter: String,
        effects: &mut Vec<Effect>,
    ) {
        let rows = self.picker_rows(provider.as_deref(), &filter);
        match key.code {
            KeyCode::Esc => return, // overlay stays closed
            KeyCode::Left => {
                // Back to the provider step.
                if provider.is_some() {
                    self.settings_overlay = Some(SettingsOverlay::Model {
                        provider: None,
                        cursor: 0,
                        filter: String::new(),
                    });
                }
                return;
            }
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => {
                if !rows.is_empty() {
                    cursor = (cursor + 1).min(rows.len() - 1);
                }
            }
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = rows.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(row) = rows.get(cursor) {
                    self.pick_model_row(row.clone(), effects);
                }
                return;
            }
            KeyCode::Backspace => {
                if filter.pop().is_none() && provider.is_some() {
                    // Empty filter + backspace goes back a step.
                    self.settings_overlay = Some(SettingsOverlay::Model {
                        provider: None,
                        cursor: 0,
                        filter: String::new(),
                    });
                    return;
                }
                cursor = 0;
            }
            KeyCode::Char(ch) => {
                filter.push(ch);
                cursor = 0;
            }
            _ => {}
        }
        self.settings_overlay = Some(SettingsOverlay::Model {
            provider,
            cursor,
            filter,
        });
    }

    /// The rows one picker step shows, filtered by the typed query.
    fn picker_rows(&self, provider: Option<&str>, filter: &str) -> Vec<PickerRow> {
        let vm = self.driver.vm();
        if vm.settings.is_none() {
            return Vec::new();
        }
        // The desktop picker's rows: recent pins, then enabled providers;
        // a provider's step lists its catalog and configured models minus
        // the hidden ones.
        let rows = match provider {
            None => picker::recent_pins(vm)
                .into_iter()
                .map(|pin| PickerRow::Pin {
                    label: format!(
                        "{} · {}",
                        model_display_name(vm, &pin.provider, &pin.model),
                        provider_display_name(vm, &pin.provider)
                    ),
                    provider: pin.provider,
                    model: pin.model,
                })
                .chain(
                    picker::enabled_providers(vm)
                        .into_iter()
                        .map(|(id, label)| PickerRow::Provider { id, label }),
                )
                .collect(),
            Some(provider_id) => picker::provider_model_choices(vm, provider_id)
                .into_iter()
                .map(|choice| PickerRow::Model {
                    provider: provider_id.to_owned(),
                    model: choice.id,
                    label: choice.name,
                })
                .collect(),
        };
        filter_picker_rows(rows, filter)
    }

    /// Enter on a picker row: pins and models select, providers descend.
    fn pick_model_row(&mut self, row: PickerRow, effects: &mut Vec<Effect>) {
        match row {
            PickerRow::Provider { id, .. } => {
                self.settings_overlay = Some(SettingsOverlay::Model {
                    provider: Some(id),
                    cursor: 0,
                    filter: String::new(),
                });
            }
            PickerRow::Pin {
                provider, model, ..
            }
            | PickerRow::Model {
                provider, model, ..
            } => {
                if self.driver.vm().selected_provider.as_deref() != Some(provider.as_str()) {
                    self.driver
                        .act(FrontendAction::ProviderSelected(provider), effects);
                }
                self.driver
                    .act(FrontendAction::ModelSelected(model), effects);
                if self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.dirty)
                {
                    self.driver.save_settings(effects);
                }
                self.driver.persist_ui_state();
            }
        }
    }

    /// Enter on the masked API-key input for a provider/MCP/web row.
    fn submit_api_key(&mut self, provider: String, buffer: &str) {
        if !self.driver.save_key(provider, buffer) {
            self.push_notice(
                ToastKind::Error,
                "key refused — empty or looks like a placeholder".to_owned(),
            );
        }
    }

    /// Enter on the base-URL input for a provider row.
    fn submit_base_url(&mut self, index: usize, buffer: &str, effects: &mut Vec<Effect>) {
        let Some(id) = self
            .driver
            .vm()
            .settings
            .as_ref()
            .and_then(|settings| settings.providers.get(index))
            .map(|provider| provider.id.clone())
        else {
            return;
        };
        self.driver.act(
            FrontendAction::SettingsProviderBaseUrlChanged {
                id,
                base_url: buffer.trim().to_owned(),
            },
            effects,
        );
        self.driver.save_settings(effects);
    }

    /// Enter on the manual proxy input. An empty value goes back to the
    /// system proxy; an invalid one keeps the input open. Returns whether
    /// the input should close.
    fn submit_proxy_url(&mut self, buffer: &str, effects: &mut Vec<Effect>) -> bool {
        let value = buffer.trim();
        if value.is_empty() {
            self.driver.select_proxy_mode("system", effects);
            return true;
        }
        if !matches!(
            mycode_config::ProxyPreference::parse(value),
            Some(mycode_config::ProxyPreference::Url(_))
        ) {
            self.push_notice(
                ToastKind::Error,
                "not a proxy URL — http, https, socks5, or socks5h (host:port reads as http)"
                    .to_owned(),
            );
            return false;
        }
        self.driver.proxy_url_typed(value, effects);
        self.driver.proxy_url_committed(effects);
        true
    }

    fn push_notice(&mut self, kind: ToastKind, text: String) {
        self.notices.push(Notice {
            kind,
            text,
            shown: Instant::now(),
        });
    }

    /// Enter / Left / Right on a settings row.
    fn activate_setting(&mut self, item: SettingItem, direction: i8, effects: &mut Vec<Effect>) {
        match item {
            SettingItem::Model => {
                self.settings_overlay = Some(SettingsOverlay::Model {
                    provider: None,
                    cursor: 0,
                    filter: String::new(),
                });
            }
            SettingItem::Thinking => {
                let levels = selected_reasoning_levels(self.driver.vm());
                if levels.is_empty() {
                    return;
                }
                let current = selected_reasoning_level(self.driver.vm());
                let len = levels.len() as i64;
                let next = levels
                    .iter()
                    .position(|level| *level == current)
                    .map(|index| (index as i64 + i64::from(direction)).rem_euclid(len))
                    .unwrap_or(0) as usize;
                self.driver.act(
                    FrontendAction::SettingsReasoningChanged(levels[next].clone()),
                    effects,
                );
                self.driver.persist_ui_state();
            }
            SettingItem::Theme => {
                let current = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .map(|settings| settings.theme.clone())
                    .unwrap_or_default();
                let next = cycle(&VALID_THEMES, &current, direction);
                self.driver.select_theme(&next, effects);
            }
            SettingItem::Language => {
                let current = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .map(|settings| settings.language.clone())
                    .unwrap_or_else(|| "auto".to_owned());
                let next = cycle(&VALID_LANGUAGES, &current, direction);
                self.driver.select_language(&next, effects);
            }
            SettingItem::FontSize => {
                let current = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .map(|settings| settings.font_size.clone())
                    .unwrap_or_else(|| "m".to_owned());
                let next = cycle(&VALID_FONT_SIZES, &current, direction);
                self.driver.select_font_size(&next, effects);
            }
            SettingItem::Proxy => {
                // Left/Right flip system ⇄ manual; manual keeps a stored URL
                // or starts from Clash's default port.
                let mode = if manual_proxy(self.driver.vm()).is_some() {
                    "system"
                } else {
                    "manual"
                };
                self.driver.select_proxy_mode(mode, effects);
            }
            SettingItem::ProxyTest => self.driver.test_proxy(effects),
            SettingItem::ThinkingTrace => {
                let next = !self.driver.vm().thinking_collapsed;
                self.driver.set_thinking_collapsed(next, effects);
            }
            SettingItem::AutoUpdate => {
                let next = !self.driver.vm().auto_update;
                self.driver.toggle_auto_update(next, effects);
            }
            SettingItem::Update => match &self.driver.vm().update {
                UpdateState::Available { .. } => self.driver.download_update(effects),
                UpdateState::Ready { .. } => self.driver.install_update(effects),
                UpdateState::Checking | UpdateState::Downloading { .. } => {}
                _ => self.driver.check_update(true, effects),
            },
            SettingItem::Provider(index) => {
                let enabled = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.providers.get(index))
                    .is_some_and(|provider| provider.enabled);
                self.driver.act(
                    FrontendAction::SettingsProviderToggled(index, !enabled),
                    effects,
                );
                self.driver.save_settings(effects);
            }
            SettingItem::Mcp(index) => {
                let enabled = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.mcp_servers.get(index))
                    .is_some_and(|server| server.enabled);
                self.driver.toggle_mcp_server(index, !enabled, effects);
            }
            SettingItem::Web(index) => {
                let enabled = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .and_then(|settings| settings.web_backends.get(index))
                    .is_some_and(|backend| backend.enabled);
                self.driver.toggle_web_backend(index, !enabled, effects);
            }
            SettingItem::Skill(index) => {
                let Some(skill) = self.driver.vm().skills.get(index) else {
                    return;
                };
                let disabled_now = self
                    .driver
                    .vm()
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.skills.disabled.contains(&skill.slug));
                self.driver.act(
                    FrontendAction::SettingsSkillToggled(skill.slug.clone(), disabled_now),
                    effects,
                );
                self.driver.save_settings(effects);
            }
        }
    }

    /// `a` on a row opens the right key input, when the row has a key id.
    fn open_key_editor(&mut self, item: &SettingItem) {
        let target = match item {
            SettingItem::Provider(index) => self
                .driver
                .vm()
                .settings
                .as_ref()
                .and_then(|settings| settings.providers.get(*index))
                .map(|provider| provider.id.clone()),
            SettingItem::Mcp(index) => self
                .driver
                .vm()
                .settings
                .as_ref()
                .and_then(|settings| settings.mcp_servers.get(*index))
                .map(|server| format!("mcp-{}", server.id)),
            SettingItem::Web(index) => self
                .driver
                .vm()
                .settings
                .as_ref()
                .and_then(|settings| settings.web_backends.get(*index))
                .map(|backend| format!("web-{}", backend.id)),
            _ => None,
        };
        if let Some(provider) = target {
            self.settings_overlay = Some(SettingsOverlay::ApiKey {
                provider,
                buffer: String::new(),
            });
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if self.driver.vm().view == MainView::Settings {
            let vertical =
                Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
            self.draw_settings(frame, vertical[0]);
            self.draw_status(frame, vertical[1]);
            if self.driver.vm().pending_ask.is_some() {
                self.draw_ask(frame, area);
            } else {
                self.draw_settings_overlay(frame, area);
            }
            return;
        }
        let composer_height = (self.composer.lines().len() as u16 + 2).clamp(3, 8);
        let vertical = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
        self.draw_transcript(frame, vertical[0]);
        self.draw_composer(frame, vertical[1]);
        self.draw_status(frame, vertical[2]);
        if self.driver.vm().pending_ask.is_some() {
            self.draw_ask(frame, area);
        } else {
            self.draw_menu(frame, vertical[0]);
        }
    }

    /// Frameless transcript: `❯` user, `⏺` assistant, `⚒` tool calls,
    /// dim tool results, the live stream, queued follow-ups, the red
    /// turn alert, all as one wrapped flow.
    fn draw_transcript(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let mut lines: Vec<Line> = Vec::new();
        if let Some(active) = &vm.active {
            for entry in &active.entries {
                render_entry(entry, &mut lines, vm.thinking_collapsed);
                // A running subagent says what it is doing under its call;
                // only jobs still in `live_jobs` are running.
                if entry.kind == EntryKind::ToolCall
                    && let Some(job) = entry
                        .call_id
                        .as_deref()
                        .and_then(|call_id| vm.live_jobs.iter().find(|job| job.call_id == call_id))
                {
                    let step = job.step_label();
                    lines.push(Line::from(Span::styled(
                        format!(
                            "  ↳ {} · {}",
                            first_line(&step),
                            elapsed_label(job.elapsed_ms())
                        ),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            if let Some(stream) = &active.streaming {
                // The elapsed tail proves a long think is still alive; the
                // newest thinking line does the same for its content.
                let elapsed = elapsed_suffix(vm);
                if !stream.thinking.is_empty() {
                    let thinking = Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::ITALIC);
                    if vm.thinking_collapsed {
                        lines.push(Line::from(Span::styled(
                            format!("  ✻ {}{elapsed}", thinking_tail(&stream.thinking)),
                            thinking,
                        )));
                    } else {
                        push_wrapped(&mut lines, "  ✻ ", thinking, &stream.thinking, thinking);
                    }
                }
                if !stream.text.is_empty() {
                    push_reply(&mut lines, "⏺ ", Style::default().fg(CRAB), &stream.text);
                } else if !stream.status.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}{elapsed}", stream.status),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            // `agent` calls the model is still writing: dimmed until they start.
            for call in &vm.preparing_calls {
                push_wrapped(
                    &mut lines,
                    "… ",
                    Style::default().fg(Color::DarkGray),
                    first_line(&call.text),
                    Style::default().fg(Color::DarkGray),
                );
            }
        } else {
            lines.extend(welcome_lines(env!("CARGO_PKG_VERSION")));
        }
        for queued in &vm.queued {
            lines.push(Line::from(Span::styled(
                format!("  queued: {}", first_line(queued)),
                Style::default().fg(Color::Yellow),
            )));
        }
        if let Some(alert) = &vm.turn_alert {
            lines.push(Line::from(Span::styled(
                format!("✖ {alert}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }

        // Scrolling counts wrapped rows, not logical lines: a long line
        // takes several rows, and counting it as one pushes the newest
        // output below the viewport. Walk up from the tail measuring each
        // line until the viewport plus the scroll offset is covered.
        let height = usize::from(area.height);
        let wanted = height.saturating_add(self.scroll);
        let mut rows = 0usize;
        let mut first = lines.len();
        while first > 0 && rows < wanted {
            first -= 1;
            rows += Paragraph::new(borrowed_line(&lines[first]))
                .wrap(Wrap { trim: false })
                .line_count(area.width)
                .max(1);
        }
        // Out of history before the offset was covered: clamp to the top.
        self.scroll = self.scroll.min(rows.saturating_sub(height));
        self.at_top = first == 0 && rows <= height + self.scroll;
        let skip = rows.saturating_sub(height + self.scroll);
        let visible = lines.split_off(first);
        frame.render_widget(
            Paragraph::new(visible)
                .wrap(Wrap { trim: false })
                .scroll((u16::try_from(skip).unwrap_or(u16::MAX), 0)),
            area,
        );
    }

    /// The settings list: dim section headers between selectable rows, the
    /// cursor row reversed, scrolls to keep the cursor visible.
    fn draw_settings(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let rows = settings_rows(vm);
        let mut item_index = 0usize;
        let mut cursor_line = 0usize;
        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                "Settings",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        for row in &rows {
            match row {
                SettingRow::Header(title) => {
                    lines.push(Line::from(Span::styled(
                        format!(" {title}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                SettingRow::Item {
                    label, value, dim, ..
                } => {
                    let selected = item_index == self.settings_cursor;
                    let style = if selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if *dim {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    };
                    let value_style = Style::default().fg(Color::DarkGray);
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {} {:<24}", if selected { "❯" } else { " " }, label),
                            style,
                        ),
                        Span::styled(format!(" {value}"), value_style),
                    ]));
                    if selected {
                        cursor_line = lines.len() - 1;
                    }
                    item_index += 1;
                }
            }
        }
        if let Some(error) = &vm.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" ✖ {error}"),
                Style::default().fg(Color::Red),
            )));
        }
        let height = usize::from(area.height);
        let scroll = if cursor_line >= height {
            cursor_line + 1 - height
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
            area,
        );
    }

    /// The settings overlay (model pick / key input) centered on screen.
    fn draw_settings_overlay(&self, frame: &mut Frame, area: Rect) {
        let Some(overlay) = &self.settings_overlay else {
            return;
        };
        let (title, lines) = match overlay {
            SettingsOverlay::Model {
                provider,
                cursor,
                filter,
            } => {
                let rows = self.picker_rows(provider.as_deref(), filter);
                let first = cursor.saturating_sub(PICKER_ROWS - 1);
                let mut lines: Vec<Line> = Vec::new();
                if provider.is_some() {
                    lines.push(Line::from(Span::styled(
                        " ← providers",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                for (index, row) in rows.iter().enumerate().skip(first).take(PICKER_ROWS) {
                    let style = if index == *cursor {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    let vm = self.driver.vm();
                    let mark = match row {
                        PickerRow::Pin { .. } => "★ ",
                        PickerRow::Provider { id, .. } => {
                            if vm.selected_provider.as_deref() == Some(id.as_str()) {
                                "● "
                            } else {
                                "  "
                            }
                        }
                        PickerRow::Model {
                            provider, model, ..
                        } => {
                            if vm.selected_provider.as_deref() == Some(provider.as_str())
                                && vm.selected_model.as_deref() == Some(model.as_str())
                            {
                                "● "
                            } else {
                                "  "
                            }
                        }
                    };
                    lines.push(Line::from(vec![Span::styled(
                        format!(" {}{}", mark, row.label()),
                        style,
                    )]));
                }
                if rows.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "  no matches",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(Span::styled(
                    format!(
                        " {}type to filter · Enter {}· Esc close",
                        if filter.is_empty() {
                            String::new()
                        } else {
                            format!("filter: {filter} · ")
                        },
                        if provider.is_none() {
                            "open "
                        } else {
                            "select "
                        }
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
                let title = match provider {
                    Some(id) => format!(" {} ", provider_display_name(self.driver.vm(), id)),
                    None => " Model ".to_owned(),
                };
                (title, lines)
            }
            SettingsOverlay::ApiKey { provider, buffer } => {
                let masked = "•".repeat(buffer.chars().count());
                (
                    format!(" API key — {provider} "),
                    vec![
                        Line::from(Span::styled(format!(" {masked}▌"), Style::default())),
                        Line::from(Span::styled(
                            " Enter save · Esc cancel",
                            Style::default().fg(Color::DarkGray),
                        )),
                    ],
                )
            }
            SettingsOverlay::BaseUrl { buffer, .. } => (
                " Base URL ".to_owned(),
                vec![
                    Line::from(Span::styled(format!(" {buffer}▌"), Style::default())),
                    Line::from(Span::styled(
                        " Enter save · Esc cancel",
                        Style::default().fg(Color::DarkGray),
                    )),
                ],
            ),
            SettingsOverlay::ProxyUrl { buffer } => (
                " Manual proxy ".to_owned(),
                vec![
                    Line::from(Span::styled(format!(" {buffer}▌"), Style::default())),
                    Line::from(Span::styled(
                        " http / https / socks5 / socks5h · host:port reads as http",
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::from(Span::styled(
                        " Enter save · empty = follow system · Esc cancel",
                        Style::default().fg(Color::DarkGray),
                    )),
                ],
            ),
        };
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        let width = (area.width.saturating_mul(2) / 3)
            .max(34)
            .min(area.width.saturating_sub(4));
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(title),
            ),
            popup,
        );
    }

    /// The main layout's only bordered widget: the composer.
    fn draw_composer(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let title = if vm.sending {
            " ❯ (running — Enter queues, Esc stops) "
        } else {
            " ❯ "
        };
        self.composer.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(title),
        );
        frame.render_widget(&self.composer, area);
    }

    /// One status line: model · directory · context left · key hints, with
    /// the newest notice painted over it.
    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        if let Some(notice) = self.notices.last() {
            let color = match notice.kind {
                ToastKind::Error => Color::Red,
                ToastKind::Warning => Color::Yellow,
                ToastKind::Info => Color::Cyan,
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {}", notice.text),
                    Style::default().fg(color),
                )),
                area,
            );
            return;
        }
        if vm.view == MainView::Settings {
            let saving = if vm.settings.as_ref().is_some_and(|settings| settings.saving) {
                "saving… · "
            } else if vm.settings.as_ref().is_some_and(|settings| settings.dirty) {
                "unsaved · "
            } else {
                ""
            };
            let status =
                format!(" {saving}↑↓ move · Enter/←→ change · a api key · u base url · Esc back ");
            frame.render_widget(
                Paragraph::new(Span::styled(status, Style::default().fg(Color::DarkGray))),
                area,
            );
            return;
        }
        let model = vm.selected_model.as_deref().unwrap_or("no model");
        let dir = vm
            .project_dir
            .as_deref()
            .map(shorten_home)
            .unwrap_or_else(|| "no folder".to_owned());
        let window = mycode_frontend::view_model::model_context_window(vm);
        let mut segments = vec![model.to_owned(), dir];
        // An unknown window or usage leaves the segment out, not blank.
        if let Some(percent) =
            mycode_frontend::view_model::context_remaining_percent(vm.context_used, window)
        {
            segments.push(format!("ctx {percent}%"));
        }
        // Hints follow what the keys do right now: Esc only stops a turn.
        segments.push(if self.turn_busy() {
            "Enter queue · Esc stop".to_owned()
        } else {
            "Enter send · ^C quit".to_owned()
        });
        let status = format!(" {} ", segments.join(" · "));
        frame.render_widget(
            Paragraph::new(Span::styled(status, Style::default().fg(Color::DarkGray))),
            area,
        );
    }

    /// The open `/`/`@` menu, docked to the bottom of the transcript area.
    fn draw_menu(&self, frame: &mut Frame, area: Rect) {
        let Some(mention) = &self.driver.vm().mention else {
            return;
        };
        if mention.items.is_empty() {
            return;
        }
        let highlight = self.menu_highlight();
        let first = highlight.saturating_sub(MENU_ROWS - 1);
        let lines: Vec<Line> = mention
            .items
            .iter()
            .enumerate()
            .skip(first)
            .take(MENU_ROWS)
            .map(|(index, item)| {
                let style = if index == highlight {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::styled(format!(" {} ", item.insert), style),
                    Span::styled(
                        format!(" {}", item.label),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            })
            .collect();
        let height = (lines.len() as u16 + 2).min(area.height);
        let popup = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(height),
            width: area.width,
            height,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
            popup,
        );
    }

    /// Ask card: centered overlay listing the current question and choices.
    fn draw_ask(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let Some(asks) = &vm.pending_ask else {
            return;
        };
        if asks.is_empty() {
            return;
        }
        let index = self.driver.ask_index().min(asks.len() - 1);
        let ask = &asks[index];
        let picked = vm.ask_answers.get(index).map_or("", String::as_str);
        let mut lines = vec![
            Line::from(Span::styled(
                format!("? {}", ask.question),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        for (number, choice) in ask.choices.iter().enumerate() {
            let style = if !picked.is_empty() && picked.split('\n').any(|line| line == choice) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("  {}. {choice}", number + 1),
                style,
            )));
        }
        let hint = if ask.choices.is_empty() {
            "  (type an answer, Enter continues, Esc dismisses)"
        } else if ask.multiple {
            "  (type numbers like 1,3 or an answer, Enter continues, Esc dismisses)"
        } else {
            "  (type a number or an answer, Enter continues, Esc dismisses)"
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        // `max` before `min`: on a terminal narrower than 34 columns a
        // `clamp(30, width - 4)` would have min > max and panic.
        let width = (area.width.saturating_mul(3) / 4)
            .max(30)
            .min(area.width.saturating_sub(4));
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(format!(
                    " question {} of {} ",
                    index + 1,
                    asks.len()
                )))
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}

/// One row of the settings model picker.
#[derive(Clone)]
enum PickerRow {
    /// A recently used provider+model pin.
    Pin {
        provider: String,
        model: String,
        label: String,
    },
    /// An enabled provider; Enter descends to its model list.
    Provider { id: String, label: String },
    /// One model of one provider.
    Model {
        provider: String,
        model: String,
        label: String,
    },
}

impl PickerRow {
    fn label(&self) -> &str {
        match self {
            Self::Pin { label, .. } | Self::Provider { label, .. } | Self::Model { label, .. } => {
                label
            }
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Pin { model, .. } | Self::Model { model, .. } => model,
            Self::Provider { id, .. } => id,
        }
    }
}

/// The shared picker filter on the row label and id: every word matches,
/// ignoring case and separators.
fn filter_picker_rows(rows: Vec<PickerRow>, filter: &str) -> Vec<PickerRow> {
    rows.into_iter()
        .filter(|row| picker::query_matches(filter, &[row.label(), row.id()]))
        .collect()
}

/// Next value in `values` cycling by `direction` (wraps); unknown currents
/// start at the first entry.
fn cycle(values: &[&'static str], current: &str, direction: i8) -> String {
    let len = values.len() as i64;
    let index = values
        .iter()
        .position(|value| *value == current)
        .map(|index| index as i64);
    let next = match index {
        Some(index) => (index + i64::from(direction)).rem_euclid(len),
        None => 0,
    };
    values[next as usize].to_owned()
}

/// The selectable items of the settings page, in display order.
fn settings_items(vm: &WorkspaceState) -> Vec<SettingItem> {
    settings_rows(vm)
        .into_iter()
        .filter_map(|row| match row {
            SettingRow::Item { item, .. } => Some(item),
            SettingRow::Header(_) => None,
        })
        .collect()
}

/// The stored manual proxy URL, or `None` while following the system.
fn manual_proxy(vm: &WorkspaceState) -> Option<String> {
    let stored = vm.settings.as_ref()?.proxy.clone();
    match mycode_config::ProxyPreference::parse(&stored) {
        Some(mycode_config::ProxyPreference::Url(url)) => Some(url),
        _ => None,
    }
}

/// The "Proxy test" row value: which route the probe took and how it went.
fn proxy_test_value(test: &ProxyTestState) -> String {
    use mycode_app::ProxyTestRoute;
    match test {
        ProxyTestState::Idle => "Enter to fetch google.com through the proxy".to_owned(),
        ProxyTestState::Testing => "testing…".to_owned(),
        ProxyTestState::Invalid(message) => format!("✗ {message}"),
        ProxyTestState::Done(report) => {
            let route = match &report.route {
                ProxyTestRoute::Direct => "direct (no system proxy set)".to_owned(),
                ProxyTestRoute::System(uri) => format!("system proxy {uri}"),
                ProxyTestRoute::Manual(url) => format!("proxy {url}"),
            };
            match &report.outcome {
                Ok((status, elapsed)) => format!(
                    "✓ google.com · {route} · HTTP {status} · {} ms",
                    elapsed.as_millis()
                ),
                Err(message) => format!(
                    "✗ google.com unreachable · {route} · {}",
                    mycode_app::brief_error(message)
                ),
            }
        }
    }
}

/// The full settings page: headers and rows in display order.
fn settings_rows(vm: &WorkspaceState) -> Vec<SettingRow> {
    let mut rows = Vec::new();
    let settings = vm.settings.as_ref();

    rows.push(SettingRow::Header("Model".to_owned()));
    let model = match (
        vm.selected_provider.as_deref(),
        vm.selected_model.as_deref(),
    ) {
        (Some(provider), Some(model)) => format!(
            "{} / {}",
            provider_display_name(vm, provider),
            model_display_name(vm, provider, model)
        ),
        (_, Some(model)) => model.to_owned(),
        _ => "none — Enter to pick".to_owned(),
    };
    rows.push(SettingRow::Item {
        item: SettingItem::Model,
        label: "Model".to_owned(),
        value: model,
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::Thinking,
        label: "Thinking".to_owned(),
        value: selected_reasoning_level(vm),
        dim: false,
    });

    rows.push(SettingRow::Header("General".to_owned()));
    rows.push(SettingRow::Item {
        item: SettingItem::Theme,
        label: "Theme".to_owned(),
        value: settings.map(|s| s.theme.clone()).unwrap_or_default(),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::Language,
        label: "Language".to_owned(),
        value: settings
            .map(|s| s.language.clone())
            .unwrap_or_else(|| "auto".to_owned()),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::FontSize,
        label: "Font size".to_owned(),
        value: settings
            .map(|s| s.font_size.clone())
            .unwrap_or_else(|| "m".to_owned()),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::ThinkingTrace,
        label: "Show thinking".to_owned(),
        value: if vm.thinking_collapsed {
            "off".to_owned()
        } else {
            "on".to_owned()
        },
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::Proxy,
        label: "Proxy".to_owned(),
        value: match manual_proxy(vm) {
            Some(url) => format!("manual · {url}  (Enter edit · ←→ follow system)"),
            None => "follow system  (Enter type a proxy · ←→ manual)".to_owned(),
        },
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::ProxyTest,
        label: "Proxy test".to_owned(),
        value: proxy_test_value(&vm.proxy_test),
        dim: false,
    });
    rows.push(SettingRow::Item {
        item: SettingItem::AutoUpdate,
        label: "Auto-update".to_owned(),
        value: if vm.auto_update {
            "on".to_owned()
        } else {
            "off".to_owned()
        },
        dim: false,
    });

    rows.push(SettingRow::Header("Providers".to_owned()));
    if let Some(settings) = settings {
        for (index, provider) in settings.providers.iter().enumerate() {
            let key = if settings.providers_with_keys.contains(&provider.id) {
                " · key ✓"
            } else {
                ""
            };
            rows.push(SettingRow::Item {
                item: SettingItem::Provider(index),
                label: format!(
                    "[{}] {}",
                    if provider.enabled { "x" } else { " " },
                    provider.id
                ),
                value: format!("{} · {} models{key}", provider.kind, provider.models.len()),
                dim: !provider.enabled,
            });
        }
        if settings.providers.is_empty() {
            rows.push(SettingRow::Header("  (none configured)".to_owned()));
        }
    }

    rows.push(SettingRow::Header("MCP".to_owned()));
    if let Some(settings) = settings {
        for (index, server) in settings.mcp_servers.iter().enumerate() {
            let probing = if vm.mcp_probing.contains(&server.id) {
                " · probing…"
            } else {
                ""
            };
            rows.push(SettingRow::Item {
                item: SettingItem::Mcp(index),
                label: format!("[{}] {}", if server.enabled { "x" } else { " " }, server.id),
                value: format!("{}{}", server.transport, probing),
                dim: !server.enabled,
            });
        }
        if settings.mcp_servers.is_empty() {
            rows.push(SettingRow::Header("  (none configured)".to_owned()));
        }
    }

    rows.push(SettingRow::Header("Web search".to_owned()));
    if let Some(settings) = settings {
        for (index, backend) in settings.web_backends.iter().enumerate() {
            rows.push(SettingRow::Item {
                item: SettingItem::Web(index),
                label: format!(
                    "[{}] {}",
                    if backend.enabled { "x" } else { " " },
                    backend.id
                ),
                value: String::new(),
                dim: !backend.enabled,
            });
        }
        if settings.web_backends.is_empty() {
            rows.push(SettingRow::Header("  (none configured)".to_owned()));
        }
    }

    if !vm.skills.is_empty() {
        rows.push(SettingRow::Header("Skills".to_owned()));
        for (index, skill) in vm.skills.iter().enumerate() {
            let disabled = settings.is_some_and(|s| s.skills.disabled.contains(&skill.slug));
            rows.push(SettingRow::Item {
                item: SettingItem::Skill(index),
                label: format!("[{}] /{}", if disabled { " " } else { "x" }, skill.slug),
                value: skill.title.clone(),
                dim: disabled,
            });
        }
    }

    rows.push(SettingRow::Header(format!(
        "About — mycode-cli {}",
        env!("CARGO_PKG_VERSION")
    )));
    let update = match &vm.update {
        UpdateState::Idle => "press Enter to check".to_owned(),
        UpdateState::Checking => "checking…".to_owned(),
        UpdateState::UpToDate => "up to date".to_owned(),
        UpdateState::Available { version, .. } => {
            format!("v{version} available — Enter to download")
        }
        UpdateState::Downloading { version } => format!("downloading v{version}…"),
        UpdateState::Ready { version } => {
            format!("v{version} ready — installs on exit; Enter to quit and install")
        }
        UpdateState::Failed(message) => format!("failed: {message} — Enter to retry"),
    };
    rows.push(SettingRow::Item {
        item: SettingItem::Update,
        label: "Update".to_owned(),
        value: update,
        dim: false,
    });
    rows
}

/// Applies the composer's fixed styling to a fresh textarea.
fn new_composer(mut composer: TextArea<'static>) -> TextArea<'static> {
    composer.set_cursor_line_style(Style::default());
    composer.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    composer.set_placeholder_text("Ask the agent");
    composer
}

/// The crab greeting on an empty transcript — "hi ferris".
fn welcome_lines(version: &str) -> Vec<Line<'static>> {
    let crab = Style::default().fg(CRAB);
    let dim = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(Span::styled("    ▄  ▄▄▄▄▄▄▄  ▄", crab)),
        Line::from(vec![
            Span::styled("  ▄▄█ █ o o █ █▄▄", crab),
            Span::styled(
                "   hi ferris",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("   ▀▀ ▀▀▀▀▀▀▀▀ ▀▀", crab),
            Span::styled(format!("   MYCode Cli {version}"), dim),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  type below and press Enter · / for commands · /settings picks a model · @ for files",
            dim,
        )),
        Line::from(""),
    ]
}

fn render_entry(entry: &ConversationEntry, lines: &mut Vec<Line<'_>>, collapsed: bool) {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match entry.kind {
        // A compaction summary rides a user message but is not the user's
        // words; one dim line marks where earlier context was folded.
        EntryKind::UserMessage if mycode_app::is_compaction_summary(&entry.text) => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ≡ earlier context compacted into a summary",
                Style::default().fg(Color::DarkGray),
            )));
        }
        EntryKind::UserMessage => {
            lines.push(Line::from(""));
            push_wrapped(
                lines,
                "❯ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                &entry.text,
                bold,
            );
        }
        EntryKind::AssistantMessage => {
            let thinking = Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC);
            if !entry.thinking.is_empty() {
                if collapsed {
                    let tail = thinking_tail(&entry.thinking);
                    let headline = if tail.is_empty() {
                        "thinking ▸".to_owned()
                    } else {
                        format!("thinking · {tail} ▸")
                    };
                    push_wrapped(lines, "  ✻ ", thinking, &headline, thinking);
                } else {
                    push_wrapped(lines, "  ✻ ", thinking, &entry.thinking, thinking);
                }
            }
            // Interruption lines the agent committed (`[error] …`) stay
            // visible but read as errors, not as answer text.
            let marker = Style::default().fg(CRAB);
            let error = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
            let mut normal: Vec<&str> = Vec::new();
            for line in entry.text.lines() {
                if line.starts_with("[error]") {
                    if !normal.is_empty() {
                        let joined = normal.join("\n");
                        push_reply(lines, "⏺ ", marker, &joined);
                        normal.clear();
                    }
                    push_wrapped(lines, "✖ ", error, line, error);
                } else {
                    normal.push(line);
                }
            }
            if !normal.is_empty() {
                let joined = normal.join("\n");
                push_reply(lines, "⏺ ", marker, &joined);
            }
        }
        EntryKind::ToolCall => {
            push_wrapped(
                lines,
                "⚒ ",
                Style::default().fg(Color::Cyan),
                first_line(&entry.text),
                Style::default().fg(Color::Cyan),
            );
        }
        EntryKind::ToolResult => {
            push_wrapped(
                lines,
                "  ⎿ ",
                Style::default().fg(Color::DarkGray),
                first_line(&entry.text),
                Style::default().fg(Color::DarkGray),
            );
        }
    }
}

/// `marker` plus a wrapped body: first line carries the marker in
/// `marker_style`, continuation lines indent under the text in
/// `text_style`.
fn push_wrapped(
    lines: &mut Vec<Line<'_>>,
    marker: &str,
    marker_style: Style,
    text: &str,
    text_style: Style,
) {
    let mut iter = text.lines();
    let indent = " ".repeat(marker.chars().count());
    match iter.next() {
        Some(first) => lines.push(Line::from(vec![
            Span::styled(marker.to_owned(), marker_style),
            Span::styled(first.to_owned(), text_style),
        ])),
        None => {
            lines.push(Line::from(Span::styled(marker.to_owned(), marker_style)));
            return;
        }
    }
    for line in iter {
        lines.push(Line::from(Span::styled(
            format!("{indent}{line}"),
            text_style,
        )));
    }
}

/// What one key did to a single-line settings input.
enum LineKey {
    Submit,
    Cancel,
    Edited,
}

/// Shared editing for the API-key, base-URL, and proxy inputs.
fn edit_line(key: &KeyEvent, buffer: &mut String) -> LineKey {
    match key.code {
        KeyCode::Esc => LineKey::Cancel,
        KeyCode::Enter => LineKey::Submit,
        KeyCode::Backspace => {
            buffer.pop();
            LineKey::Edited
        }
        KeyCode::Char(ch) => {
            buffer.push(ch);
            LineKey::Edited
        }
        _ => LineKey::Edited,
    }
}

/// The distinct 1-based choice numbers in `text` (comma or space
/// separated), or `None` when any word is not a choice number.
fn choice_numbers(text: &str, choices: usize) -> Option<Vec<usize>> {
    let mut numbers = Vec::new();
    for word in text.split([',', ' ']).filter(|word| !word.is_empty()) {
        let number = word.parse::<usize>().ok()?;
        if !(1..=choices).contains(&number) {
            return None;
        }
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    (!numbers.is_empty()).then_some(numbers)
}

/// A copy of `line` whose spans borrow its text, for measuring wrapped
/// height without copying every string.
fn borrowed_line<'a>(line: &'a Line<'_>) -> Line<'a> {
    Line {
        spans: line
            .spans
            .iter()
            .map(|span| Span::styled(span.content.as_ref(), span.style))
            .collect(),
        style: line.style,
        alignment: line.alignment,
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// ` · 42s` while a turn runs; empty at rest or before the clock starts.
fn elapsed_suffix(vm: &WorkspaceState) -> String {
    let ms = vm.turn_elapsed_ms();
    if ms == 0 {
        String::new()
    } else {
        format!(" · {}", elapsed_label(ms))
    }
}

/// Newest non-empty thinking line, cut from the front so a line still
/// streaming shows its newest words — same rule as the desktop headline.
fn thinking_tail(text: &str) -> String {
    const MAX: usize = 72;
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("");
    let count = line.chars().count();
    if count <= MAX {
        return line.to_owned();
    }
    let tail: String = line.chars().skip(count - (MAX - 1)).collect();
    format!("…{tail}")
}

/// `~/…` display for the status bar's directory segment.
fn shorten_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = home.to_string_lossy().replace('\\', "/");
        let home = home.trim_end_matches('/');
        let path_unix = path.replace('\\', "/");
        // Only a whole path component matches: `/home/al` is not `~` of
        // `/home/alice`.
        if let Some(rest) = path_unix.strip_prefix(home)
            && !home.is_empty()
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::choice_numbers;

    #[test]
    fn choice_numbers_accept_lists_and_reject_free_text() {
        assert_eq!(choice_numbers("2", 3), Some(vec![2]));
        assert_eq!(choice_numbers(" 1, 3 3", 3), Some(vec![1, 3]));
        assert_eq!(choice_numbers("4", 3), None);
        assert_eq!(choice_numbers("0", 3), None);
        assert_eq!(choice_numbers("1 and 2", 3), None);
        assert_eq!(choice_numbers("", 3), None);
    }
}
