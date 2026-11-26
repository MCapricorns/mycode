//! Self-update buttons, the pending-ask card's input entities, and the
//! Escape key's layer dismissal. The product side
//! of every one of these lives in the frontend driver; this file keeps the
//! prompts and the per-question text fields only gpui can own.
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Context, Entity, Window};
use mycode::frontend::driver::AskIdentity;

use crate::view_model::nav::{DismissLayer, top_dismiss_layer};
use crate::view_model::{FrontendAction, MainView};
use crate::workspace::Workspace;

impl Workspace {
    // ---- catalog ----

    pub(crate) fn on_refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.driver.refresh_catalog();
        cx.notify();
    }

    // ---- updates ----

    pub(crate) fn on_check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.check_update(manual, fx), cx);
    }

    pub(crate) fn on_download_update(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.download_update(fx), cx);
    }

    /// Installs the staged update: the driver arms the swap, then `Effect::Quit`
    /// exits so it can replace the binary.
    pub(crate) fn on_install_update(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.install_update(fx), cx);
    }

    pub(crate) fn on_toggle_auto_update(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.toggle_auto_update(enabled, fx), cx);
    }

    // ---- ask card inputs (entities stay here; answers live in the driver) ----

    pub(crate) fn ask_identity(&self) -> Option<AskIdentity> {
        self.driver.open_ask_identity()
    }

    pub(crate) fn ask_input(
        &mut self,
        identity: &AskIdentity,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        while self.ask_inputs.len() <= index {
            let input_index = self.ask_inputs.len();
            let draft = self
                .driver
                .ask_texts
                .get(input_index)
                .cloned()
                .unwrap_or_default();
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(crate::i18n::t(
                    "Type a free-text answer",
                    "输入自由文本回答",
                ));
                input.set_value(draft, window, cx);
                input
            });
            let identity = identity.clone();
            cx.subscribe_in(&input, window, move |workspace, input, event, _, cx| {
                // The old frame may still hold this entity after AskReset.
                // Even a refresh of the same ask must not revive an old input.
                if workspace
                    .ask_inputs
                    .get(input_index)
                    .is_none_or(|current| current.entity_id() != input.entity_id())
                {
                    return;
                }
                match event {
                    InputEvent::PressEnter { .. } => {
                        workspace.on_ask_next(&identity, cx);
                    }
                    // Typing and a picked choice are mutually exclusive;
                    // the driver clears the pick when fresh text arrives.
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        workspace.driver_do(
                            |d, fx| {
                                d.with_open_ask(&identity, fx, |d, fx| {
                                    d.ask_text_typed(input_index, text, fx);
                                });
                            },
                            cx,
                        );
                    }
                    _ => {}
                }
            })
            .detach();
            self.ask_inputs.push(input);
        }
        // A choice pick clears this row's text on the next paint — the
        // effect fires without a window, so the input's own render applies it.
        if self.ask_text_cleared.remove(&index) {
            self.ask_inputs[index].update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.ask_inputs[index].clone()
    }

    pub(crate) fn on_pick_ask_choice(
        &mut self,
        identity: &AskIdentity,
        index: usize,
        answer: String,
        submit: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(
            |d, fx| {
                d.with_open_ask(identity, fx, |d, fx| {
                    d.pick_ask_choice(index, answer, submit, fx);
                });
            },
            cx,
        );
    }

    /// Advances the ask card one question; the last question submits.
    pub(crate) fn on_ask_next(&mut self, identity: &AskIdentity, cx: &mut Context<Self>) {
        self.driver_do(
            |d, fx| d.with_open_ask(identity, fx, |d, fx| d.ask_next(fx)),
            cx,
        );
    }

    /// Steps the ask card back one question; earlier answers stay put.
    pub(crate) fn on_ask_back(&mut self, identity: &AskIdentity, cx: &mut Context<Self>) {
        self.driver_do(
            |d, fx| d.with_open_ask(identity, fx, |d, fx| d.ask_back(fx)),
            cx,
        );
    }

    pub(crate) fn on_dismiss_ask(&mut self, identity: &AskIdentity, cx: &mut Context<Self>) {
        self.driver_do(
            |d, fx| d.with_open_ask(identity, fx, |d, fx| d.dismiss_ask(fx)),
            cx,
        );
    }

    // ---- escape ----

    /// Escape closes the foremost panel. On the unobstructed desk it stops
    /// the current turn or compaction and keeps its counters and draft.
    pub(crate) fn on_escape(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if self.vm().command_palette_open {
            self.set_command_palette(false, window, cx);
            return;
        }
        let on_desk = self.vm().view == MainView::Chat;
        let layer = top_dismiss_layer(
            self.project_picker.is_some(),
            self.driver.git_diff_open(),
            self.vm().changes_panel_open,
            on_desk && self.vm().subagent_window.is_some(),
            on_desk && self.vm().inspector_open && !self.vm().inspector_pinned,
        );
        // The folder picker can open above the update dialog.
        if self.vm().update_dialog_open && layer != Some(DismissLayer::Picker) {
            return;
        }
        if let Some(layer) = layer {
            match layer {
                DismissLayer::Picker => self.on_picker_cancel(cx),
                DismissLayer::Diff => self.on_close_git_diff_panel(cx),
                DismissLayer::Changes => {
                    self.apply_action(FrontendAction::ChangesPanelToggled(false), cx);
                }
                DismissLayer::Subagent => {
                    self.apply_action(FrontendAction::SubagentWindowChanged(None), cx);
                }
                DismissLayer::Inspector => {
                    self.apply_action(
                        FrontendAction::InspectorChanged {
                            open: false,
                            pinned: false,
                        },
                        cx,
                    );
                }
            }
            return;
        }
        if self.send_choice_open {
            self.send_choice_open = false;
            cx.notify();
        } else if self.driver.dismiss_floating() {
            cx.notify();
        } else if !on_desk {
            self.apply_action(FrontendAction::ShowMainView(MainView::Chat), cx);
        } else if let Some(identity) = self.ask_identity() {
            self.on_dismiss_ask(&identity, cx);
        } else if self.vm().sending || self.vm().compacting {
            self.on_cancel_chat(cx);
        }
    }

    /// Probes one MCP server for its tool list.
    pub(crate) fn on_list_mcp_tools(&mut self, server_id: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.list_mcp_tools(server_id, fx), cx);
    }
}
