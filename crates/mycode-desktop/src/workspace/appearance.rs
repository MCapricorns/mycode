//! Theme, interface font size, UI language, and the input placeholders
//! that have to follow a language switch.
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::theme::Theme;
use gpui_kit::{Context, Entity, Window, px};

use super::Workspace;
use crate::view_model::FrontendAction;

impl Workspace {
    /// Applies and persists dark or light.
    pub(crate) fn on_select_theme(&mut self, theme_id: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.select_theme(theme_id, fx), cx);
    }

    pub(super) fn paint_appearance(&mut self, theme_id: &str, cx: &mut Context<Self>) {
        let mode = crate::ui::desk::theme_mode(theme_id);
        if cx.theme().mode != mode {
            Theme::change(mode, None, cx);
        }
        crate::ui::desk::apply(Theme::global_mut(cx));
        self.applied_font_size.clear();
        crate::ui::syntax::sync_base(cx);
    }

    /// Applies and persists the interface font size. The next frame paints it.
    pub(crate) fn on_select_font_size(&mut self, font_size: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.select_font_size(font_size, fx), cx);
    }

    /// Paints the saved interface font size and the OS UI font onto the
    /// window, once per change. The size comes from the settings projection,
    /// which is what gets written to `settings.json`.
    pub(crate) fn sync_interface_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size_id = self
            .vm()
            .settings
            .as_ref()
            .map(|settings| settings.font_size.as_str())
            .unwrap_or("m");
        let size_id = crate::ui::desk::normalize_font_size(size_id);
        if self.applied_font_size == size_id {
            return;
        }
        let family = crate::ui::desk::ui_font_family(cx);
        let mono = crate::ui::desk::mono_font_family(cx);
        {
            let theme = Theme::global_mut(cx);
            crate::ui::desk::apply_font_size(theme, size_id);
            theme.font_family = family;
            theme.mono_font_family = mono;
        }
        window.set_rem_size(px(crate::ui::desk::interface_rem_px(size_id)));
        crate::ui::syntax::sync_base(cx);
        self.applied_font_size = size_id.to_owned();
    }

    /// Rewrites placeholders that were captured when the input was created.
    ///
    /// `t()` is live, but `InputState` stores the placeholder string. Switching
    /// to English left the Chinese settings-search placeholder in the box.
    pub(crate) fn sync_localized_placeholders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lang = u8::from(crate::i18n::is_chinese());
        if self.applied_language == lang {
            return;
        }
        self.applied_language = lang;
        let composer_placeholder = crate::ui::composer_placeholder(self.vm().sending);
        self.composer.update(cx, |state, cx| {
            state.set_placeholder(composer_placeholder, window, cx);
        });
        let pairs: [(&Option<Entity<InputState>>, &str); 5] = [
            (
                &self.session_filter_input,
                crate::i18n::t("Search sessions", "搜索会话"),
            ),
            (
                &self.settings_search_input,
                crate::i18n::t("Search settings", "搜索设置"),
            ),
            (
                &self.model_picker_input,
                crate::i18n::t("Filter by name or id", "按名称或 id 筛选"),
            ),
            (
                &self.preset_model_search_input,
                crate::i18n::t("Filter models", "筛选模型"),
            ),
            (
                &self.preset_search_input,
                crate::i18n::t("Filter providers…", "筛选服务商…"),
            ),
        ];
        for (input, placeholder) in pairs {
            if let Some(input) = input {
                input.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx);
                });
            }
        }
    }

    /// Applies and persists the UI language. The whole window re-renders on
    /// the next frame, so no theme-style refresh is needed.
    pub(crate) fn on_select_language(&mut self, language: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.select_language(language, fx), cx);
    }

    pub(crate) fn on_toggle_language_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::LanguageMenuToggled(open), cx);
    }
}
