//! Provider rows in the settings editor: forms, API keys, endpoints,
//! OAuth sign-in, and the preset catalog.
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_frontend::forms::normalized_key_input;

use crate::ui::ProviderForm;
use crate::view_model::FrontendAction;
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
        let mut added = false;
        self.driver_do(
            |d, fx| {
                added = d.add_custom_provider(
                    &id,
                    &kind,
                    &base_url,
                    &model,
                    &api_key,
                    &context_limit,
                    &max_output,
                    fx,
                );
            },
            cx,
        );
        if added {
            form.update(cx, |form, cx| form.clear(window, cx));
        }
    }

    pub(crate) fn on_remove_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.remove_provider(index, fx), cx);
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
                        workspace.apply_action(FrontendAction::PickerQueryChanged(text), cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        let text = entity.read(cx).value().to_string();
                        workspace.apply_action(FrontendAction::PickerQueryChanged(text), cx);
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
                    workspace.apply_action(FrontendAction::SettingsQueryChanged(text), cx);
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
                    workspace.apply_action(FrontendAction::PresetModelQueryChanged(text), cx);
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
                        .masked(true)
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
        let raw = input.read(cx).value().to_string();
        if !self.driver.save_key(id.to_owned(), &raw) {
            return;
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.provider_key_replace.remove(id);
        cx.notify();
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
    /// An accepted value is written back normalized and becomes the seed.
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
        let base_url = |workspace: &Self| {
            workspace.vm().settings.as_ref().and_then(|settings| {
                settings
                    .providers
                    .iter()
                    .find(|provider| provider.id == id)
                    .map(|provider| provider.base_url.clone())
            })
        };
        let before = base_url(self);
        self.driver_do(|d, fx| d.apply_provider_endpoint(id, &typed, fx), cx);
        let Some(after) = base_url(self).filter(|after| Some(after) != before.as_ref()) else {
            return;
        };
        input.update(cx, |state, cx| state.set_value(after.clone(), window, cx));
        self.provider_endpoint_seed.insert(id.to_owned(), after);
    }

    /// Device-code sign-in for a provider that is already configured.
    ///
    /// Does not open the add-from-catalog form.
    pub(crate) fn on_start_provider_oauth(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        self.driver.start_provider_oauth(provider_id);
        cx.notify();
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
                    workspace.apply_action(FrontendAction::PresetSearchChanged(text), cx);
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
            FrontendAction::ActivePresetChanged(Some(provider_id.to_owned())),
            cx,
        );
    }

    /// The preset form's Close returns to the catalog picker for the next
    /// row.
    pub(crate) fn on_close_preset(&mut self, cx: &mut Context<Self>) {
        self.preset_key_input = None;
        self.apply_action(FrontendAction::ActivePresetChanged(None), cx);
    }

    /// Starts an OAuth device-flow sign-in from the shared page, binding
    /// the catalog row's models (or the configured subset when the row is
    /// already in settings).
    pub(crate) fn on_start_oauth_sign_in(&mut self, provider_id: String, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.start_oauth_sign_in(provider_id, fx), cx);
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
                        .masked(true)
                })
            })
            .clone()
    }

    /// Adds one catalog preset: provider entry plus stored key, then saves.
    pub(crate) fn on_add_preset(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        let known = self
            .vm()
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.provider(provider_id).is_some());
        if !known {
            return;
        }
        let api_key = self
            .preset_key_input
            .clone()
            .and_then(|input| normalized_key_input(&input.read(cx).value()));
        // Drop the input entity so the pasted key never lingers on screen.
        self.preset_key_input = None;
        self.driver_do(|d, fx| d.add_preset_provider(provider_id, api_key, fx), cx);
    }
}
