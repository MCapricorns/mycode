//! Keyboard navigation for the composer pickers. Selection is transient UI
//! state; changing a draft or filter resets it without changing the model.
use gpui_kit::{
    Context, Focusable as _, KeyDownEvent, ScrollHandle, UniformListScrollHandle, Window,
};

use crate::ui::model_picker::{
    ModelPickerTarget, ProviderChoice, preferred_index, selected_in_provider,
};
use crate::view_model::{FrontendAction, MentionKind};
use mycode_frontend::picker::ModelChoice;

use super::Workspace;

#[derive(Default)]
pub(super) struct MenuNavigation {
    mention: Option<usize>,
    reasoning: Option<usize>,
    model: Option<usize>,
    provider: Option<usize>,
    mention_scroll: ScrollHandle,
    reasoning_scroll: ScrollHandle,
    model_scroll: UniformListScrollHandle,
    provider_scroll: ScrollHandle,
}

/// Picker-relevant state captured before a driver call.
pub(super) struct MenuNavigationMark {
    session: Option<String>,
    mention_len: Option<usize>,
}

fn highlight(selected: Option<usize>, len: usize, preferred: usize) -> usize {
    selected.unwrap_or(preferred).min(len.saturating_sub(1))
}

fn moved(current: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    }
}

impl Workspace {
    pub(super) fn reset_menu_navigation(&mut self, action: &FrontendAction) {
        match action {
            FrontendAction::ComposerChanged(text) if text.as_str() != self.vm().composer_draft => {
                self.menu_navigation.mention = None;
            }
            FrontendAction::MentionFiles(_) => self.menu_navigation.mention = None,
            FrontendAction::ReasoningMenuToggled(_) => self.menu_navigation.reasoning = None,
            FrontendAction::ModelMenuToggled(_) | FrontendAction::SubagentMenuToggled(_) => {
                self.menu_navigation.model = None;
                self.menu_navigation.provider = None;
            }
            FrontendAction::ModelMenuBrowse(_) => self.menu_navigation.model = None,
            FrontendAction::PickerQueryChanged(query)
                if query.as_str() != self.vm().picker_query =>
            {
                self.menu_navigation.model = None;
            }
            FrontendAction::ConversationOpened(_)
            | FrontendAction::SessionDeleted
            | FrontendAction::ConversationParked => {
                self.menu_navigation = MenuNavigation::default();
            }
            _ => {}
        }
    }

    /// The state a picker selection depends on, captured around a driver
    /// call whose actions the window never sees (replies and events).
    pub(super) fn menu_navigation_mark(&self) -> MenuNavigationMark {
        MenuNavigationMark {
            session: self
                .vm()
                .active
                .as_ref()
                .map(|active| active.session_id.clone()),
            mention_len: self
                .vm()
                .mention
                .as_ref()
                .map(|mention| mention.items.len()),
        }
    }

    /// The driver-side counterpart of [`Self::reset_menu_navigation`]: a
    /// conversation switch drops every selection, fresh mention results drop
    /// the mention highlight.
    pub(super) fn reset_menu_navigation_after(&mut self, before: MenuNavigationMark) {
        let after = self.menu_navigation_mark();
        if after.session != before.session {
            self.menu_navigation = MenuNavigation::default();
        } else if after.mention_len != before.mention_len {
            self.menu_navigation.mention = None;
        }
    }

    pub(crate) fn mention_highlight(&self) -> usize {
        let Some(mention) = &self.vm().mention else {
            return 0;
        };
        let preferred = if mention.kind == MentionKind::Command {
            crate::view_model::preferred_slash_index(&mention.fragment, &mention.items)
        } else {
            0
        };
        highlight(self.menu_navigation.mention, mention.items.len(), preferred)
    }

    pub(crate) fn reasoning_highlight(&self, levels: &[String]) -> usize {
        let selected = crate::view_model::selected_reasoning_level(self.vm());
        let preferred = levels
            .iter()
            .position(|level| *level == selected)
            .unwrap_or(0);
        highlight(self.menu_navigation.reasoning, levels.len(), preferred)
    }

    /// The row the model step highlights in `filtered`: the keyboard
    /// selection, else the target's current model, else the first match.
    pub(crate) fn model_highlight(
        &self,
        target: &ModelPickerTarget,
        provider: &str,
        filtered: &[ModelChoice],
    ) -> usize {
        let selected = selected_in_provider(self.vm(), target, provider);
        let preferred = preferred_index(filtered, &self.vm().picker_query, selected.as_deref());
        highlight(self.menu_navigation.model, filtered.len(), preferred)
    }

    pub(crate) fn provider_highlight(&self, len: usize) -> usize {
        highlight(self.menu_navigation.provider, len, 0)
    }

    pub(crate) fn mention_scroll(&self) -> &ScrollHandle {
        &self.menu_navigation.mention_scroll
    }

