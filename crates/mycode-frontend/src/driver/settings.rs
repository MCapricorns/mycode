//! The settings editor's product half: the debounced save pipeline, MCP
//! rows, key-vault writes, subagent routes, and the appearance/language
//! picks. Input widgets stay in the frontend; every decision about what
//! reaches `settings.json` or the key store is made here.
use mycode_app::BridgeCommand;
use mycode_app::{AuthorityRevision, ProxyPreference};

use super::{DriverMsg, Effect, FrontendDriver, SETTINGS_TEXT_SAVE_DEBOUNCE};
use crate::forms;
use crate::view_model::{FrontendAction, ProxyTestState};

impl FrontendDriver {
    // ---- the save pipeline ----

    /// Waits out the text-field debounce, then writes `settings.json`.
    pub fn schedule_settings_text_save(&mut self) {
        self.settings_text_save_generation = self.settings_text_save_generation.wrapping_add(1);
        self.settings_text_save_pending = true;
        let generation = self.settings_text_save_generation;
        self.schedule(SETTINGS_TEXT_SAVE_DEBOUNCE, move || {
            DriverMsg::SettingsTextSaveTick { generation }
        });
    }

    /// Writes a pending text-field edit immediately. Blur and leaving
    /// Settings use this so the draft is not waiting on the debounce.
    pub fn flush_settings_text_save(&mut self, out: &mut Vec<Effect>) {
        if !self.settings_text_save_pending {
            return;
        }
        self.settings_text_save_generation = self.settings_text_save_generation.wrapping_add(1);
        self.settings_text_save_pending = false;
        self.save_settings(out);
    }

    /// Writes a settings edit that arrived while a save was in flight.
    /// A pending text debounce keeps its timer so a half-typed proxy URL
    /// is not flushed early.
    pub fn continue_settings_save(&mut self, out: &mut Vec<Effect>) {
        if self.settings_text_save_pending {
            return;
        }
        self.save_settings(out);
    }

    /// Persists the settings document under CAS when local edits exist.
    pub fn save_settings(&mut self, out: &mut Vec<Effect>) {
        let Some((document, edit_epoch, revision)) = self
            .vm
            .settings
            .as_ref()
            .filter(|settings| !settings.saving && settings.dirty)
            .map(|settings| {
                (
                    settings.to_settings(),
                    settings.edit_epoch,
                    settings.revision,
                )
            })
        else {
            return;
        };
        if let Err(message) = document.validate() {
            self.act(
                FrontendAction::SettingsSaveFailed(format!("invalid settings: {message}")),
                out,
            );
            return;
        }
        self.settings_save_epoch = edit_epoch;
        self.vm.settings.as_mut().expect("settings").saving = true;
        let revision = AuthorityRevision::new(revision).unwrap_or(AuthorityRevision::ABSENT);
        self.dispatch(BridgeCommand::SaveSettings {
            expected_revision: revision,
            settings: document,
        });
    }

    // ---- proxy ----

    /// The proxy URL field's text changed. Only a value that parses as a
    /// proxy reaches the settings document, so a half-typed URL never fails
    /// a save.
    pub fn proxy_url_typed(&mut self, text: &str, out: &mut Vec<Effect>) {
        let text = text.trim();
        if matches!(ProxyPreference::parse(text), Some(ProxyPreference::Url(_))) {
            self.act(
                FrontendAction::SettingsProxyChanged {
                    value: text.to_owned(),
                    typed: true,
                },
                out,
            );
        }
    }

    /// The proxy field blurred; a pending debounce flushes now.
    pub fn proxy_url_committed(&mut self, out: &mut Vec<Effect>) {
        self.flush_settings_text_save(out);
    }

