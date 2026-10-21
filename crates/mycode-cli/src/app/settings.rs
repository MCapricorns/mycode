//! The settings page: rows, the cursor, and the floating pickers for model
//! selection, API keys, base URLs, and the manual proxy. All edits go through
//! the shared `FrontendAction`s and save pipeline; this module only keeps the
//! keystroke-level state.

use crossterm::event::{KeyCode, KeyEvent};
use mycode_config::{VALID_FONT_SIZES, VALID_LANGUAGES, VALID_THEMES};
use mycode_frontend::driver::{Effect, ToastKind};
use mycode_frontend::picker;
use mycode_frontend::view_model::{
    FrontendAction, MainView, UpdateState, model_display_name, provider_display_name,
    selected_reasoning_level, selected_reasoning_levels,
};

use super::Tui;
use super::settings_rows::{manual_proxy, settings_items};

/// A picker floating over the settings page.
pub(super) enum SettingsOverlay {
    /// Two-step provider → model pick. `provider=None` is the provider
    /// step; `Some(id)` lists that provider's models.
    Model {
        provider: Option<String>,
        cursor: usize,
        filter: String,
    },
    /// Single-line input for one server's API key (masked).
    ApiKey { provider: String, buffer: String },
    /// Single-line input for the manual proxy URL.
    ProxyUrl { buffer: String },
}

/// One selectable row on the settings page.
#[derive(Clone)]
pub(super) enum SettingItem {
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
    /// MCP server row index; Enter toggles (enable runs the probe), `a`
    /// edits its `mcp-<id>` key.
    Mcp(usize),
    /// Skill row index into `vm.skills`; Enter toggles it.
    Skill(usize),
}

impl Tui {
    /// All key input while `vm.view == Settings`.
    pub(super) fn on_settings_key(
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
            _ => {}
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
    pub(super) fn picker_rows(&self, provider: Option<&str>, filter: &str) -> Vec<PickerRow> {
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
            SettingItem::Mcp(index) => self
                .driver
                .vm()
                .settings
                .as_ref()
                .and_then(|settings| settings.mcp_servers.get(*index))
                .map(|server| format!("mcp-{}", server.id)),
            _ => None,
        };
        if let Some(provider) = target {
            self.settings_overlay = Some(SettingsOverlay::ApiKey {
                provider,
                buffer: String::new(),
            });
        }
    }
}

/// One row of the settings model picker.
#[derive(Clone)]
pub(super) enum PickerRow {
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
    pub(super) fn label(&self) -> &str {
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
