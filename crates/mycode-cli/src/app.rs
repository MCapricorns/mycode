//! The terminal frontend: one shared [`FrontendDriver`], a borderless
//! scrolling transcript, a single bordered composer, and a one-line status
//! bar. The Grok-style layout keeps the transcript frameless — the terminal
//! is already the container — and every picklist or card is a transient
//! overlay above the composer.
//!
//! Key map: `Enter` submits, `Alt+Enter`/`Ctrl+J` inserts a newline,
//! `Esc` dismisses the topmost layer or cancels a running turn, `Ctrl+C`
//! cancels the turn and quits when idle, `PageUp`/`PageDown` scroll.

use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use mycode_app::{ConversationEntry, EntryKind};
use mycode_frontend::driver::{Effect, FrontendDriver, ToastKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::{Frame, Terminal};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::backend::CrosstermBackend;
use tui_textarea::{Input, Key as TextareaKey, TextArea};

/// Wake channel payload: the driver thread signals "inbox has work".
type WakeRx = mpsc::Receiver<()>;

/// One transient notice above the status bar.
struct Notice {
    kind: ToastKind,
    text: String,
    shown: Instant,
}

/// Terminal-side state the shared layer deliberately does not own:
/// keystroke-level text editing, scroll offset, and painted notices.
struct Tui {
    driver: FrontendDriver,
    composer: TextArea<'static>,
    /// Lines of transcript hidden below the viewport bottom; `0` pins to tail.
    scroll: usize,
    notices: Vec<Notice>,
    quit: bool,
}

/// Installs the terminal, runs the event loop, restores the terminal.
/// Returns the process exit code.
pub fn run(home: mycode_config::HomeLayout) -> i32 {
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
            eprintln!("mycode: cannot start the terminal UI: {error}");
            return 1;
        }
    };
    let mut app = Tui::new(driver);
    app.apply_effects(std::mem::take(&mut effects));
    app.event_loop(&mut terminal.0, &wake_rx)
}

/// Restores cooked mode and the normal screen on drop, including panic paths.
struct TerminalGuard(Terminal<CrosstermBackend<Stdout>>);

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        Ok(Self(Terminal::new(backend)?))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.0.backend_mut(), LeaveAlternateScreen);
    }
}

impl Tui {
    fn new(driver: FrontendDriver) -> Self {
        let mut composer = TextArea::default();
        composer.set_cursor_line_style(Style::default());
        composer.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        composer.set_placeholder_text("Ask the agent");
        Self {
            driver,
            composer,
            scroll: 0,
            notices: Vec::new(),
            quit: false,
        }
    }