    /// Picks the proxy mode: `system` follows the OS, `manual` uses a typed
    /// URL. Manual keeps a stored URL, or starts from Clash's default port.
    pub fn select_proxy_mode(&mut self, mode: &str, out: &mut Vec<Effect>) {
        let current = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.proxy.clone())
            .unwrap_or_default();
        let value = match mode {
            "manual" => match ProxyPreference::parse(&current) {
                Some(ProxyPreference::Url(_)) => return,
                _ => forms::DEFAULT_PROXY_URL.to_owned(),
            },
            _ => String::new(),
        };
        out.push(Effect::SyncProxyField);
        self.act(
            FrontendAction::SettingsProxyChanged {
                value,
                typed: false,
            },
            out,
        );
    }

    /// Checks the proxy value on screen by fetching a probe URL through
    /// it. One test at a time; a value edit resets the result.
    pub fn test_proxy(&mut self, out: &mut Vec<Effect>) {
        if self.vm.proxy_test == ProxyTestState::Testing {
            return;
        }
        let Some(proxy) = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.proxy.clone())
        else {
            return;
        };
        self.act(
            FrontendAction::ProxyTestChanged(ProxyTestState::Testing),
            out,
        );
        self.dispatch(BridgeCommand::TestProxy { proxy });
    }

    // ---- MCP servers ----

    /// Probes one magpie Library server and marks the row as connecting
    /// until the reply lands. The catalog is read-only: enabling and
    /// editing happen in magpie, not here.
    pub fn list_mcp_tools(&mut self, server_id: &str, out: &mut Vec<Effect>) {
        self.dispatch_mcp_probe(server_id, out);
    }

    /// Sends one tools probe for the row on screen. `false` when the row is
    /// gone.
    fn dispatch_mcp_probe(&mut self, server_id: &str, out: &mut Vec<Effect>) -> bool {
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.iter().find(|s| s.id == server_id))
            .cloned()
        else {
            return false;
        };
        self.act(FrontendAction::McpProbeStarted(server_id.to_owned()), out);
        self.dispatch(BridgeCommand::McpListTools {
            server: Box::new(server),
        });
        true
    }

    // ---- subagent routes ----

    /// One role's enabled switch on the Agents page.
    pub fn toggle_subagent_role(&mut self, role: &str, enabled: bool, out: &mut Vec<Effect>) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        next.role_mut(role).enabled = enabled;
        self.act(FrontendAction::SettingsSubagentsChanged(next), out);
    }

    /// One role's thinking-effort override; `inherit`/`default` clears it.
    pub fn set_subagent_thinking(
        &mut self,
        role: &str,
        thinking: Option<String>,
        out: &mut Vec<Effect>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        entry.thinking = thinking.filter(|level| level != "inherit" && level != "default");
        self.act(FrontendAction::SettingsSubagentsChanged(next), out);
    }

    /// One role's provider/model route: provider-only picks take that
    /// provider's first model, model-only picks take the session's provider
    /// (or the first enabled one), and `inherit` clears the route.
    pub fn set_subagent_route(
        &mut self,
        role: &str,
        provider: Option<String>,
        model: Option<String>,
        out: &mut Vec<Effect>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        if provider.as_deref() == Some("inherit") || model.as_deref() == Some("inherit") {
            entry.provider = None;
            entry.model = None;
        } else if let Some(provider) = provider {
            let model = model.or_else(|| {
                settings
                    .providers
                    .iter()
                    .find(|item| item.id == provider)
                    .and_then(|item| item.models.first().cloned())
            });
            match model {
                Some(model) => {
                    entry.provider = Some(provider);
                    entry.model = Some(model);
                }
                None => {
                    entry.provider = None;
                    entry.model = None;
                }
            }
        } else if let Some(model) = model {
            let fallback = self.vm.selected_provider.clone().or_else(|| {
                settings
                    .providers
                    .iter()
                    .find(|item| item.enabled)
                    .map(|item| item.id.clone())
            });
            match fallback {
                Some(provider) => {
                    entry.provider = Some(provider);
                    entry.model = Some(model);
                }
                None => {
                    entry.provider = None;
                    entry.model = None;
                }
            }
        }
        self.act(FrontendAction::SettingsSubagentsChanged(next), out);
    }

    // ---- appearance & language ----

    /// Applies and persists dark or light.
    pub fn select_theme(&mut self, theme_id: &str, out: &mut Vec<Effect>) {
        let theme_id = crate::theme::normalize_theme(theme_id);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.theme == theme_id)
        {
            return;
        }
        out.push(Effect::AppearanceTheme(theme_id.to_owned()));
        self.act(
            FrontendAction::SettingsThemeSelected(theme_id.to_owned()),
            out,
        );
        self.save_settings(out);
    }

    /// Applies and persists the interface font size. The frontend repaints
    /// the whole window at the new scale on [`Effect::FontSizeChanged`].
    pub fn select_font_size(&mut self, font_size: &str, out: &mut Vec<Effect>) {
        let font_size = crate::theme::normalize_font_size(font_size);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.font_size == font_size)
        {
            return;
        }
        self.act(
            FrontendAction::SettingsFontSizeSelected(font_size.to_owned()),
            out,
        );
        out.push(Effect::FontSizeChanged);
        self.save_settings(out);
    }

    /// Applies and persists the UI language: the process-global strings flip
    /// now, and [`Effect::LanguageChanged`] tells the frontend to rewrite
    /// placeholders captured at widget creation.
    pub fn select_language(&mut self, language: &str, out: &mut Vec<Effect>) {
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.language == language)
        {
            return;
        }
        crate::i18n::apply_language(language);
        self.act(
            FrontendAction::SettingsLanguageSelected(language.to_owned()),
            out,
        );
        out.push(Effect::LanguageChanged);
        self.save_settings(out);
    }
}
