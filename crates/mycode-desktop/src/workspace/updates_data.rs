//! Self-update buttons, data export/import prompts, the pending-ask card's
//! input entities, and the Escape key's layer dismissal. The product side
//! of every one of these lives in the frontend driver; this file keeps the
//! prompts and the per-question text fields only gpui can own.
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Context, Entity, Window};

use crate::view_model::nav::{DismissLayer, SettingsBack, settings_back_target, top_dismiss_layer};
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

    pub(crate) fn ask_input(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        while self.ask_inputs.len() <= index {
            let input_index = self.ask_inputs.len();
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(crate::i18n::t(
                    "Type a free-text answer",
                    "输入自由文本回答",
                ))
            });
            cx.subscribe_in(&input, window, move |workspace, input, event, _, cx| {
                match event {
                    InputEvent::PressEnter { .. } => {
                        workspace.driver_do(|d, fx| d.ask_next(fx), cx);
                    }
                    // Typing and a picked choice are mutually exclusive;
                    // the driver clears the pick when fresh text arrives.
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        workspace.driver_do(|d, fx| d.ask_text_typed(input_index, text, fx), cx);
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
        index: usize,
        answer: String,
        submit: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(|d, fx| d.pick_ask_choice(index, answer, submit, fx), cx);
    }

    /// Advances the ask card one question; the last question submits.
    pub(crate) fn on_ask_next(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.ask_next(fx), cx);
    }

    /// Steps the ask card back one question; earlier answers stay put.
    pub(crate) fn on_ask_back(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.ask_back(fx), cx);
    }

    pub(crate) fn on_dismiss_ask(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.dismiss_ask(fx), cx);
    }

    // ---- escape ----

    /// Escape dismisses the topmost layer: the command palette, then the
    /// folder picker, then other floating UI. In settings it steps back one
    /// page, matching that page's back button, and leaves for the desk only
    /// from a top-level section. Two things never answer it: the update
    /// prompt (it waits for an explicit button) and a running turn (Stop is
    /// the only cancel).
    pub(crate) fn on_escape(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if self.vm().command_palette_open {
            self.set_command_palette(false, window, cx);
            return;
        }
        // One keypress closes one layer. Diff is painted above the drawer
        // and Details, so it goes first; the drawer next; then an unpinned
        // Details overlay. A pinned Details column is part of the desk, not
        // a floating layer.
        if let Some(layer) = top_dismiss_layer(
            self.project_picker.is_some(),
            self.driver.git_diff_open(),
            self.vm().changes_panel_open,
            // The Details overlay is painted only on the desk. A leftover
            // unpinned inspector must not consume Escape while Settings is up.
            self.vm().inspector_open
                && !self.vm().inspector_pinned
                && self.vm().view == MainView::Chat,
        ) {
            match layer {
                DismissLayer::Picker => self.on_picker_cancel(cx),
                DismissLayer::Diff => self.on_close_git_diff_panel(cx),
                DismissLayer::Changes => {
                    self.apply_action(FrontendAction::ChangesPanelToggled(false), cx);
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
        // The install prompt ignores Escape: a stray keypress must neither
        // drop the staged update nor reach the turn running behind it.
        if self.vm().update_dialog_open {
            return;
        }
        if self.vm().subagent_window.is_some() {
            self.apply_action(FrontendAction::SubagentWindowChanged(None), cx);
            return;
        }
        let mut dismissed = self.driver.dismiss_floating();
        if dismissed {
            cx.notify();
        }
        if !dismissed && self.vm().view == MainView::Settings {
            // One level, matching the control on screen: a sub-page's own
            // back button, or Back to desk when that is the only back control.
            match settings_back_target(
                self.vm().settings_section,
                self.vm().models_subview,
                self.vm().provider_detail.is_some(),
                self.vm().active_preset.is_some(),
                self.vm().web_detail.is_some(),
                self.vm().mcp_subview,
            ) {
                SettingsBack::CloseProviderDetail => self.on_close_provider_detail(cx),
                SettingsBack::ClosePreset => self.on_close_preset(cx),
                SettingsBack::ModelsList => {
                    self.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
                }
                SettingsBack::WebList => {
                    self.on_close_web_detail(cx);
                }
                SettingsBack::McpList => {
                    self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
                }
                SettingsBack::Desk => {
                    self.apply_action(FrontendAction::ShowMainView(MainView::Chat), cx);
                }
            }
            dismissed = true;
        }
        if !dismissed && self.vm().pending_ask.is_some() {
            self.on_dismiss_ask(cx);
            dismissed = true;
        }
        // While a turn runs, Escape does nothing at all: it neither stops
        // the turn (the Stop button is the only way out) nor recalls the
        // queue, so a stray keypress cannot touch running work. A held
        // queue — after Stop, when nothing is sending — still recalls into
        // the composer for editing.
        if !dismissed && !self.vm().sending && !self.vm().compacting && !self.vm().queued.is_empty()
        {
            self.on_recall_queued(None, window, cx);
        }
    }

    // ---- data export / import ----

    /// Exports the product configuration (settings, UI state, provider API
    /// keys) to a file chosen in a save dialog. Conversations never travel,
    /// and the file carries the keys as plaintext JSON.
    pub(crate) fn on_export_data(&mut self, cx: &mut Context<Self>) {
        let home = crate::ui::project_picker::starting_directory();
        let documents = home.join("Documents");
        let directory = if documents.is_dir() { documents } else { home };
        let stamp = crate::unix_now().as_secs();
        let suggested = format!("mycode-export-{stamp}.json");
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |workspace, _cx| {
                workspace.driver.export_data(path);
            });
        })
        .detach();
    }

    /// Applies one export bundle chosen in an open dialog: settings, UI
    /// state, and provider keys. Conversations are never imported.
    pub(crate) fn on_import_data(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                crate::i18n::t("Choose a MYCode export bundle", "选择 MYCode 导出文件").into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first().cloned() else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.driver.import_data(path);
                cx.notify();
            });
        })
        .detach();
    }

    /// The bundle waiting for the MCP-command confirmation, if any.
    pub(crate) fn import_confirm(&self) -> Option<&std::path::Path> {
        self.driver.import_confirm()
    }

    /// Re-sends an import the user confirmed after it was refused for
    /// changing MCP server commands.
    pub(crate) fn on_confirm_import(&mut self, cx: &mut Context<Self>) {
        self.driver.confirm_import();
        cx.notify();
    }

    /// Drops a refused import without applying it.
    pub(crate) fn on_cancel_import(&mut self, cx: &mut Context<Self>) {
        self.driver.cancel_import();
        cx.notify();
    }

    /// The MCP row's enable switch (probe-gated enable lives in the driver).
    pub(crate) fn on_mcp_toggled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.toggle_mcp_server(index, enabled, fx), cx);
    }

    /// Probes one MCP server for its tool list.
    pub(crate) fn on_list_mcp_tools(&mut self, server_id: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.list_mcp_tools(server_id, fx), cx);
    }

    /// Adds a catalog-listed MCP server; a pasted key arms the enable probe.
    pub(crate) fn on_add_builtin_mcp(
        &mut self,
        server: mycode_config::McpServerSettings,
        api_key: &str,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(|d, fx| d.add_builtin_mcp(server, api_key, fx), cx);
    }
}
