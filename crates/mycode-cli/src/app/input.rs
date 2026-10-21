//! Composer input: keys, pastes, the `/`/`@` menu, and the three send
//! paths (submit, interrupt-and-send, steer). Every handler ends by applying
//! the shared effects it collected, so terminal state and the driver's view
//! model stay in step.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mycode_frontend::driver::{Effect, ToastKind};
use mycode_frontend::view_model::{FrontendAction, MainView, MentionKind, preferred_slash_index};
use tui_textarea::{CursorMove, Input, Key as TextareaKey, TextArea};

use super::Tui;
use super::render::new_composer;
use super::settings::SettingsOverlay;

impl Tui {
    /// A bracketed paste lands as text, newlines included, never as Enter.
    pub(super) fn on_paste(&mut self, text: &str) {
        // On the settings page a paste belongs to the open single-line
        // input (an API key is nearly always pasted), never to the hidden
        // composer.
        if self.driver.vm().view == MainView::Settings {
            let line: String = text.chars().filter(|ch| !ch.is_control()).collect();
            match &mut self.settings_overlay {
                Some(
                    SettingsOverlay::ApiKey { buffer, .. } | SettingsOverlay::ProxyUrl { buffer },
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
        // A path to a picture is the picture, not text: terminals turn a
        // dragged file into its path, and a copied `C:\Shots\a.png` is almost
        // always meant as the image.
        if self.attach_pasted_image(&text) {
            return;
        }
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

    pub(super) fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.driver.set_conversation_at_tail(self.scroll == 0);
        let mut effects = Vec::new();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
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
            (KeyCode::Enter, false, false) if !shift && !self.more_input_queued() => {
                self.submit_current(&mut effects);
            }
            (KeyCode::Enter, true, false) => self.interrupt_send_current(&mut effects),
            (KeyCode::Char('g'), true, false) => self.steer_current(&mut effects),
            // Clipboard pictures: Ctrl+V attaches the image itself (a
            // terminal's own paste chord still types text), Ctrl+X drops it.
            (KeyCode::Char('v'), true, false) => self.attach_clipboard_image(),
            (KeyCode::Char('x'), true, false) => self.clear_attachments(),
            // Newline: terminals that report Shift/Alt+Enter land here; the
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

    /// Whether typed input is already queued behind this Enter — a console
    /// typing a paste out as keys. Anything that is not a key press (mouse
    /// motion from mouse capture, a key release, a resize, a focus change) is
    /// stashed for the event loop and does not count, so moving the mouse
    /// cannot turn Enter into a newline.
    fn more_input_queued(&mut self) -> bool {
        while matches!(event::poll(Duration::ZERO), Ok(true)) {
            let Ok(event) = event::read() else {
                return false;
            };
            let typed = match &event {
                Event::Paste(_) => true,
                Event::Key(key) => key.kind == KeyEventKind::Press,
                _ => false,
            };
            self.pending_events.push_back(event);
            if typed {
                return true;
            }
        }
        false
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
    pub(super) fn menu_highlight(&self) -> usize {
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
        // The model may have changed after the picture was attached; the
        // send is the last place the vision gate can hold.
        if !self.attachments.images.is_empty() && !self.takes_images() {
            self.push_notice(
                ToastKind::Warning,
                "The selected model cannot see images; remove them or pick a vision model."
                    .to_owned(),
            );
            return;
        }
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
        let attachments = std::mem::take(&mut self.attachments);
        if let Some(attachments) = self.driver.submit_with(&text, attachments, effects) {
            // No turn took them: the chips stay so the next Enter sends them.
            self.attachments = attachments;
        }
        self.scroll = 0;
    }

    /// `Ctrl+Enter`: stops the running turn and sends the draft now. With no
    /// turn running, or an ask card or menu open, it is `Enter`.
    fn interrupt_send_current(&mut self, effects: &mut Vec<Effect>) {
        if !self.turn_busy() || self.driver.vm().pending_ask.is_some() || self.menu_len() > 0 {
            self.submit_current(effects);
            return;
        }
        if !self.attachments.is_empty() {
            // A send-now carries text only; the chips keep their place for
            // the next message instead of riding this one silently.
            self.push_notice(
                ToastKind::Info,
                "Attachments wait for the next send.".to_owned(),
            );
        }
        let text = self.composer.lines().join("\n");
        self.driver.interrupt_send(&text, effects);
        self.scroll = 0;
    }

    /// `Ctrl+G`: hands the draft to the running turn without stopping it;
    /// the model reads it before its next step. With no turn running, or an
    /// ask card or menu open, it is `Enter`.
    fn steer_current(&mut self, effects: &mut Vec<Effect>) {
        if self.driver.vm().pending_ask.is_some() || self.menu_len() > 0 {
            self.submit_current(effects);
            return;
        }
        if !self.attachments.is_empty() {
            self.push_notice(
                ToastKind::Warning,
                "Attachments can only go with a normal send.".to_owned(),
            );
            return;
        }
        let text = self.composer.lines().join("\n");
        self.driver.steer_send(&text, effects);
        self.scroll = 0;
    }

    /// Rebuilds the textarea so cursor, undo, and scroll all reset; the
    /// cursor lands at the end of the text.
    pub(super) fn set_composer_text(&mut self, text: &str) {
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

    /// A running turn or compaction.
    pub(super) fn turn_busy(&self) -> bool {
        self.driver.vm().sending || self.driver.vm().compacting
    }

    /// `Ctrl+C`: cancel a running turn first, quit only when idle.
    pub(super) fn interrupt_or_quit(&mut self, effects: &mut Vec<Effect>) {
        if self.turn_busy() {
            self.driver.cancel_chat(effects);
        } else {
            self.quit = true;
        }
    }
}

/// The distinct 1-based choice numbers in `text` (comma or space
/// separated), or `None` when any word is not a choice number.
pub(super) fn choice_numbers(text: &str, choices: usize) -> Option<Vec<usize>> {
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
