//! The terminal frontend: one shared [`FrontendDriver`], a borderless
//! scrolling transcript, a single bordered composer, and a one-line status
//! bar. The Grok-style layout keeps the transcript frameless — the terminal
//! is already the container — and every picklist or card is a transient
//! overlay above the composer.
//!
//! Key map: `Enter` submits (or accepts the highlighted `/`/`@` row),
//! `Alt+Enter`/`Ctrl+J` inserts a newline, `Up`/`Down` move through an open
//! `/`/`@` menu, `Esc` dismisses the topmost layer or cancels a running
//! turn, `Ctrl+C` cancels the turn and quits when idle, `Ctrl+N` starts a
//! new task, `PageUp`/`PageDown` scroll.
//!
//! The terminal's working directory is the project: once the remembered UI
//! state has loaded, it replaces the folder a desktop session left behind,
//! so tools never run in a directory other than the one `mycode` started in.

use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use mycode_app::{ConversationEntry, EntryKind};
use mycode_frontend::driver::{Effect, FrontendDriver, ToastKind};
use mycode_frontend::view_model::{FrontendAction, MentionKind, preferred_slash_index};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::{Frame, Terminal};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tui_textarea::{CursorMove, Input, Key as TextareaKey, TextArea};

/// Wake channel payload: the driver thread signals "inbox has work".
type WakeRx = mpsc::Receiver<()>;

/// Info and warning notices disappear after this reading time; errors stay
/// until `Esc` dismisses them.
const NOTICE_TTL: Duration = Duration::from_secs(6);
/// Rows of an open `/`/`@` menu painted above the composer.
const MENU_ROWS: usize = 8;

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
    /// Rows of transcript hidden below the viewport bottom; `0` pins to tail.
    scroll: usize,
    notices: Vec<Notice>,
    /// Highlighted row of the open `/`/`@` menu; `None` follows the shared
    /// preferred row. Reset whenever the draft changes.
    mention_selected: Option<usize>,
    /// The working directory, applied as the project once the remembered
    /// UI state has loaded. `None` after that (or when it is unreadable).
    startup_dir: Option<String>,
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
        let _ = disable_raw_mode();
        let _ = execute!(
            self.0.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

impl Tui {
    fn new(driver: FrontendDriver) -> Self {
        Self {
            driver,
            composer: new_composer(TextArea::default()),
            scroll: 0,
            notices: Vec::new(),
            mention_selected: None,
            startup_dir: std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned()),
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
            // A reader who paged up keeps their place while the stream grows.
            self.driver.set_conversation_at_tail(self.scroll == 0);
            self.driver.poll(&mut effects);
            while let Some(event) = self.driver.next_event() {
                self.driver.on_event(event, &mut effects);
            }
            self.adopt_startup_dir(&mut effects);
            if !effects.is_empty() {
                self.apply_effects(std::mem::take(&mut effects));
            }
            // Edit-and-resend, skill inserts, and the welcome send leave
            // their draft here rather than in an effect.
            if let Some(text) = self.driver.take_composer_prefill() {
                self.set_composer_text(&text);
            }
            self.expire_notices();
            if terminal.draw(|frame| self.draw(frame)).is_err() {
                return 1;
            }
            if self.quit {
                self.driver.shutdown();
                return 0;
            }
            match event::poll(Duration::from_millis(60)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) => self.on_key(key),
                    Ok(Event::Paste(text)) => self.on_paste(&text),
                    Ok(_) => {}
                    Err(_) => return 1,
                },
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
        self.driver.act(FrontendAction::WorkspaceRootAdded(dir), effects);
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
    }

    /// Drops info and warning notices past their reading time. Runs every
    /// frame, not only when new effects arrive.
    fn expire_notices(&mut self) {
        self.notices.retain(|notice| {
            notice.kind == ToastKind::Error || notice.shown.elapsed() < NOTICE_TTL
        });
    }

    /// A bracketed paste lands as text, newlines included, never as Enter.
    fn on_paste(&mut self, text: &str) {
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
            self.driver.act(FrontendAction::ComposerChanged(draft), effects);
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
        match (key.code, ctrl, alt) {
            (KeyCode::Char('c'), true, false) => {
                if self.driver.vm().sending || self.driver.vm().compacting {
                    self.driver.cancel_chat(&mut effects);
                } else {
                    self.quit = true;
                }
            }
            (KeyCode::Esc, false, false) => {
                if self.driver.vm().pending_ask.is_some() {
                    self.driver.dismiss_ask(&mut effects);
                } else if self.driver.vm().sending || self.driver.vm().compacting {
                    self.driver.cancel_chat(&mut effects);
                } else if self.driver.dismiss_floating() {
                    self.mention_selected = None;
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
            (KeyCode::Enter, false, false)
                if !matches!(event::poll(Duration::ZERO), Ok(true)) =>
            {
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
        if let Some(asks) = self.driver.vm().pending_ask.clone() {
            let index = self.driver.ask_index().min(asks.len().saturating_sub(1));
            let choices = &asks[index].choices;
            // A bare number picks that choice; anything else is free text.
            match text.trim().parse::<usize>() {
                Ok(number) if (1..=choices.len()).contains(&number) => {
                    let answer = choices[number - 1].clone();
                    self.driver.pick_ask_choice(index, answer, false, effects);
                }
                _ => self.driver.ask_text_typed(index, text, effects),
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
                    push_wrapped(&mut lines, "⏺ ", &stream.text, Style::default());
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
            rows += Paragraph::new(lines[first].clone())
                .wrap(Wrap { trim: false })
                .line_count(area.width)
                .max(1);
        }
        // Out of history before the offset was covered: clamp to the top.
        self.scroll = self.scroll.min(rows.saturating_sub(height));
        let skip = rows.saturating_sub(height + self.scroll);
        let visible = lines.split_off(first);
        frame.render_widget(
            Paragraph::new(visible)
                .wrap(Wrap { trim: false })
                .scroll((u16::try_from(skip).unwrap_or(u16::MAX), 0)),
            area,
        );
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
                    Span::styled(format!(" {}", item.label), Style::default().fg(Color::DarkGray)),
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
        let Some(asks) = self.driver.vm().pending_ask.clone() else {
            return;
        };
        if asks.is_empty() {
            return;
        }
        let index = self.driver.ask_index().min(asks.len() - 1);
        let ask = &asks[index];
        let picked = self
            .driver
            .vm()
            .ask_answers
            .get(index)
            .cloned()
            .unwrap_or_default();
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
        } else {
            "  (type a number or an answer, Enter continues, Esc dismisses)"
        };
        lines.push(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))));
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

/// Applies the composer's fixed styling to a fresh textarea.
fn new_composer(mut composer: TextArea<'static>) -> TextArea<'static> {
    composer.set_cursor_line_style(Style::default());
    composer.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    composer.set_placeholder_text("Ask the agent");
    composer
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
