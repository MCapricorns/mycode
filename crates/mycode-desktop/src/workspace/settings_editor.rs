//! The settings editor half of the workspace: form entities, provider and
//! backend rows, MCP forms, subagent routes, the platform shell, and the
//! save pipeline they all feed.
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_app::BridgeCommand;

use crate::ui::{BackendForm, McpForm, ProviderForm, build_mcp_server};
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

impl Workspace {
    pub(crate) fn settings_ua_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.ua_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(mycode_config::default_user_agent())
            });
            cx.subscribe_in(
                &input,
                window,
                |workspace, entity, event, _, cx| match event {
                    InputEvent::Change => {
                        let text = entity.read(cx).value().to_string();
                        workspace.apply_action(DesktopAction::SettingsUserAgentChanged(text), cx);
                    }
                    InputEvent::Blur => workspace.flush_settings_text_save(cx),
                    InputEvent::Focus | InputEvent::PressEnter { .. } => {}
                },
            )
            .detach();
            self.ua_input = Some(input);
        }
        let input = self.ua_input.clone().expect("ua input");
        if self.ua_sync_pending {
            let target = self
                .vm
                .settings
                .as_ref()
                .map(|settings| settings.user_agent.clone())
                .unwrap_or_default();
            let current = input.read(cx).value().to_string();
            if current != target {
                input.update(cx, |state, cx| state.set_value(target, window, cx));
            }
            self.ua_sync_pending = false;
        }
        input
    }

    pub(crate) fn provider_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ProviderForm> {
        self.provider_form
            .get_or_insert_with(|| ProviderForm::new(window, cx))
            .clone()
    }

    pub(crate) fn on_add_provider(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.provider_form.clone() else {
            return;
        };
        let id = form.read(cx).id.read(cx).value().trim().to_string();
        let kind = form.read(cx).kind.clone();
        let base_url = form.read(cx).base_url.read(cx).value().trim().to_string();
        let model = form.read(cx).model.read(cx).value().trim().to_string();
        let api_key = form.read(cx).api_key.read(cx).value().trim().to_string();
        let context_limit = super::parse_token_field(&form.read(cx).context_limit.read(cx).value());
        let max_output = super::parse_token_field(&form.read(cx).max_output.read(cx).value());
        if id.is_empty() || base_url.is_empty() || model.is_empty() {
            self.apply_action(
                DesktopAction::Failed("fill id, base URL, and model".to_owned()),
                cx,
            );
            return;
        }
        let (context_limit, max_output) = match (context_limit, max_output) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                self.apply_action(
                    DesktopAction::Failed(
                        "context window and max output must be plain numbers".to_owned(),
                    ),
                    cx,
                );
                return;
            }
        };
        self.apply_action(
            DesktopAction::SettingsProviderAdded(mycode_config::ProviderSettings {
                id: id.clone(),
                kind,
                base_url,
                models: vec![model],
                enabled: true,
                context_limit,
                max_output,
            }),
            cx,
        );
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: id,
                    api_key,
                },
                cx,
            );
        }
        self.apply_action(
            DesktopAction::ShowModelsSubview(crate::view_model::ModelsSubview::List),
            cx,
        );
    }

    pub(crate) fn on_remove_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        let removed = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.providers.get(index))
            .map(|provider| provider.id.clone());
        self.apply_action(DesktopAction::SettingsProviderRemoved(index), cx);
        if removed.is_some_and(|id| self.vm.provider_detail.as_deref() == Some(id.as_str())) {
            self.apply_action(DesktopAction::ProviderDetailOpened(None), cx);
        }
    }

    /// Search box for the shared model picker. Created once; the value is
    /// cleared when a provider step opens.
    pub(crate) fn model_picker_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.model_picker_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::t("Filter by name or id", "按名称或 id 筛选"))
            });
            cx.subscribe_in(
                &input,
                window,
                |workspace, entity, event, _, cx| match event {
                    InputEvent::Change => {
                        let text = entity.read(cx).value().to_string();
                        workspace.apply_action(DesktopAction::PickerQueryChanged(text), cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        let text = entity.read(cx).value().to_string();
                        workspace.apply_action(DesktopAction::PickerQueryChanged(text), cx);
                        workspace.accept_model_search(cx);
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            )
            .detach();
            self.model_picker_input = Some(input);
        }
        self.model_picker_input.clone().expect("model picker input")
    }

    /// Top-bar filter for settings navigation.
    pub(crate) fn settings_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.settings_search_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::t("Search settings", "搜索设置"))
            });
            cx.subscribe_in(&input, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(DesktopAction::SettingsQueryChanged(text), cx);
                }
            })
            .detach();
            self.settings_search_input = Some(input);
        }
        self.settings_search_input
            .clone()
            .expect("settings search input")
    }

    /// Filter for the always-visible preset model checklist.
    pub(crate) fn preset_model_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.preset_model_search_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(crate::i18n::t("Filter models", "筛选模型"))
            });
            cx.subscribe_in(&input, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(DesktopAction::PresetModelQueryChanged(text), cx);
                }
            })
            .detach();
            self.preset_model_search_input = Some(input);
        }
        self.preset_model_search_input
            .clone()
            .expect("preset model search input")
    }

    pub(crate) fn provider_key_input(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.provider_key_inputs
            .entry(id.to_owned())
            .or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("paste API key here"))
            })
            .clone()
    }

    pub(crate) fn provider_key_replacing(&self, id: &str) -> bool {
        self.provider_key_replace.contains(id)
    }

    /// Opens an empty field so a stored provider key can be replaced.
    pub(crate) fn on_replace_provider_key(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.provider_key_inputs.get(id).cloned() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.provider_key_replace.insert(id.to_owned());
        cx.notify();
    }

    pub(crate) fn on_save_provider_key(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.provider_key_inputs.get(id).cloned() else {
            return;
        };
        let api_key = mycode_config::normalize_api_key(&input.read(cx).value());
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.provider_key_replace.remove(id);
        self.dispatch(
            BridgeCommand::SaveProviderKey {
                provider_id: id.to_owned(),
                api_key,
            },
            cx,
        );
    }

    /// Endpoint field for one provider. The stored base URL is the only
    /// value ever written into it.
    pub(crate) fn provider_endpoint_input(
        &mut self,
        id: &str,
        base_url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if !self.provider_endpoint_inputs.contains_key(id) {
            let seeded = base_url.to_owned();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("https://"));
            input.update(cx, |state, cx| state.set_value(seeded.clone(), window, cx));
            let provider_id = id.to_owned();
            cx.subscribe_in(&input, window, move |workspace, _, event, window, cx| {
                if matches!(event, InputEvent::Blur) {
                    workspace.on_apply_provider_endpoint(&provider_id, window, cx);
                }
            })
            .detach();
            self.provider_endpoint_inputs.insert(id.to_owned(), input);
            self.provider_endpoint_seed.insert(id.to_owned(), seeded);
        }
        let input = self
            .provider_endpoint_inputs
            .get(id)
            .cloned()
            .expect("endpoint input");
        let seed = self
            .provider_endpoint_seed
            .get(id)
            .cloned()
            .unwrap_or_default();
        let current = input.read(cx).value().to_string();
        if current == seed && seed != base_url {
            let next = base_url.to_owned();
            input.update(cx, |state, cx| state.set_value(next.clone(), window, cx));
            self.provider_endpoint_seed.insert(id.to_owned(), next);
        }
        input
    }

    /// Commits an endpoint edit after the same document validation the settings
    /// save runs, then writes `settings.json`. The key vault is not touched.
    pub(crate) fn on_apply_provider_endpoint(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.provider_endpoint_inputs.get(id).cloned() else {
            return;
        };
        let typed = input.read(cx).value().to_string();
        let Some(settings) = self.vm.settings.clone() else {
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
        match settings.preview_provider_base_url(id, &typed) {
            Ok(base_url) => {
                input.update(cx, |state, cx| {
                    state.set_value(base_url.clone(), window, cx)
                });
                self.provider_endpoint_seed
                    .insert(id.to_owned(), base_url.clone());
                self.apply_action(
                    DesktopAction::SettingsProviderBaseUrlChanged {
                        id: id.to_owned(),
                        base_url,
                    },
                    cx,
                );
            }
            Err(message) => {
                self.apply_action(DesktopAction::Failed(message), cx);
            }
        }
    }

    /// Device-code sign-in for a provider that is already configured.
    ///
    /// Does not open the add-from-catalog form.
    pub(crate) fn on_start_provider_oauth(&mut self, provider_id: &str, cx: &mut Context<Self>) {
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
        self.dispatch(
            BridgeCommand::StartOAuthSignIn {
                provider_id: provider_id.to_owned(),
                models,
            },
            cx,
        );
    }

    // ---- provider presets from the catalog ----

    /// The filter input for the provider preset picker.
    pub(crate) fn preset_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.preset_search_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::t("Filter providers…", "筛选服务商…"))
            });
            cx.subscribe_in(&input, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(DesktopAction::PresetSearchChanged(text), cx);
                }
            })
            .detach();
            self.preset_search_input = Some(input);
        }
        self.preset_search_input
            .clone()
            .expect("preset search input")
    }

    pub(crate) fn on_open_preset(
        &mut self,
        provider_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preset_key_input = None;
        if let Some(input) = self.preset_model_search_input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.apply_action(
            DesktopAction::ActivePresetChanged(Some(provider_id.to_owned())),
            cx,
        );
    }

    pub(crate) fn on_close_preset(&mut self, cx: &mut Context<Self>) {
        self.preset_key_input = None;
        self.apply_action(DesktopAction::ActivePresetChanged(None), cx);
    }

    /// Starts an OAuth device-flow sign-in with the checked model list.
    pub(crate) fn on_start_oauth_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(provider_id) = self.vm.active_preset.clone() else {
            return;
        };
        let models = self.vm.preset_models.clone();
        self.dispatch(
            BridgeCommand::StartOAuthSignIn {
                provider_id,
                models,
            },
            cx,
        );
    }

    pub(crate) fn preset_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.preset_key_input
            .get_or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("paste API key here"))
            })
            .clone()
    }

    /// Adds one catalog preset: provider entry plus stored key, then saves.
    pub(crate) fn on_add_preset(&mut self, provider_id: &str, cx: &mut Context<Self>) {
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
        // Unique id: the catalog spelling, suffixed when already configured.
        let mut id = preset.id.clone();
        let mut suffix = 2;
        while self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.providers.iter().any(|p| p.id == id))
        {
            id = format!("{}-{suffix}", preset.id);
            suffix += 1;
        }
        let api_key = self
            .preset_key_input
            .clone()
            .map(|input| input.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        // Drop the input entity so the pasted key never lingers on screen.
        self.preset_key_input = None;
        self.apply_action(
            DesktopAction::SettingsProviderAdded(mycode_config::ProviderSettings {
                id: id.clone(),
                kind: preset.kind.clone(),
                base_url: preset.base_url.clone(),
                models,
                enabled: true,
                context_limit: None,
                max_output: None,
            }),
            cx,
        );
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: id.clone(),
                    api_key,
                },
                cx,
            );
        }
        self.apply_action(DesktopAction::ActivePresetChanged(None), cx);
        self.apply_action(DesktopAction::ProviderSelected(id.clone()), cx);
        self.apply_action(DesktopAction::ModelSelected(model), cx);
        self.on_save_settings(cx);
        self.persist_ui_state(cx);
    }

    // ---- web search backends ----

    pub(crate) fn backend_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<BackendForm> {
        self.backend_form
            .get_or_insert_with(|| BackendForm::new(window, cx))
            .clone()
    }

    pub(crate) fn on_add_backend(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.backend_form.clone() else {
            return;
        };
        let id = form.read(cx).id.read(cx).value().trim().to_string();
        let kind = form.read(cx).kind.read(cx).value().trim().to_string();
        let endpoint = form.read(cx).endpoint.read(cx).value().trim().to_string();
        if id.is_empty() || kind.is_empty() || endpoint.is_empty() {
            self.apply_action(
                DesktopAction::Failed("fill id, kind, and endpoint".to_owned()),
                cx,
            );
            return;
        }
        self.apply_action(
            DesktopAction::SettingsBackendAdded(mycode_config::WebBackendSettings {
                id,
                kind,
                endpoint,
                enabled: false,
            }),
            cx,
        );
        self.on_show_web_subview(crate::view_model::WebSubview::List, cx);
    }

    pub(crate) fn on_remove_backend(&mut self, index: usize, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::SettingsBackendRemoved(index), cx);
    }

    pub(crate) fn web_key_input(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.web_key_inputs
            .entry(id.to_owned())
            .or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder("paste API key (Bearer is added)")
                })
            })
            .clone()
    }

    pub(crate) fn on_save_web_key(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.web_key_inputs.get(id).cloned() else {
            return;
        };
        let api_key = mycode_config::normalize_api_key(&input.read(cx).value());
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.web_key_replace.remove(id);
        self.dispatch(
            BridgeCommand::SaveProviderKey {
                provider_id: format!("web-{id}"),
                api_key,
            },
            cx,
        );
    }

    pub(crate) fn web_key_replacing(&self, id: &str) -> bool {
        self.web_key_replace.contains(id)
    }

    /// Opens an empty field so a stored web key can be replaced. The stored
    /// secret is never copied into the field.
    pub(crate) fn on_replace_web_key(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.web_key_inputs.get(id).cloned() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.web_key_replace.insert(id.to_owned());
        cx.notify();
    }

    // ---- MCP servers ----

    pub(crate) fn mcp_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<McpForm> {
        self.mcp_form
            .get_or_insert_with(|| McpForm::new(window, cx))
            .clone()
    }

    pub(crate) fn mcp_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.mcp_key_input
            .get_or_insert_with(|| {
                cx.new(|cx| InputState::new(window, cx).placeholder("paste API key here"))
            })
            .clone()
    }

    /// Adds the server the custom form currently describes: the pure builder
    /// next to [`McpForm`] validates the fields, this method dispatches.
    pub(crate) fn on_add_mcp(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.mcp_form.clone() else {
            return;
        };
        let (id, transport, endpoint, command, env_line, api_key) = {
            let read = form.read(cx);
            (
                read.id.read(cx).value().trim().to_string(),
                read.transport.clone(),
                read.endpoint.read(cx).value().trim().to_string(),
                read.command.read(cx).value().trim().to_string(),
                read.env.read(cx).value().trim().to_string(),
                read.api_key.read(cx).value().trim().to_string(),
            )
        };
        if id.is_empty() {
            self.apply_action(DesktopAction::Failed("fill id".to_owned()), cx);
            return;
        }
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.mcp_servers.iter().any(|server| server.id == id))
        {
            self.apply_action(
                DesktopAction::Failed(format!("an MCP server named '{id}' already exists")),
                cx,
            );
            return;
        }
        let server = match build_mcp_server(&id, &transport, &endpoint, &command, &env_line) {
            Ok(server) => server,
            Err(message) => {
                self.apply_action(DesktopAction::Failed(message), cx);
                return;
            }
        };
        self.apply_action(DesktopAction::SettingsMcpAdded(server), cx);
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{id}"),
                    api_key,
                },
                cx,
            );
        }
        self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
    }

    pub(crate) fn mcp_json_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        self.mcp_json_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder("Paste mcp.json or a Claude Desktop / Cursor config…")
                        .auto_grow(4, 16)
                })
            })
            .clone()
    }

    pub(crate) fn on_import_mcp_json(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.mcp_json_input.clone() else {
            return;
        };
        let raw = input.read(cx).value().to_string();
        let mut added = 0usize;
        let imported = match mycode_config::parse_mcp_import(&raw) {
            Ok(imported) => imported,
            Err(message) => {
                self.apply_action(DesktopAction::Failed(message), cx);
                return;
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
        for row in imported {
            if existing.contains(&row.server.id) {
                self.apply_action(
                    DesktopAction::Failed(format!(
                        "an MCP server named '{}' already exists",
                        row.server.id
                    )),
                    cx,
                );
                continue;
            }
            let server_id = row.server.id.clone();
            existing.push(server_id.clone());
            let api_key = row.api_key.clone();
            self.apply_action(DesktopAction::SettingsMcpAdded(row.server), cx);
            added += 1;
            if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
                self.dispatch(
                    BridgeCommand::SaveProviderKey {
                        provider_id: format!("mcp-{server_id}"),
                        api_key,
                    },
                    cx,
                );
            }
        }
        if added > 0 {
            self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
        }
    }

    // ---- subagent routes ----

    pub(crate) fn on_subagent_role_enabled(
        &mut self,
        role: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        next.role_mut(role).enabled = enabled;
        self.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
    }

    pub(crate) fn on_toggle_subagent_menu(
        &mut self,
        role: &str,
        field: &str,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        let next = open.then(|| (role.to_owned(), field.to_owned()));
        self.apply_action(DesktopAction::SubagentMenuToggled(next), cx);
    }

    pub(crate) fn on_set_subagent_thinking(
        &mut self,
        role: &str,
        thinking: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut next = settings.subagents.clone();
        let entry = next.role_mut(role);
        entry.thinking = thinking.filter(|level| level != "inherit" && level != "default");
        self.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
    }

    pub(crate) fn on_set_subagent_route(
        &mut self,
        role: &str,
        provider: Option<String>,
        model: Option<String>,
        cx: &mut Context<Self>,
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
        self.apply_action(DesktopAction::SettingsSubagentsChanged(next), cx);
    }

    // ---- platform shell ----

    pub(crate) fn on_detect_shell(&mut self, cx: &mut Context<Self>) {
        let Some(detected) = mycode_tools::detect_default_shell() else {
            self.apply_action(
                DesktopAction::Failed(
                    "No usable shell was found. Browse to pwsh or Git bash.".to_owned(),
                ),
                cx,
            );
            return;
        };
        self.set_shell_preference(
            detected.kind.as_str(),
            &detected.program.to_string_lossy(),
            "auto",
            cx,
        );
        self.on_save_settings(cx);
    }

    pub(crate) fn on_browse_shell(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a shell executable".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first() else {
                return;
            };
            let program = path.to_string_lossy().into_owned();
            let kind = mycode_tools::ShellKind::from_program(path);
            let _ = this.update(cx, |workspace, cx| {
                let stem = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !matches!(stem.as_str(), "pwsh" | "bash" | "sh") {
                    workspace.apply_action(
                        DesktopAction::Failed(
                            "Unsupported shell: only pwsh and bash are supported.".to_owned(),
                        ),
                        cx,
                    );
                    return;
                }
                workspace.set_shell_preference(kind.as_str(), &program, "user", cx);
                workspace.on_save_settings(cx);
            });
        })
        .detach();
    }

    pub(crate) fn on_set_shell_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let current = settings.tools.shell.clone().unwrap_or_default();
        if current.kind == kind && !current.program.is_empty() {
            return;
        }
        if let Some(detected) =
            mycode_tools::ShellKind::parse(kind).and_then(mycode_tools::detect_shell_kind)
        {
            self.set_shell_preference(kind, &detected.program.to_string_lossy(), "user", cx);
        } else if !current.program.is_empty()
            && mycode_tools::ShellKind::from_program(std::path::Path::new(&current.program))
                .as_str()
                == kind
        {
            self.set_shell_preference(kind, &current.program, "user", cx);
        } else {
            self.apply_action(
                DesktopAction::Failed(format!(
                    "No {kind} executable was found. Use Browse to pick one."
                )),
                cx,
            );
        }
        self.on_save_settings(cx);
    }

    fn set_shell_preference(
        &mut self,
        kind: &str,
        program: &str,
        source: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let mut tools = settings.tools.clone();
        tools.shell = Some(mycode_config::ShellSettings {
            kind: kind.to_owned(),
            program: program.to_owned(),
            source: source.to_owned(),
        });
        self.apply_action(DesktopAction::SettingsToolsChanged(tools), cx);
        self.apply_runtime_shell();
    }

    /// Pushes the configured shell into the tool runtime's process-global
    /// slot so newly spawned tool children use it.
    pub(super) fn apply_runtime_shell(&mut self) {
        let shell = self.vm.settings.as_ref().and_then(|settings| {
            let configured = settings.tools.shell.as_ref()?;
            let program = configured.program.trim();
            if program.is_empty() {
                return None;
            }
            let kind = mycode_tools::ShellKind::parse(&configured.kind).unwrap_or_else(|| {
                mycode_tools::ShellKind::from_program(std::path::Path::new(program))
            });
            Some(mycode_tools::DetectedShell {
                kind,
                program: std::path::PathBuf::from(program),
            })
        });
        mycode_tools::set_runtime_shell(shell);
    }

    /// Restores each chat's working directory after startup so a later
    /// session switch does not run tools in the last-opened project.
    pub(super) fn restore_session_projects(&mut self, cx: &mut Context<Self>) {
        let bindings = self.vm.session_projects.clone();
        for (session_id, project) in bindings {
            self.dispatch(
                BridgeCommand::SetProjectDir {
                    session_id,
                    path: Some(project),
                },
                cx,
            );
        }
    }

    /// Waits out the text-field debounce, then writes `settings.json`.
    const SETTINGS_TEXT_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

    /// Writes a text field such as User-Agent about 400ms after the last
    /// keystroke. A newer edit or a blur supersedes this timer.
    pub(crate) fn schedule_settings_text_save(&mut self, cx: &mut Context<Self>) {
        self.settings_text_save_generation = self.settings_text_save_generation.wrapping_add(1);
        self.settings_text_save_pending = true;
        let generation = self.settings_text_save_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Self::SETTINGS_TEXT_SAVE_DEBOUNCE)
                .await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.settings_text_save_generation != generation {
                    return;
                }
                workspace.settings_text_save_pending = false;
                workspace.on_save_settings(cx);
            });
        })
        .detach();
    }

    /// Writes a pending text-field edit immediately. Blur and leaving
    /// Settings use this so the draft is not waiting on the debounce.
    pub(crate) fn flush_settings_text_save(&mut self, cx: &mut Context<Self>) {
        if !self.settings_text_save_pending {
            return;
        }
        self.settings_text_save_generation = self.settings_text_save_generation.wrapping_add(1);
        self.settings_text_save_pending = false;
        self.on_save_settings(cx);
    }

    /// Writes a settings edit that arrived while a save was in flight.
    /// A pending text debounce keeps its timer so a half-typed User-Agent
    /// is not flushed early.
    pub(crate) fn continue_settings_save(&mut self, cx: &mut Context<Self>) {
        if self.settings_text_save_pending {
            return;
        }
        self.on_save_settings(cx);
    }

    /// Persists the settings document under CAS when local edits exist.
    pub(crate) fn on_save_settings(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.vm.settings.clone() else {
            return;
        };
        if settings.saving || !settings.dirty {
            return;
        }
        let document = settings.to_settings();
        if let Err(message) = document.validate() {
            self.apply_action(
                DesktopAction::SettingsSaveFailed(format!("invalid settings: {message}")),
                cx,
            );
            return;
        }
        self.settings_save_epoch = settings.edit_epoch;
        self.vm.settings.as_mut().expect("settings").saving = true;
        let revision = mycode_config::AuthorityRevision::new(settings.revision)
            .unwrap_or(mycode_config::AuthorityRevision::ABSENT);
        cx.notify();
        self.dispatch(
            BridgeCommand::SaveSettings {
                expected_revision: revision,
                settings: document,
            },
            cx,
        );
    }
}
