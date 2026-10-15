//! Self-update, data export/import, MCP probes, the pending-ask flow, and
//! the Escape key's dismissal of floating menus.
use std::time::Duration;

use gpui_kit::component::input::InputState;
use gpui_kit::{AppContext as _, Context, Entity, Window};

use mycode_app::BridgeCommand;

use crate::view_model::{DesktopAction, MainView, UpdateState, close_floating_menus};
use crate::workspace::Workspace;

/// Wakes once a day of runtime, then checks for an update when that is enabled.
const UPDATE_RECHECK_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// One floating layer Escape can dismiss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DismissLayer {
    Picker,
    Diff,
    Changes,
    Inspector,
}

/// The single layer one Escape press closes. Later flags stay open.
pub(crate) fn top_dismiss_layer(
    picker: bool,
    diff: bool,
    changes: bool,
    inspector_overlay: bool,
) -> Option<DismissLayer> {
    if picker {
        Some(DismissLayer::Picker)
    } else if diff {
        Some(DismissLayer::Diff)
    } else if changes {
        Some(DismissLayer::Changes)
    } else if inspector_overlay {
        Some(DismissLayer::Inspector)
    } else {
        None
    }
}

/// Where one Escape press goes inside Settings. Each arm matches the back
/// control that page actually draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsBack {
    /// Provider detail's back returns to the provider list.
    CloseProviderDetail,
    /// A catalog preset's back returns to the catalog, not the provider list.
    ClosePreset,
    /// Catalog or custom-endpoint back returns to the provider list.
    ModelsList,
    /// Custom web-backend back returns to the backend list.
    WebList,
    /// Catalog, JSON, or custom MCP back returns to the server list.
    McpList,
    /// Back to desk.
    Desk,
}

/// The settings page Escape should open. Sub-pages step back once.
pub(crate) fn settings_back_target(
    section: crate::view_model::SettingsSection,
    models: crate::view_model::ModelsSubview,
    provider_open: bool,
    preset_open: bool,
    web: crate::view_model::WebSubview,
    mcp: crate::view_model::McpSubview,
) -> SettingsBack {
    use crate::view_model::{McpSubview, ModelsSubview, SettingsSection, WebSubview};
    match section {
        SettingsSection::Models => match models {
            ModelsSubview::List if provider_open => SettingsBack::CloseProviderDetail,
            ModelsSubview::Catalog if preset_open => SettingsBack::ClosePreset,
            ModelsSubview::Catalog | ModelsSubview::Custom => SettingsBack::ModelsList,
            ModelsSubview::List => SettingsBack::Desk,
        },
        SettingsSection::Web if web != WebSubview::List => SettingsBack::WebList,
        SettingsSection::Mcp if mcp != McpSubview::List => SettingsBack::McpList,
        _ => SettingsBack::Desk,
    }
}

