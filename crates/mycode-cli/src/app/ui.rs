//! Frame painting: the transcript, composer, status line, settings page,
//! and the transient overlays (menus, pickers, ask card). Every function
//! here reads the shared view model and writes ratatui widgets; none of
//! them changes driver state.

use mycode_frontend::driver::ToastKind;
use mycode_frontend::view_model::{
    MainView, elapsed_label, open_session_is_empty, provider_display_name,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::render::{
    borrowed_line, elapsed_suffix, first_line, push_wrapped, render_entry, shorten_home,
    thinking_tail, welcome_lines,
};
use super::settings::PickerRow;
use super::settings_rows::{SettingRow, settings_rows};
use super::{MENU_ROWS, PICKER_ROWS, Tui};
use crate::reply::push_reply;

impl Tui {
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        // The Theme row changes ink, and the settings document is the only
        // source; refresh here so a change shows on the next frame.
        self.palette = super::theme::Palette::of(self.driver.vm());
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
        let attachment_rows = u16::from(!self.attachments.is_empty());
        let vertical = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(attachment_rows),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);
        self.draw_transcript(frame, vertical[0]);
        if attachment_rows > 0 {
            self.draw_attachments(frame, vertical[1]);
        }
        self.draw_composer(frame, vertical[2]);
        self.draw_status(frame, vertical[3]);
        if self.driver.vm().pending_ask.is_some() {
            self.draw_ask(frame, area);
        } else {
            self.draw_menu(frame, vertical[0]);
        }
    }

    /// One line above the composer naming what will be attached. The names
    /// are what the transcript note will show; `Ctrl+X` clears them.
    fn draw_attachments(&self, frame: &mut Frame, area: Rect) {
        let palette = self.palette;
        let mut spans = Vec::new();
        for image in &self.attachments.images {
            spans.push(Span::styled(
                format!(" [image] {} ", image.name),
                Style::default().fg(palette.info),
            ));
        }
        for file in &self.attachments.files {
            spans.push(Span::styled(
                format!(" [file] {} ", file.name),
                Style::default().fg(palette.info),
            ));
        }
        spans.push(Span::styled(
            " · Ctrl+X clears",
            Style::default().fg(palette.dim),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Frameless transcript: `❯` user, `⏺` assistant, `⚒` tool calls,
    /// dim tool results, the live stream, queued follow-ups, the red
    /// turn alert, all as one wrapped flow.
    fn draw_transcript(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let palette = self.palette;
        let mut lines: Vec<Line> = Vec::new();
        // `/new` and Ctrl+N open an empty session rather than no session;
        // the greeting says the composer is a fresh task either way.
        let welcome = vm.active.is_none() || open_session_is_empty(vm);
        if welcome {
            lines.extend(welcome_lines(env!("CARGO_PKG_VERSION"), palette));
        } else if let Some(active) = &vm.active {
            for entry in &active.entries {
                render_entry(entry, &mut lines, vm.thinking_collapsed, palette);
                // A running subagent says what it is doing under its call;
                // only jobs still in `live_jobs` are running.
                if entry.kind == mycode_app::EntryKind::ToolCall
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
                        Style::default().fg(palette.dim),
                    )));
                }
            }
            if let Some(stream) = &active.streaming {
                // The elapsed tail proves a long think is still alive; the
                // newest thinking line does the same for its content.
                let elapsed = elapsed_suffix(vm);
                if !stream.thinking.is_empty() {
                    let thinking = Style::default()
                        .fg(palette.thinking)
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
                    push_reply(
                        &mut lines,
                        "⏺ ",
                        Style::default().fg(palette.accent),
                        &stream.text,
                        palette,
                    );
                } else if !stream.status.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("  {}{elapsed}", stream.status),
                        Style::default().fg(palette.dim),
                    )));
                }
            }
            // `agent` calls the model is still writing: dimmed until they start.
            for call in &vm.preparing_calls {
                push_wrapped(
                    &mut lines,
                    "… ",
                    Style::default().fg(palette.dim),
                    first_line(&call.text),
                    Style::default().fg(palette.dim),
                );
            }
        }
        for steer in &vm.pending_steers {
            lines.push(Line::from(Span::styled(
                format!("  steering: {}", first_line(steer)),
                Style::default().fg(palette.thinking),
            )));
        }
        for queued in &vm.queued {
            lines.push(Line::from(Span::styled(
                format!("  queued: {}", first_line(queued)),
                Style::default().fg(palette.warning),
            )));
        }
        if let Some(alert) = &vm.turn_alert {
            lines.push(Line::from(Span::styled(
                format!("✖ {alert}"),
                Style::default()
                    .fg(palette.error)
                    .add_modifier(Modifier::BOLD),
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
        let palette = self.palette;
        let rows = settings_rows(vm, self.driver.home(), &mut self.shared_mcp);
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
                            .fg(palette.info)
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
                        Style::default().fg(palette.dim)
                    } else {
                        Style::default()
                    };
                    let value_style = Style::default().fg(palette.dim);
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
                SettingRow::Info { label, value } => {
                    lines.push(Line::from(vec![
                        Span::raw(format!("   {label:<24}")),
                        Span::styled(format!(" {value}"), Style::default().fg(palette.dim)),
                    ]));
                }
            }
        }
        if let Some(error) = &vm.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" ✖ {error}"),
                Style::default().fg(palette.error),
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
        let palette = self.palette;
        let Some(overlay) = &self.settings_overlay else {
            return;
        };
        let (title, lines) = match overlay {
            super::settings::SettingsOverlay::Model {
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
                        Style::default().fg(palette.dim),
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
                        Style::default().fg(palette.dim),
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
                    Style::default().fg(palette.dim),
                )));
                let title = match provider {
                    Some(id) => format!(" {} ", provider_display_name(self.driver.vm(), id)),
                    None => " Model ".to_owned(),
                };
                (title, lines)
            }
            super::settings::SettingsOverlay::ApiKey { provider, buffer } => {
                let masked = "•".repeat(buffer.chars().count());
                (
                    format!(" API key — {provider} "),
                    vec![
                        Line::from(Span::styled(format!(" {masked}▌"), Style::default())),
                        Line::from(Span::styled(
                            " Enter save · Esc cancel",
                            Style::default().fg(palette.dim),
                        )),
                    ],
                )
            }
            super::settings::SettingsOverlay::ProxyUrl { buffer } => (
                " Manual proxy ".to_owned(),
                vec![
                    Line::from(Span::styled(format!(" {buffer}▌"), Style::default())),
                    Line::from(Span::styled(
                        " http / https / socks5 / socks5h · host:port reads as http",
                        Style::default().fg(palette.dim),
                    )),
                    Line::from(Span::styled(
                        " Enter save · empty = follow system · Esc cancel",
                        Style::default().fg(palette.dim),
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
                    .border_style(Style::default().fg(palette.dim))
                    .title(title),
            ),
            popup,
        );
    }

    /// The main layout's only bordered widget: the composer.
    fn draw_composer(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let palette = self.palette;
        let title = if vm.sending {
            " ❯ (running — Enter queues · Ctrl+Enter interrupts · Ctrl+G steers · Esc stops) "
        } else {
            " ❯ "
        };
        self.composer.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.dim))
                .title(title),
        );
        frame.render_widget(&self.composer, area);
    }

    /// One status line: model · directory · context left · key hints, with
    /// the newest notice painted over it.
    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let palette = self.palette;
        if let Some(notice) = self.notices.last() {
            let color = match notice.kind {
                ToastKind::Error => palette.error,
                ToastKind::Warning => palette.warning,
                ToastKind::Info => palette.info,
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
                Paragraph::new(Span::styled(status, Style::default().fg(palette.dim))),
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
            "Enter queue · ^G steer · Esc stop".to_owned()
        } else {
            "Enter send · ^C quit".to_owned()
        });
        let status = format!(" {} ", segments.join(" · "));
        frame.render_widget(
            Paragraph::new(Span::styled(status, Style::default().fg(palette.dim))),
            area,
        );
    }

    /// The open `/`/`@` menu, docked to the bottom of the transcript area.
    fn draw_menu(&self, frame: &mut Frame, area: Rect) {
        let palette = self.palette;
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
                    Span::styled(format!(" {}", item.label), Style::default().fg(palette.dim)),
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
                    .border_style(Style::default().fg(palette.dim)),
            ),
            popup,
        );
    }

    /// Ask card: centered overlay listing the current question and choices.
    fn draw_ask(&mut self, frame: &mut Frame, area: Rect) {
        let vm = self.driver.vm();
        let palette = self.palette;
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
            Style::default().fg(palette.dim),
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