    pub(crate) fn reasoning_scroll(&self) -> &ScrollHandle {
        &self.menu_navigation.reasoning_scroll
    }

    pub(crate) fn model_scroll(&self) -> &UniformListScrollHandle {
        &self.menu_navigation.model_scroll
    }

    pub(crate) fn provider_scroll(&self) -> &ScrollHandle {
        &self.menu_navigation.provider_scroll
    }

    /// Captures only plain navigation keys for a visible picker. Other input
    /// shortcuts and higher-priority dialogs retain their normal handlers.
    pub(crate) fn on_menu_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if !matches!(key, "up" | "down" | "enter")
            || modifiers.control
            || modifiers.platform
            || modifiers.alt
            || modifiers.shift
            || modifiers.function
            || self.vm().command_palette_open
            || self.project_picker.is_some()
            || self.vm().update_dialog_open
            || self.vm().subagent_window.is_some()
        {
            return false;
        }
        if self.vm().reasoning_menu_open {
            let levels = crate::view_model::selected_reasoning_levels(self.vm());
            if levels.is_empty() {
                return false;
            }
            let current = self.reasoning_highlight(&levels);
            if key == "enter" {
                self.on_select_reasoning(&levels[current], cx);
            } else {
                let index = moved(current, levels.len(), key == "down");
                self.menu_navigation.reasoning = Some(index);
                self.menu_navigation.reasoning_scroll.scroll_to_item(index);
                cx.notify();
            }
            return true;
        }
        if let Some(target) = self.open_model_picker() {
            if let Some(provider) = self.vm().model_menu_browse.clone() {
                let choices = crate::ui::model_picker::provider_model_choices(self.vm(), &provider);
                let filtered = crate::ui::model_picker::filter_model_choices(
                    &choices,
                    &self.vm().picker_query,
                );
                if filtered.is_empty() {
                    return true;
                }
                if key == "enter" {
                    self.accept_model_search(cx);
                } else {
                    let index = moved(
                        self.model_highlight(&target, &provider, &filtered),
                        filtered.len(),
                        key == "down",
                    );
                    self.menu_navigation.model = Some(index);
                    self.menu_navigation
                        .model_scroll
                        .scroll_to_item(index, gpui_kit::ScrollStrategy::Nearest);
                    cx.notify();
                }
            } else {
                let choices = crate::ui::model_picker::provider_choices(self.vm(), &target);
                if choices.is_empty() {
                    return true;
                }
                let current = self.provider_highlight(choices.len());
                if key == "enter" {
                    match &choices[current] {
                        ProviderChoice::Inherit => {
                            if let ModelPickerTarget::Role(role) = target {
                                self.on_set_subagent_route(
                                    &role,
                                    Some("inherit".to_owned()),
                                    None,
                                    cx,
                                );
                                self.apply_action(FrontendAction::SubagentMenuToggled(None), cx);
                            }
                        }
                        ProviderChoice::Pin(pin) => crate::ui::model_picker::select_model(
                            self,
                            &target,
                            &pin.provider,
                            &pin.model,
                            cx,
                        ),
                        ProviderChoice::Provider(id) => self.on_browse_model_provider(Some(id), cx),
                    }
                } else {
                    let index = moved(current, choices.len(), key == "down");
                    self.menu_navigation.provider = Some(index);
                    let child =
                        crate::ui::model_picker::provider_scroll_index(self.vm(), &target, index);
                    self.menu_navigation.provider_scroll.scroll_to_item(child);
                    cx.notify();
                }
            }
            return true;
        }
        if self
            .vm()
            .mention
            .as_ref()
            .is_some_and(|mention| !mention.items.is_empty())
            && self.composer.read(cx).focus_handle(cx).is_focused(window)
        {
            if key == "enter" {
                return self.accept_open_mention(window, cx);
            }
            let index = moved(
                self.mention_highlight(),
                self.vm()
                    .mention
                    .as_ref()
                    .expect("visible mention")
                    .items
                    .len(),
                key == "down",
            );
            // The file list's FILES heading is the panel's first scroll
            // child; the flat slash menu has no headings.
            let heading = usize::from(
                self.vm()
                    .mention
                    .as_ref()
                    .is_some_and(|mention| mention.kind == MentionKind::File),
            );
            self.menu_navigation.mention = Some(index);
            self.menu_navigation
                .mention_scroll
                .scroll_to_item(index + heading);
            cx.notify();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{highlight, moved};

    #[test]
    fn navigation_wraps_and_clamps_after_filtering() {
        assert_eq!(moved(0, 3, false), 2);
        assert_eq!(moved(2, 3, true), 0);
        assert_eq!(moved(0, 0, true), 0);
        assert_eq!(highlight(None, 5, 3), 3);
        assert_eq!(highlight(Some(4), 2, 0), 1);
        assert_eq!(highlight(None, 0, 7), 0);
    }
}