impl Workspace {
    /// Parks until a day has passed, then runs the automatic update check.
    pub(super) fn spawn_update_recheck(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(UPDATE_RECHECK_AFTER).await;
                if this
                    .update(cx, |workspace, cx| {
                        if workspace.vm.auto_update {
                            workspace.on_check_update(false, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    // ---- catalog ----

    pub(crate) fn on_refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.pending_catalog_refresh = true;
        self.dispatch(BridgeCommand::RefreshCatalog, cx);
    }

    // ---- updates ----

    pub(crate) fn on_check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        // Checking replaces the update state with `Checking`. Doing that
        // mid-download makes the next offer look idle and starts a second
        // download of the same package.
        if matches!(
            self.vm.update,
            UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            return;
        }
        if manual {
            self.manual_update_check = true;
        }
        self.apply_action(DesktopAction::UpdateStateChanged(UpdateState::Checking), cx);
        self.dispatch(BridgeCommand::CheckUpdate, cx);
    }

    pub(crate) fn on_download_update(&mut self, cx: &mut Context<Self>) {
        // One download at a time: a re-offer arriving mid-download must not
        // restart it, and a staged update only needs the install prompt.
        if matches!(
            self.vm.update,
            UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            return;
        }
        let Some(offer) = self.vm.last_offer.clone() else {
            return;
        };
        self.apply_action(
            DesktopAction::UpdateStateChanged(UpdateState::Downloading {
                version: offer.version.clone(),
            }),
            cx,
        );
        self.dispatch(BridgeCommand::DownloadUpdate { offer }, cx);
    }

    /// Installs the staged update: arm the detached swap, then exit.
    pub(crate) fn on_install_update(&mut self, cx: &mut Context<Self>) {
        let Some(prepared) = self.vm.prepared_update.clone() else {
            return;
        };
        if let Err(message) = mycode_app::apply_and_restart(&prepared) {
            self.apply_action(DesktopAction::Failed(message), cx);
            return;
        }
        cx.quit();
    }

    pub(crate) fn on_toggle_auto_update(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::AutoUpdateToggled(enabled), cx);
        self.persist_ui_state(cx);
    }

    // ---- ask / tools ----

    /// Probes one MCP server as the editor currently shows it (saved or
    /// not) and marks the row as connecting until the reply lands.
    pub(crate) fn on_list_mcp_tools(&mut self, server_id: &str, cx: &mut Context<Self>) {
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.iter().find(|s| s.id == server_id))
            .cloned()
        else {
            return;
        };
        self.apply_action(DesktopAction::McpProbeStarted(server_id.to_owned()), cx);
        self.dispatch(
            BridgeCommand::McpListTools {
                server: Box::new(server),
            },
            cx,
        );
    }

    pub(crate) fn on_add_builtin_mcp(
        &mut self,
        server: mycode_config::McpServerSettings,
        api_key: &str,
        cx: &mut Context<Self>,
    ) {
        let server_id = server.id.clone();
        self.apply_action(DesktopAction::SettingsMcpAdded(server), cx);
        if !api_key.is_empty() {
            self.dispatch(
                BridgeCommand::SaveProviderKey {
                    provider_id: format!("mcp-{server_id}"),
                    api_key: api_key.to_owned(),
                },
                cx,
            );
        }
        self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
    }

    pub(crate) fn ask_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.ask_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder(crate::i18n::t(
                        "Type a free-text answer for every question (separate with |)",
                        "为每个问题输入自由文本（用 | 分隔）",
                    ))
                })
            })
            .clone()
    }

    pub(crate) fn on_pick_ask_choice(
        &mut self,
        index: usize,
        answer: String,
        submit: bool,
        cx: &mut Context<Self>,
    ) {
        let current = self.vm.ask_answers.get(index).cloned().unwrap_or_default();
        let multiple = self
            .vm
            .pending_ask
            .as_ref()
            .and_then(|rows| rows.get(index))
            .is_some_and(|row| row.multiple);
        let answer = mycode_tools::builtin::toggle_ask_choice(&current, &answer, multiple);
        self.apply_action(DesktopAction::AskChoicePicked { index, answer }, cx);
        if submit {
            self.on_answer_ask(self.vm.ask_answers.clone(), cx);
        }
    }

    pub(crate) fn on_submit_free_ask(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.ask_input.clone() else {
            return;
        };
        let Some(rows) = self.vm.pending_ask.clone() else {
            return;
        };
        let raw = input.read(cx).value().to_string();
        let parts: Vec<String> = raw.split('|').map(|part| part.trim().to_owned()).collect();
        let mut answers = self.vm.ask_answers.clone();
        answers.resize(rows.len(), String::new());
        for (index, slot) in answers.iter_mut().enumerate() {
            if slot.is_empty() {
                *slot = parts.get(index).cloned().unwrap_or_default();
            }
        }
        self.on_answer_ask(answers, cx);
    }

    pub(crate) fn on_dismiss_ask(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        self.ask_input = None;
        self.apply_action(DesktopAction::AskAnswered, cx);
        self.dispatch(
            BridgeCommand::AskDismiss {
                session_id: conversation.session_id,
            },
            cx,
        );
    }

    fn on_answer_ask(&mut self, answers: Vec<String>, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        // Drop the free-text field so the next ask panel starts empty; the
        // entity is created lazily on first use and would otherwise carry
        // this answer into an unrelated question.
        self.ask_input = None;
        self.apply_action(DesktopAction::AskAnswered, cx);
        self.dispatch(
            BridgeCommand::AskAnswer {
                session_id: conversation.session_id,
                answers,
            },
            cx,
        );
    }

    // ---- escape ----

    /// Escape dismisses the topmost layer: the command palette, then the
    /// folder picker, then other floating UI. In settings it steps back one
    /// page, matching that page's back button, and leaves for the desk only
    /// from a top-level section.
    pub(crate) fn on_escape(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if self.vm.command_palette_open {
            self.set_command_palette(false, window, cx);
            return;
        }
        // One keypress closes one layer. Diff is painted above the drawer
        // and Details, so it goes first; the drawer next; then an unpinned
        // Details overlay. A pinned Details column is part of the desk, not
        // a floating layer.
        if let Some(layer) = top_dismiss_layer(
            self.project_picker.is_some(),
            self.git_diff_panel_open,
            self.vm.changes_panel_open,
            // The Details overlay is painted only on the desk. A leftover
            // unpinned inspector must not consume Escape while Settings is up.
            self.vm.inspector_open && !self.vm.inspector_pinned && self.vm.view == MainView::Chat,
        ) {
            match layer {
                DismissLayer::Picker => {
                    self.project_picker = None;
                    cx.notify();
                }
                DismissLayer::Diff => {
                    self.git_diff_panel_open = false;
                    cx.notify();
                }
                DismissLayer::Changes => {
                    self.apply_action(DesktopAction::ChangesPanelToggled(false), cx);
                }
                DismissLayer::Inspector => {
                    self.apply_action(
                        DesktopAction::InspectorChanged {
                            open: false,
                            pinned: false,
                        },
                        cx,
                    );
                }
            }
            return;
        }
        if self.vm.update_dialog_open {
            self.apply_action(DesktopAction::UpdateDialogToggled(false), cx);
            return;
        }
        if self.vm.subagent_window.is_some() {
            self.apply_action(DesktopAction::SubagentWindowChanged(None), cx);
            return;
        }
        let mut dismissed = close_floating_menus(&mut self.vm);
        if dismissed {
            cx.notify();
        }
        if !dismissed && self.vm.view == MainView::Settings {
            // One level, matching the control on screen: a sub-page's own
            // back button, or Back to desk when that is the only back control.
            match settings_back_target(
                self.vm.settings_section,
                self.vm.models_subview,
                self.vm.provider_detail.is_some(),
                self.vm.active_preset.is_some(),
                self.vm.web_subview,
                self.vm.mcp_subview,
            ) {
                SettingsBack::CloseProviderDetail => self.on_close_provider_detail(cx),
                SettingsBack::ClosePreset => self.on_close_preset(cx),
                SettingsBack::ModelsList => {
                    self.on_show_models_subview(crate::view_model::ModelsSubview::List, cx);
                }
                SettingsBack::WebList => {
                    self.on_show_web_subview(crate::view_model::WebSubview::List, cx);
                }
                SettingsBack::McpList => {
                    self.on_show_mcp_subview(crate::view_model::McpSubview::List, cx);
                }
                SettingsBack::Desk => {
                    self.apply_action(DesktopAction::ShowMainView(MainView::Chat), cx);
                }
            }
            dismissed = true;
        }
        if !dismissed && self.vm.pending_ask.is_some() {
            self.on_dismiss_ask(cx);
            dismissed = true;
        }
        if !dismissed && self.vm.sending {
            self.on_cancel_chat(cx);
        }
    }

    // ---- data export / import ----

    /// Exports the product configuration (settings, UI state, provider API
    /// keys) to a file chosen in a save dialog. Conversations never travel,
    /// and the file carries the keys as plaintext JSON.
    pub(crate) fn on_export_data(&mut self, cx: &mut Context<Self>) {
        let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let stamp = crate::unix_now().as_secs();
        let suggested = format!("mycode-export-{stamp}.json");
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.dispatch(BridgeCommand::ExportData { path }, cx);
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
                workspace.import_confirm = None;
                workspace.import_request = Some(path.clone());
                workspace.dispatch(
                    BridgeCommand::ImportData {
                        path,
                        confirm_mcp_commands: false,
                    },
                    cx,
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// The bundle waiting for the MCP-command confirmation, if any.
    pub(crate) fn import_confirm(&self) -> Option<&std::path::Path> {
        self.import_confirm.as_deref()
    }

    /// Re-sends an import the user confirmed after it was refused for
    /// changing MCP server commands.
    pub(crate) fn on_confirm_import(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.import_confirm.take() else {
            return;
        };
        self.import_request = Some(path.clone());
        self.dispatch(
            BridgeCommand::ImportData {
                path,
                confirm_mcp_commands: true,
            },
            cx,
        );
        cx.notify();
    }

    /// Drops a refused import without applying it.
    pub(crate) fn on_cancel_import(&mut self, cx: &mut Context<Self>) {
        self.import_confirm = None;
        self.import_request = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{DismissLayer, SettingsBack, settings_back_target, top_dismiss_layer};
    use crate::view_model::{McpSubview, ModelsSubview, SettingsSection, WebSubview};

    #[test]
    fn escape_closes_one_layer_topmost_first() {
        assert_eq!(
            top_dismiss_layer(false, true, true, true),
            Some(DismissLayer::Diff)
        );
        assert_eq!(
            top_dismiss_layer(false, false, true, true),
            Some(DismissLayer::Changes)
        );
        assert_eq!(
            top_dismiss_layer(false, false, false, true),
            Some(DismissLayer::Inspector)
        );
        assert_eq!(
            top_dismiss_layer(true, true, true, true),
            Some(DismissLayer::Picker)
        );
        assert_eq!(top_dismiss_layer(false, false, false, false), None);
    }

    #[test]
    fn settings_escape_matches_the_visible_back_button() {
        let desk = |section| {
            settings_back_target(
                section,
                ModelsSubview::List,
                false,
                false,
                WebSubview::List,
                McpSubview::List,
            )
        };
        assert_eq!(desk(SettingsSection::General), SettingsBack::Desk);
        assert_eq!(desk(SettingsSection::About), SettingsBack::Desk);
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::List,
                true,
                false,
                WebSubview::List,
                McpSubview::List,
            ),
            SettingsBack::CloseProviderDetail
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::Catalog,
                false,
                true,
                WebSubview::List,
                McpSubview::List,
            ),
            SettingsBack::ClosePreset
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::Catalog,
                false,
                false,
                WebSubview::List,
                McpSubview::List,
            ),
            SettingsBack::ModelsList
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::Custom,
                false,
                false,
                WebSubview::List,
                McpSubview::List,
            ),
            SettingsBack::ModelsList
        );
        assert_eq!(
            settings_back_target(
                SettingsSection::Web,
                ModelsSubview::List,
                false,
                false,
                WebSubview::Custom,
                McpSubview::List,
            ),
            SettingsBack::WebList
        );
        for subview in [McpSubview::Catalog, McpSubview::Json, McpSubview::Custom] {
            assert_eq!(
                settings_back_target(
                    SettingsSection::Mcp,
                    ModelsSubview::List,
                    false,
                    false,
                    WebSubview::List,
                    subview,
                ),
                SettingsBack::McpList,
                "{subview:?}"
            );
        }
    }
}