    /// Poll → effects → paint → keys, until the driver or the user quits.
    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>, wake: &WakeRx) -> i32 {
        let mut effects = Vec::new();
        loop {
            // Drain driver work deposited since the last frame: replies and
            // timers through `poll`, streamed core events through
            // `next_event` — a frontend that skips either never sees the
            // turn finish.
            while wake.try_recv().is_ok() {}
            self.driver.poll(&mut effects);
            while let Some(event) = self.driver.next_event() {
                self.driver.on_event(event, &mut effects);
            }
            if !effects.is_empty() {
                self.apply_effects(std::mem::take(&mut effects));
            }
            if terminal.draw(|frame| self.draw(frame)).is_err() {
                return 1;
            }
            if self.quit {
                self.driver.shutdown();
                return 0;
            }
            match event::poll(Duration::from_millis(60)) {
                Ok(true) => {
                    if let Event::Key(key) = event::read().unwrap_or_else(|_| {
                        Event::Key(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE))
                    }) {
                        self.on_key(key);
                    }
                }
                Ok(false) => {}
                Err(_) => return 1,
            }
        }
    }

    /// Maps shared effects onto terminal-side state.
    fn apply_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Toast { kind, text } => self.notices.push(Notice {
                    kind,
                    text,
                    shown: Instant::now(),
                }),
                Effect::ComposerSet { text, focus: _ } => self.set_composer_text(&text),
                // Focus, window chrome, and paging anchors are GPUI
                // concerns; a terminal frame is always focused.
                Effect::Quit => self.quit = true,
                Effect::ScrollToTail => self.scroll = 0,
                _ => {}
            }
        }
        // Notices outlive their reading time only when errors.
        self.notices.retain(|notice| {
            notice.kind == ToastKind::Error
                || notice.shown.elapsed() < Duration::from_secs(6)
        });
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let mut effects = Vec::new();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match (key.code, ctrl, alt) {
            (KeyCode::Char('c'), true, false) => {
                if self.driver.vm().sending {
                    self.driver.cancel_chat(&mut effects);
                } else {
                    self.quit = true;
                }
            }
            (KeyCode::Esc, false, false) => {
                if self.driver.vm().pending_ask.is_some() {
                    self.driver.dismiss_ask(&mut effects);
                } else if self.driver.vm().sending {
                    self.driver.cancel_chat(&mut effects);
                } else if !self.driver.dismiss_floating() {
                    // Nothing floating: reset scroll to the tail.
                    self.scroll = 0;
                }
            }
            (KeyCode::Enter, false, false) => self.submit_current(&mut effects),
            // Newline: terminals that report Alt/Ctrl+Enter land here; the
            // rest use Ctrl+J, which crossterm maps from LF.
            (KeyCode::Enter, _, true) | (KeyCode::Char('j'), true, false) => {
                self.composer.input(Input {
                    key: TextareaKey::Enter,
                    ..Input::default()
                });
            }
            (KeyCode::PageUp, _, _) => self.scroll = self.scroll.saturating_add(10),
            (KeyCode::PageDown, _, _) => self.scroll = self.scroll.saturating_sub(10),
            (KeyCode::Char('n'), true, false) => {
                self.driver.new_session(&mut effects);
            }
            _ => {
                self.composer.input(Input::from(key));
                let draft = self.composer.lines().join("\n");
                if draft != self.driver.vm().composer_draft {
                    let draft_clone = draft.clone();
                    self.driver.act(
                        mycode_frontend::view_model::FrontendAction::ComposerChanged(draft_clone),
                        &mut effects,
                    );
                }
            }
        }
        self.apply_effects(effects);
    }

    /// Enter: an ask card consumes the text as the row's answer, otherwise
    /// the draft goes through the shared submit path (welcome, queue, send).
    fn submit_current(&mut self, effects: &mut Vec<Effect>) {
        let text = self.composer.lines().join("\n");
        if self.driver.vm().pending_ask.is_some() {
            let index = self.driver.ask_index();
            self.driver.ask_text_typed(index, text, effects);
            self.driver.ask_next(effects);
            self.clear_composer();
            return;
        }
        self.driver.submit(&text, effects);
        self.clear_composer();
        self.scroll = 0;
    }

    /// Rebuilds the textarea so cursor, undo, and scroll all reset.
    fn set_composer_text(&mut self, text: &str) {
        let mut composer = if text.is_empty() {
            TextArea::default()
        } else {
            TextArea::from(text.lines().map(str::to_owned).collect::<Vec<_>>())
        };
        composer.set_cursor_line_style(Style::default());
        composer.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        composer.set_placeholder_text("Ask the agent");
        self.composer = composer;
    }

    fn clear_composer(&mut self) {
        self.set_composer_text("");
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
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
                render_entry(entry, &mut lines);
            }
            if let Some(stream) = &active.streaming {
                if !stream.thinking.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", first_line(&stream.thinking)),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    )));
                }
                if !stream.text.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("⏺ {}", first_line(&stream.text)),
                        Style::default(),
                    )));
                    for line in stream.text.lines().skip(1) {
                        lines.push(Line::from(format!("  {line}")));
                    }
                } else if !stream.status.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", stream.status),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        } else {
            lines.push(Line::from(Span::styled(
                "mycode — type below and press Enter to start.",
                Style::default().fg(Color::DarkGray),
            )));
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

        let height = usize::from(area.height);
        let total = lines.len();
        let max_scroll = total.saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
        let skip = max_scroll - self.scroll;
        let visible: Vec<Line> = lines.into_iter().skip(skip).take(height).collect();
        frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), area);
    }

    /// The only bordered widget: the composer.
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
        let model = vm.selected_model.as_deref().unwrap_or("no model");
        let dir = vm
            .project_dir
            .as_deref()
            .map(shorten_home)
            .unwrap_or_else(|| "no folder".to_owned());
        let window = mycode_frontend::view_model::model_context_window(vm);
        let ctx = mycode_frontend::view_model::context_remaining_percent(vm.context_used, window)
            .map(|percent| format!("ctx {percent}%"))
            .unwrap_or_default();
        let status = format!(" {model} · {dir} · {ctx} · Enter send · Esc stop · ^C quit ");
        frame.render_widget(
            Paragraph::new(Span::styled(status, Style::default().fg(Color::DarkGray))),
            area,
        );
    }

    /// Ask card: centered overlay listing the current question and choices.
    fn draw_ask(&mut self, frame: &mut Frame, area: Rect) {
        let Some(asks) = self.driver.vm().pending_ask.clone() else {
            return;
        };
        let index = self.driver.ask_index().min(asks.len().saturating_sub(1));
        let ask = &asks[index];
        let mut lines = vec![
            Line::from(Span::styled(
                format!("? {}", ask.question),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        for (number, choice) in ask.choices.iter().enumerate() {
            lines.push(Line::from(format!("  {}. {choice}", number + 1)));
        }
        if ask.choices.is_empty() || ask.optional {
            lines.push(Line::from(Span::styled(
                "  (type an answer, Enter continues)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        let width = (area.width.saturating_mul(3) / 4).clamp(30, area.width.saturating_sub(4));
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" question {} of {} ", index + 1, asks.len())),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}

fn render_entry(entry: &ConversationEntry, lines: &mut Vec<Line<'_>>) {
    match entry.kind {
        EntryKind::UserMessage => {
            lines.push(Line::from(""));
            push_wrapped(lines, "❯ ", &entry.text, Style::default().add_modifier(Modifier::BOLD));
        }
        EntryKind::AssistantMessage => {
            if !entry.thinking.is_empty() {
                push_wrapped(
                    lines,
                    "  ",
                    &entry.thinking,
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                );
            }
            push_wrapped(lines, "⏺ ", &entry.text, Style::default());
        }
        EntryKind::ToolCall => {
            push_wrapped(
                lines,
                "⚒ ",
                first_line(&entry.text),
                Style::default().fg(Color::Cyan),
            );
        }
        EntryKind::ToolResult => {
            push_wrapped(
                lines,
                "  ⎿ ",
                first_line(&entry.text),
                Style::default().fg(Color::DarkGray),
            );
        }
    }
}

/// `marker` plus a wrapped body: first line carries the marker, continuation
/// lines indent under the text.
fn push_wrapped(lines: &mut Vec<Line<'_>>, marker: &str, text: &str, style: Style) {
    let mut iter = text.lines();
    let indent = " ".repeat(marker.chars().count());
    match iter.next() {
        Some(first) => lines.push(Line::from(Span::styled(format!("{marker}{first}"), style))),
        None => {
            lines.push(Line::from(Span::styled(marker.to_owned(), style)));
            return;
        }
    }
    for line in iter {
        lines.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// `~/…` display for the status bar's directory segment.
fn shorten_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = home.to_string_lossy().replace('\\', "/");
        let path_unix = path.replace('\\', "/");
        if let Some(rest) = path_unix.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}
