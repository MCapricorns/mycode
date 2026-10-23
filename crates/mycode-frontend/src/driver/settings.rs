//! The settings editor's product half: the debounced save pipeline, MCP
//! rows, key-vault writes, subagent routes, and the appearance/language
//! picks. Input widgets stay in the frontend; every decision about what
//! reaches `settings.json` or the key store is made here.
use mycode_app::BridgeCommand;
use mycode_app::{AuthorityRevision, ProxyPreference};

use super::{DriverMsg, Effect, FrontendDriver, SETTINGS_TEXT_SAVE_DEBOUNCE, ToastKind};
use crate::forms;
use crate::view_model::{FrontendAction, McpSubview, ProxyTestState};

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

    /// The row's enable switch. Disabling is immediate; enabling first
    /// verifies the stored key (for HTTP rows that declare auth) and a live
    /// tools probe, landing `on` only when the probe returns tools.
    pub fn toggle_mcp_server(&mut self, index: usize, enabled: bool, out: &mut Vec<Effect>) {
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.get(index))
            .cloned()
        else {
            return;
        };
        if !enabled {
            self.pending_mcp_enable.remove(&server.id);
            self.act(FrontendAction::SettingsMcpToggled(index, false), out);
            return;
        }
        if self.pending_mcp_enable.contains(&server.id) || self.vm.mcp_probing.contains(&server.id)
        {
            return;
        }
        // A row that declares a credential header needs its `mcp-<id>` key
        // before the probe can mean anything.
        let needs_key = server.transport == "http" && server.key_header.is_some();
        let keyed = self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.mcp_with_keys.iter().any(|id| id == &server.id));
        if needs_key && !keyed {
            self.toast(
                crate::i18n::t(
                    "Store an API key for this server first",
                    "请先为该服务器保存 API 密钥",
                ),
                ToastKind::Error,
                out,
            );
            return;
        }
        self.pending_mcp_enable.insert(server.id.clone());
        self.probe_mcp_for_enable(&server.id, out);
    }

    /// Dispatches the tools probe that gates an enable switch: the row is
    /// marked connecting until the reply lands `enabled` or the error.
    pub fn probe_mcp_for_enable(&mut self, server_id: &str, out: &mut Vec<Effect>) {
        if !self.dispatch_mcp_probe(server_id, out) {
            self.pending_mcp_enable.remove(server_id);
        }
    }

    /// Probes one MCP server as the editor currently shows it (saved or
    /// not) and marks the row as connecting until the reply lands.
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

    /// Adds the server the custom form describes from its raw field values:
    /// validation lives in [`forms::build_mcp_server`], this decides.
    /// `true` when the row was added, so the frontend can clear its form.
    #[allow(clippy::too_many_arguments)]
    pub fn add_custom_mcp(
        &mut self,
        id: &str,
        transport: &str,
        endpoint: &str,
        command: &str,
        env_line: &str,
        api_key: &str,
        out: &mut Vec<Effect>,
    ) -> bool {
        let id = id.trim();
        if id.is_empty() {
            self.act(
                FrontendAction::Failed(
                    crate::i18n::t("Fill in the server id.", "请填写服务器 ID。").to_owned(),
                ),
                out,
            );
            return false;
        }
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.id == id))
        {
            self.act(
                FrontendAction::Failed(format!(
                    "{} '{id}'",
                    crate::i18n::t(
                        "An MCP server already uses the id",
                        "已有 MCP 服务器使用此 ID："
                    )
                )),
                out,
            );
            return false;
        }
        let server = match forms::build_mcp_server(id, transport, endpoint, command, env_line) {
            Ok(server) => server,
            Err(message) => {
                self.act(FrontendAction::Failed(message), out);
                return false;
            }
        };
        self.act(FrontendAction::SettingsMcpAdded(server), out);
        if transport == "http"
            && let Some(api_key) = forms::normalized_key_input(api_key)
        {
            self.dispatch(BridgeCommand::SaveProviderKey {
                provider_id: format!("mcp-{id}"),
                api_key,
            });
        }
        self.act(FrontendAction::ShowMcpSubview(McpSubview::List), out);
        true
    }

    /// Imports every server a pasted `mcp.json`-style document describes:
    /// skips ids already configured, saves each row's key, and reports one
    /// aggregated duplicate message instead of overwriting per-row errors.
    /// `true` once the document parsed (even if every row was a duplicate),
    /// so the frontend can clear the paste box.
    pub fn import_mcp_json(&mut self, raw: &str, out: &mut Vec<Effect>) -> bool {
        let mut added = 0usize;
        let imported = match mycode_app::parse_mcp_import(raw) {
            Ok(imported) => imported,
            Err(message) => {
                self.act(FrontendAction::Failed(message), out);
                return false;
            }
        };
        let mut existing: Vec<String> = self
            .vm
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .mcp_servers
                    .iter()
                    .map(|server| server.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut duplicates: Vec<String> = Vec::new();
        for row in imported {
            if existing.contains(&row.server.id) {
                duplicates.push(row.server.id.clone());
                continue;
            }
            let server_id = row.server.id.clone();
            existing.push(server_id.clone());
            self.act(FrontendAction::SettingsMcpAdded(row.server), out);
            added += 1;
            if let Some(api_key) = row
                .api_key
                .and_then(|key| forms::normalized_key_input(&key))
            {
                self.dispatch(BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{server_id}"),
                    api_key,
                });
            }
        }
        if added > 0 {
            self.act(FrontendAction::ShowMcpSubview(McpSubview::List), out);
        }
        if !duplicates.is_empty() {
            // One aggregated message: per-row failures overwrote each other,
            // so only the last duplicate name was ever reported.
            self.act(
                FrontendAction::Failed(format!(
                    "{} {}",
                    crate::i18n::t("MCP servers already exist:", "MCP 服务器已存在："),
                    duplicates.join(", ")
                )),
                out,
            );
        }
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
