//! The settings editor's product half: the debounced save pipeline, provider
//! and MCP rows, key-vault writes, OAuth sign-in starts, subagent routes,
//! and the appearance/language picks. Input widgets stay in the frontend;
//! every decision about what reaches `settings.json` or the key store is
//! made here.
use mycode_app::BridgeCommand;
use mycode_config::{AuthorityRevision, ProxyPreference};

use super::{DriverMsg, Effect, FrontendDriver, SETTINGS_TEXT_SAVE_DEBOUNCE, ToastKind};
use crate::forms;
use crate::view_model::{FrontendAction, McpSubview, ModelsSubview};

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
    /// A pending text debounce keeps its timer so a half-typed User-Agent
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

    /// Picks the proxy mode: `auto`, `direct`, or `custom`. Custom keeps a
    /// stored URL, or starts from Clash's default port.
    pub fn select_proxy_mode(&mut self, mode: &str, out: &mut Vec<Effect>) {
        let current = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.proxy.clone())
            .unwrap_or_default();
        let value = match mode {
            "direct" => mycode_config::PROXY_DIRECT.to_owned(),
            "custom" => match ProxyPreference::parse(&current) {
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

    // ---- web search backends ----

    /// The row's enable switch. Disabling is immediate; enabling first runs
    /// a key-resolving search probe against the row, landing `on` only when
    /// the probe returns `Ok`.
    pub fn toggle_web_backend(&mut self, index: usize, enabled: bool, out: &mut Vec<Effect>) {
        let Some(backend) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.web_backends.get(index))
            .cloned()
        else {
            return;
        };
        if !enabled {
            self.pending_web_enable.remove(&backend.id);
            self.act(FrontendAction::SettingsBackendToggled(index, false), out);
            return;
        }
        if self.pending_web_enable.contains(&backend.id) {
            return;
        }
        self.pending_web_enable.insert(backend.id.clone());
        self.dispatch(BridgeCommand::ProbeWebBackend {
            backend: Box::new(backend),
        });
    }

    /// Saves one provider's, web backend's, or MCP server's API key after
    /// normalization. `target` is the key-store id: the provider id itself,
    /// `web-{id}`, or `mcp-{id}`. `false` refuses a blank/`bearer` field so
    /// the replace flow cannot delete a stored credential.
    pub fn save_key(&mut self, target: String, raw_key: &str) -> bool {
        let Some(api_key) = forms::normalized_key_input(raw_key) else {
            return false;
        };
        self.dispatch(BridgeCommand::SaveProviderKey {
            provider_id: target,
            api_key,
        });
        true
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
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.iter().find(|s| s.id == server_id))
            .cloned()
        else {
            self.pending_mcp_enable.remove(server_id);
            return;
        };
        self.act(FrontendAction::McpProbeStarted(server_id.to_owned()), out);
        self.dispatch(BridgeCommand::McpListTools {
            server: Box::new(server),
        });
    }

    /// Probes one MCP server as the editor currently shows it (saved or
    /// not) and marks the row as connecting until the reply lands.
    pub fn list_mcp_tools(&mut self, server_id: &str, out: &mut Vec<Effect>) {
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.iter().find(|s| s.id == server_id))
            .cloned()
        else {
            return;
        };
        self.act(FrontendAction::McpProbeStarted(server_id.to_owned()), out);
        self.dispatch(BridgeCommand::McpListTools {
            server: Box::new(server),
        });
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
            self.act(FrontendAction::Failed("fill id".to_owned()), out);
            return false;
        }
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.id == id))
        {
            self.act(
                FrontendAction::Failed(format!("an MCP server named '{id}' already exists")),
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
        let imported = match mycode_config::parse_mcp_import(raw) {
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
                    "MCP servers already exist: {}",
                    duplicates.join(", ")
                )),
                out,
            );
        }
        true
    }

    /// Adds a catalog-listed server row (starts disabled; the key save arms
    /// the enable probe so a pasted key ends in a verified enable).
    pub fn add_builtin_mcp(
        &mut self,
        mut server: mycode_config::McpServerSettings,
        api_key: &str,
        out: &mut Vec<Effect>,
    ) {
        let server_id = server.id.clone();
        // New rows start disabled: the enable probe is armed by the key
        // save that follows, so a pasted key ends in a verified enable.
        server.enabled = false;
        self.act(FrontendAction::SettingsMcpAdded(server), out);
        if !api_key.is_empty() {
            self.pending_mcp_enable.insert(server_id.clone());
            self.dispatch(BridgeCommand::SaveProviderKey {
                provider_id: format!("mcp-{server_id}"),
                api_key: api_key.to_owned(),
            });
        }
        self.act(FrontendAction::ShowMcpSubview(McpSubview::List), out);
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

    // ---- providers ----

    /// Adds the provider the custom form describes from its raw field
    /// values: [`forms::build_custom_provider`] validates and refuses a
    /// duplicate id before anything is dispatched. `true` when the row was
    /// added, so the frontend can clear its form.
    #[allow(clippy::too_many_arguments)]
    pub fn add_custom_provider(
        &mut self,
        id: &str,
        kind: &str,
        base_url: &str,
        model: &str,
        api_key: &str,
        context_limit: &str,
        max_output: &str,
        out: &mut Vec<Effect>,
    ) -> bool {
        let existing: Vec<String> = self
            .vm
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .providers
                    .iter()
                    .map(|provider| provider.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let provider = match forms::build_custom_provider(
            &existing,
            id,
            kind,
            base_url,
            model,
            context_limit,
            max_output,
        ) {
            Ok(provider) => provider,
            Err(message) => {
                self.act(FrontendAction::Failed(message), out);
                return false;
            }
        };
        let provider_id = provider.id.clone();
        self.act(FrontendAction::SettingsProviderAdded(provider), out);
        if let Some(api_key) = forms::normalized_key_input(api_key) {
            self.dispatch(BridgeCommand::SaveProviderKey {
                provider_id,
                api_key,
            });
        }
        self.act(FrontendAction::ShowModelsSubview(ModelsSubview::List), out);
        true
    }

    /// Removes one provider row and closes its detail page when open.
    pub fn remove_provider(&mut self, index: usize, out: &mut Vec<Effect>) {
        let removed = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.providers.get(index))
            .map(|provider| provider.id.clone());
        self.act(FrontendAction::SettingsProviderRemoved(index), out);
        if removed.is_some_and(|id| self.vm.provider_detail.as_deref() == Some(id.as_str())) {
            self.act(FrontendAction::ProviderDetailOpened(None), out);
        }
    }

    /// Commits an endpoint edit after the same document validation the
    /// settings save runs. The key vault is not touched. `typed` is the
    /// field's text; an unchanged or invalid value leaves the row alone.
    pub fn apply_provider_endpoint(&mut self, id: &str, typed: &str, out: &mut Vec<Effect>) {
        let previewed = {
            let Some(settings) = self.vm.settings.as_ref() else {
                return;
            };
            let Some(current) = settings
                .providers
                .iter()
                .find(|provider| provider.id == id)
                .map(|provider| provider.base_url.clone())
            else {
                return;
            };
            if typed.trim() == current {
                return;
            }
            settings.preview_provider_base_url(id, typed)
        };
        match previewed {
            Ok(base_url) => {
                self.act(
                    FrontendAction::SettingsProviderBaseUrlChanged {
                        id: id.to_owned(),
                        base_url,
                    },
                    out,
                );
            }
            Err(message) => {
                self.act(FrontendAction::Failed(message), out);
            }
        }
    }

    /// Device-code sign-in for a provider that is already configured.
    /// Does not open the add-from-catalog form.
    pub fn start_provider_oauth(&mut self, provider_id: &str) {
        let models = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| {
                settings
                    .providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
            })
            .map(|provider| provider.models.clone())
            .unwrap_or_default();
        self.dispatch(BridgeCommand::StartOAuthSignIn {
            provider_id: provider_id.to_owned(),
            models,
        });
    }

    /// Starts an OAuth device-flow sign-in from the shared page, binding
    /// the catalog row's models (or the configured subset when the row is
    /// already in settings).
    pub fn start_oauth_sign_in(&mut self, provider_id: String, out: &mut Vec<Effect>) {
        let models = crate::view_model::preset_pick(&self.vm, &provider_id);
        if models.is_empty() {
            self.act(
                FrontendAction::Failed(
                    crate::i18n::t(
                        "Provider catalog is not loaded yet; refresh it in About and try again.",
                        "服务商目录尚未就绪，可在「关于」页刷新目录后重试。",
                    )
                    .to_owned(),
                ),
                out,
            );
            return;
        }
        self.dispatch(BridgeCommand::StartOAuthSignIn {
            provider_id,
            models,
        });
    }

    /// Adds one catalog preset: provider entry plus stored key, then saves.
    /// `api_key` was already normalized by the caller's field handling.
    pub fn add_preset_provider(
        &mut self,
        provider_id: &str,
        api_key: Option<String>,
        out: &mut Vec<Effect>,
    ) {
        let Some(catalog) = self.vm.catalog.clone() else {
            return;
        };
        let Some(preset) = catalog.provider(provider_id) else {
            return;
        };
        // Checked models in catalog order; an empty selection falls back to
        // the catalog's first model.
        let checked = self.vm.preset_models.clone();
        let mut models: Vec<String> = preset
            .models
            .iter()
            .filter(|entry| checked.contains(&entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        if models.is_empty() {
            let Some(first) = preset.models.first() else {
                return;
            };
            models.push(first.id.clone());
        }
        let model = models[0].clone();
        // OpenDesign is one gateway: adding it again updates the configured
        // row (its endpoint and overrides stay). Other presets get a unique
        // id, the catalog spelling suffixed when already configured.
        let opendesign = preset.id == mycode_providers::catalog::opendesign::PROVIDER_ID;
        let existing = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.providers.iter().find(|p| p.id == preset.id))
            .filter(|_| opendesign)
            .cloned();
        let mut id = preset.id.clone();
        let mut suffix = 2;
        while existing.is_none()
            && self
                .vm
                .settings
                .as_ref()
                .is_some_and(|settings| settings.providers.iter().any(|p| p.id == id))
        {
            id = format!("{}-{suffix}", preset.id);
            suffix += 1;
        }
        let row = match existing {
            Some(existing) => mycode_config::ProviderSettings {
                models,
                enabled: true,
                ..existing
            },
            None => mycode_config::ProviderSettings {
                id: id.clone(),
                kind: preset.kind.clone(),
                base_url: preset.base_url.clone(),
                models,
                enabled: true,
                context_limit: None,
                max_output: None,
                hidden_models: Vec::new(),
            },
        };
        self.act(FrontendAction::SettingsProviderAdded(row), out);
        if let Some(api_key) = api_key {
            self.dispatch(BridgeCommand::SaveProviderKey {
                provider_id: id.clone(),
                api_key,
            });
        }
        self.act(FrontendAction::ActivePresetChanged(None), out);
        self.act(FrontendAction::ProviderSelected(id), out);
        self.act(FrontendAction::ModelSelected(model), out);
        self.save_settings(out);
        self.persist_ui_state();
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
