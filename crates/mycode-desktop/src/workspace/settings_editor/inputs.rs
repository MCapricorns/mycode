//! The two search boxes shared by the settings shell and the model picker.

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use crate::view_model::FrontendAction;
use crate::workspace::Workspace;

impl Workspace {
    /// Search box for the shared model picker. Created once; the value is
    /// cleared when the picker opens.
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
}
