//! Model, provider, and reasoning pickers in the composer toolbar.
use gpui_kit::{Context, Window};

use super::Workspace;
use crate::view_model::FrontendAction;

impl Workspace {
    pub(crate) fn on_toggle_model_menu(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(FrontendAction::ModelMenuToggled(open), cx);
        if open && let Some(input) = self.model_picker_input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    /// Moves the open picker into one provider, or back to the provider step.
    ///
    /// Entering a provider asks the next render to focus the search box.
    /// Does not record a model selection.
    pub(crate) fn on_browse_model_provider(
        &mut self,
        provider_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.picker_focus_pending = provider_id.is_some();
        self.apply_action(
            FrontendAction::ModelMenuBrowse(provider_id.map(str::to_owned)),
            cx,
        );
    }

    /// Whether the model step should focus its search box this frame.
    pub(crate) fn take_model_step_focus(&mut self) -> bool {
        if self.vm().model_menu_browse.is_none() {
            return false;
        }
        let pending = self.picker_focus_pending;
        self.picker_focus_pending = false;
        pending
    }

    /// Enter in the model search box selects the preferred row.
    pub(crate) fn accept_model_search(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.vm().model_menu_browse.clone() else {
            return;
        };
        let choices = crate::ui::model_picker::provider_model_choices(self.vm(), &provider);
        let filtered =
            crate::ui::model_picker::filter_model_choices(&choices, &self.vm().picker_query);
        if filtered.is_empty() {
            return;
        }
        let role = self
            .vm()
            .subagent_menu
            .clone()
            .filter(|(_, field)| field == "model");
        let selected = if let Some((role, _)) = &role {
            self.vm()
                .settings
                .as_ref()
                .and_then(|settings| settings.subagents.role(role))
                .filter(|entry| entry.provider.as_deref() == Some(provider.as_str()))
                .and_then(|entry| entry.model.clone())
        } else if self.vm().selected_provider.as_deref() == Some(provider.as_str()) {
            self.vm().selected_model.clone()
        } else {
            None
        };
        let preferred = crate::ui::model_picker::preferred_index(
            &filtered,
            &self.vm().picker_query,
            selected.as_deref(),
        );
        let index = self.model_highlight(filtered.len(), preferred);
        let Some(model) = filtered.get(index).map(|choice| choice.id.clone()) else {
            return;
        };
        if let Some((role, _)) = role {
            self.on_set_subagent_route(&role, Some(provider), Some(model), cx);
            self.apply_action(FrontendAction::SubagentMenuToggled(None), cx);
        } else if self.vm().model_menu_open {
            self.on_select_model_on(&provider, &model, cx);
        }
    }

    pub(crate) fn on_open_provider_detail(&mut self, id: &str, cx: &mut Context<Self>) {
        self.apply_action(
            FrontendAction::ProviderDetailOpened(Some(id.to_owned())),
            cx,
        );
    }

    pub(crate) fn on_close_provider_detail(&mut self, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ProviderDetailOpened(None), cx);
    }

    pub(crate) fn on_toggle_reasoning_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ReasoningMenuToggled(open), cx);
    }

    pub(crate) fn on_select_provider(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ProviderSelected(provider_id.to_owned()), cx);
        if self
            .vm()
            .settings
            .as_ref()
            .is_some_and(|settings| settings.dirty)
        {
            self.driver_do(|d, fx| d.save_settings(fx), cx);
        }
        self.persist_ui_state();
    }

    /// Picks one model of the selected provider. The reducer appends an
    /// unknown model to the provider row (so the next turn can use it) or
    /// refuses the pick at the per-provider cap.
    pub(crate) fn on_select_model_on(
        &mut self,
        provider_id: &str,
        model_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.vm().selected_provider.as_deref() != Some(provider_id) {
            self.on_select_provider(provider_id, cx);
        }
        self.on_select_model(model_id, cx);
    }

    pub(crate) fn on_select_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ModelSelected(model_id.to_owned()), cx);
        if self
            .vm()
            .settings
            .as_ref()
            .is_some_and(|settings| settings.dirty)
        {
            self.driver_do(|d, fx| d.save_settings(fx), cx);
        }
        self.persist_ui_state();
    }

    /// Stores the reasoning effort on the open session. Other sessions keep
    /// the effort they already chose.
    pub(crate) fn on_select_reasoning(&mut self, level: &str, cx: &mut Context<Self>) {
        self.apply_action(
            FrontendAction::SettingsReasoningChanged(level.to_owned()),
            cx,
        );
        self.persist_ui_state();
    }
}
