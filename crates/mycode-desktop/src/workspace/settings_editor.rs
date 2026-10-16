//! The settings editor half of the workspace: form entities, provider and
//! backend rows, MCP forms, subagent routes, and the save pipeline they
//! all feed.
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_app::BridgeCommand;

use crate::ui::{
    BackendForm, McpForm, ProviderForm, build_custom_backend, build_custom_provider,
    build_mcp_server,
};
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

/// Trims an API key candidate for UI button enablement and save dispatch.
///
/// Saving an empty or whitespace-only key field is a no-op so the replace-key
/// flow cannot delete a stored credential.
pub(crate) fn normalized_key_input(raw: &str) -> Option<String> {
    let key = mycode_config::normalize_api_key(raw);
    (!key.is_empty() && !key.eq_ignore_ascii_case("bearer")).then_some(key)
}

/// The URL the custom proxy mode starts from: Clash's mixed port.
pub(crate) const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

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

    /// The proxy URL field. Only a value that parses as a proxy reaches the
    /// settings document, so a half-typed URL never fails a save.
    pub(crate) fn settings_proxy_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.proxy_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(DEFAULT_PROXY_URL));
            cx.subscribe_in(
                &input,
                window,
                |workspace, entity, event, _, cx| match event {
                    InputEvent::Change => {
                        let text = entity.read(cx).value().trim().to_owned();
                        if matches!(
                            mycode_config::ProxyPreference::parse(&text),
                            Some(mycode_config::ProxyPreference::Url(_))
                        ) {
                            workspace.apply_action(
                                DesktopAction::SettingsProxyChanged {
                                    value: text,
                                    typed: true,
                                },
                                cx,
                            );
                        }
                    }
                    InputEvent::Blur => workspace.flush_settings_text_save(cx),
                    InputEvent::Focus | InputEvent::PressEnter { .. } => {}
                },
            )
            .detach();
            self.proxy_input = Some(input);
        }
        let input = self.proxy_input.clone().expect("proxy input");
        if self.proxy_sync_pending {
            let target = self
                .vm
                .settings
                .as_ref()
                .map(|settings| settings.proxy.clone())
                .filter(|proxy| {
                    matches!(
                        mycode_config::ProxyPreference::parse(proxy),
                        Some(mycode_config::ProxyPreference::Url(_))
                    )
                })
                .unwrap_or_default();
            let current = input.read(cx).value().to_string();
            if current != target {
                input.update(cx, |state, cx| state.set_value(target, window, cx));
            }
            self.proxy_sync_pending = false;
        }
        input
    }

    /// Picks the proxy mode from the General page: `auto`, `direct`, or
    /// `custom`. Custom keeps a stored URL, or starts from Clash's default
    /// port.
    pub(crate) fn on_select_proxy_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        let current = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.proxy.clone())
            .unwrap_or_default();
        let value = match mode {
            "direct" => mycode_config::PROXY_DIRECT.to_owned(),
            "custom" => match mycode_config::ProxyPreference::parse(&current) {
                Some(mycode_config::ProxyPreference::Url(_)) => return,
                _ => DEFAULT_PROXY_URL.to_owned(),
            },
            _ => String::new(),
        };
        self.proxy_sync_pending = true;
        self.apply_action(
            DesktopAction::SettingsProxyChanged {
                value,
                typed: false,
            },
            cx,
        );
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

    pub(crate) fn on_add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.provider_form.clone() else {
            return;
        };
        let (id, kind, base_url, model, api_key, context_limit, max_output) = {
            let read = form.read(cx);
            (
                read.id.read(cx).value().to_string(),
                read.kind.clone(),
                read.base_url.read(cx).value().to_string(),
                read.model.read(cx).value().to_string(),
                read.api_key.read(cx).value().to_string(),
                read.context_limit.read(cx).value().to_string(),
                read.max_output.read(cx).value().to_string(),
            )
        };
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
        let provider = match build_custom_provider(
            &existing,
            &id,
            &kind,
            &base_url,
            &model,
            &context_limit,
            &max_output,
        ) {
            Ok(provider) => provider,
            Err(message) => {
                self.apply_action(DesktopAction::Failed(message), cx);
                return;
            }
        };
        let provider_id = provider.id.clone();
        self.apply_action(DesktopAction::SettingsProviderAdded(provider), cx);
        if let Some(api_key) = normalized_key_input(&api_key) {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id,
                    api_key,
                },
                cx,
            );
        }
        form.update(cx, |form, cx| form.clear(window, cx));
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
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(crate::i18n::t("Paste API key", "粘贴 API 密钥"))
                });
                cx.subscribe_in(&input, window, |_, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
                .detach();
                input
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
        let Some(api_key) = normalized_key_input(&input.read(cx).value()) else {
            return;
        };
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

    pub(crate) fn on_cancel_replace_provider_key(&mut self, id: &str, cx: &mut Context<Self>) {
        self.provider_key_replace.remove(id);
        self.provider_key_inputs.remove(id);
        cx.notify();
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
        if self.vm.models_subview == crate::view_model::ModelsSubview::OpenDesign {
            // The OpenDesign page is the form itself; closing it leaves the page.
            self.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
        } else {
            self.apply_action(DesktopAction::ActivePresetChanged(None), cx);
        }
    }

    /// Lists the models the pasted (or stored) OpenDesign key can use.
    pub(crate) fn on_load_opendesign_models(&mut self, cx: &mut Context<Self>) {
        if self.vm.opendesign_loading {
            return;
        }
        // Blank uses the stored key; a Bearer-only paste is refused like on Save.
        let raw = self
            .preset_key_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let api_key = if raw.trim().is_empty() {
            String::new()
        } else {
            match normalized_key_input(&raw) {
                Some(key) => key,
                None => {
                    self.apply_action(
                        DesktopAction::Failed(
                            crate::i18n::t("paste a valid API key", "请粘贴有效的 API 密钥")
                                .to_owned(),
                        ),
                        cx,
                    );
                    return;
                }
            }
        };
        self.apply_action(DesktopAction::OpenDesignLoading(true), cx);
        self.dispatch(BridgeCommand::RefreshOpenDesignModels { api_key }, cx);
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
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(crate::i18n::t("Paste API key", "粘贴 API 密钥"))
                })
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
        let api_key = self
            .preset_key_input
            .clone()
            .and_then(|input| normalized_key_input(&input.read(cx).value()));
        // Drop the input entity so the pasted key never lingers on screen.
        self.preset_key_input = None;
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
        self.apply_action(DesktopAction::SettingsProviderAdded(row), cx);
        if let Some(api_key) = api_key {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: id.clone(),
                    api_key,
                },
                cx,
            );
        }
        if opendesign {
            self.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
        } else {
            self.apply_action(DesktopAction::ActivePresetChanged(None), cx);
        }
        self.apply_action(DesktopAction::ProviderSelected(id), cx);
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

    pub(crate) fn on_add_backend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.backend_form.clone() else {
            return;
        };
        let (id, kind, endpoint) = {
            let read = form.read(cx);
            (
                read.id.read(cx).value().to_string(),
                read.kind.read(cx).value().to_string(),
                read.endpoint.read(cx).value().to_string(),
            )
        };
        let existing: Vec<String> = self
            .vm
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .web_backends
                    .iter()
                    .map(|backend| backend.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let backend = match build_custom_backend(&existing, &id, &kind, &endpoint) {
            Ok(backend) => backend,
            Err(message) => {
                self.apply_action(DesktopAction::Failed(message), cx);
                return;
            }
        };
        self.apply_action(DesktopAction::SettingsBackendAdded(backend), cx);
        form.update(cx, |form, cx| form.clear(window, cx));
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
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder(crate::i18n::t(
                        "Paste API key (Bearer is added)",
                        "粘贴 API 密钥（自动加 Bearer）",
                    ))
                });
                cx.subscribe_in(&input, window, |_, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
                .detach();
                input
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
        let Some(api_key) = normalized_key_input(&input.read(cx).value()) else {
            return;
        };
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

    pub(crate) fn on_cancel_replace_web_key(&mut self, id: &str, cx: &mut Context<Self>) {
        self.web_key_replace.remove(id);
        self.web_key_inputs.remove(id);
        cx.notify();
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
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(crate::i18n::t("Paste API key", "粘贴 API 密钥"))
                })
            })
            .clone()
    }

    /// Adds the server the custom form currently describes: the pure builder
    /// next to [`McpForm`] validates the fields, this method dispatches.
    pub(crate) fn on_add_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                read.api_key.read(cx).value().to_string(),
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
        if transport == "http"
            && let Some(api_key) = normalized_key_input(&api_key)
        {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{id}"),
                    api_key,
                },
                cx,
            );
        }
        form.update(cx, |form, cx| form.clear(window, cx));
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
                        .placeholder(crate::i18n::t(
                            "Paste mcp.json or a Claude Desktop / Cursor config…",
                            "粘贴 mcp.json，或 Claude Desktop / Cursor 配置…",
                        ))
                        .auto_grow(4, 16)
                })
            })
            .clone()
    }

    pub(crate) fn on_import_mcp_json(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let mut duplicates: Vec<String> = Vec::new();
        for row in imported {
            if existing.contains(&row.server.id) {
                duplicates.push(row.server.id.clone());
                continue;
            }
            let server_id = row.server.id.clone();
            existing.push(server_id.clone());
            let api_key = row.api_key.clone();
            self.apply_action(DesktopAction::SettingsMcpAdded(row.server), cx);
            added += 1;
            if let Some(api_key) = api_key.and_then(|key| normalized_key_input(&key)) {
                self.dispatch(
                    BridgeCommand::SaveProviderKey {
                        provider_id: format!("mcp-{server_id}"),
                        api_key,
                    },
                    cx,
                );
            }
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        if added > 0 {
            self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
        }
        if !duplicates.is_empty() {
            // One aggregated message: per-row failures overwrote each other,
            // so only the last duplicate name was ever reported.
            self.apply_action(
                DesktopAction::Failed(format!(
                    "MCP servers already exist: {}",
                    duplicates.join(", ")
                )),
                cx,
            );
        }
    }

    // ---- skills ----

    pub(crate) fn on_skill_enabled(&mut self, slug: &str, enabled: bool, cx: &mut Context<Self>) {
        self.apply_action(
            DesktopAction::SettingsSkillToggled(slug.to_owned(), enabled),
            cx,
        );
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

#[cfg(test)]
mod tests {
    use super::normalized_key_input;

    #[test]
    fn blank_key_inputs_are_refused_and_bearer_prefixes_are_stripped() {
        assert_eq!(normalized_key_input(""), None);
        assert_eq!(normalized_key_input("   \t\n  "), None);
        assert_eq!(normalized_key_input("Bearer   "), None);
        assert_eq!(
            normalized_key_input("  sk-test-123  "),
            Some("sk-test-123".to_owned())
        );
        assert_eq!(
            normalized_key_input("Bearer sk-test-123"),
            Some("sk-test-123".to_owned())
        );
    }
}
