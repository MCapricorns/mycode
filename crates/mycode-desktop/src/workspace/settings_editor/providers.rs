//! Provider rows in the settings editor: forms, API keys, endpoints,
//! OAuth sign-in, and the preset catalog.
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_app::BridgeCommand;

use super::normalized_key_input;
use crate::ui::{ProviderForm, build_custom_provider};
use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

impl Workspace {
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
            settings.preview_provider_base_url(id, &typed)
        };
        match previewed {
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
        if matches!(
            self.vm.models_subview,
            crate::view_model::ModelsSubview::OpenDesign
                | crate::view_model::ModelsSubview::GitHubCopilot
                | crate::view_model::ModelsSubview::OpenAiCodex
        ) {
            // The dedicated page is the form itself; closing it leaves the
            // page.
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
}
