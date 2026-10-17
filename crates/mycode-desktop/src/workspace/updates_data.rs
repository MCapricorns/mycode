//! Self-update, data export/import, MCP probes, the pending-ask flow, and
//! the Escape key's dismissal of floating menus.
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, InputState};
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
    /// A web-vendor detail's back returns to the backend list.
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
    web_detail_open: bool,
    mcp: crate::view_model::McpSubview,
) -> SettingsBack {
    use crate::view_model::{McpSubview, ModelsSubview, SettingsSection};
    match section {
        SettingsSection::Models => match models {
            ModelsSubview::List if provider_open => SettingsBack::CloseProviderDetail,
            ModelsSubview::Catalog if preset_open => SettingsBack::ClosePreset,
            ModelsSubview::Catalog | ModelsSubview::OpenDesign | ModelsSubview::Custom => {
                SettingsBack::ModelsList
            }
            ModelsSubview::List => SettingsBack::Desk,
        },
        SettingsSection::Web if web_detail_open => SettingsBack::WebList,
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

    /// The row's enable switch. Disabling is immediate; enabling first
    /// verifies the stored key (for HTTP rows that declare auth) and a live
    /// tools probe, landing `on` only when the probe returns tools.
    pub(crate) fn on_mcp_toggled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        let Some(settings) = self.vm.settings.as_ref() else {
            return;
        };
        let Some(server) = settings.mcp_servers.get(index).cloned() else {
            return;
        };
        if !enabled {
            self.apply_action(DesktopAction::SettingsMcpToggled(index, false), cx);
            return;
        }
        // A row that declares a credential header needs its `mcp-<id>` key
        // before the probe can mean anything.
        let needs_key = server.transport == "http" && server.key_header.is_some();
        let keyed = settings.mcp_with_keys.iter().any(|id| id == &server.id);
        if needs_key && !keyed {
            self.push_toast(
                crate::i18n::t(
                    "Store an API key for this server first",
                    "请先为该服务器保存 API 密钥",
                )
                .to_owned(),
                crate::workspace::ToastKind::Error,
                cx,
            );
            return;
        }
        self.pending_mcp_enable.insert(server.id.clone());
        self.probe_mcp_for_enable(&server.id, cx);
    }

    /// Dispatches the tools probe that gates an enable switch: the row is
    /// marked connecting until the reply lands `enabled` or the error.
    pub(super) fn probe_mcp_for_enable(&mut self, server_id: &str, cx: &mut Context<Self>) {
        let Some(server) = self
            .vm
            .settings
            .as_ref()
            .and_then(|settings| settings.mcp_servers.iter().find(|s| s.id == server_id))
            .cloned()
        else {
            self.pending_mcp_enable.remove(server_id);
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
        mut server: mycode_config::McpServerSettings,
        api_key: &str,
        cx: &mut Context<Self>,
    ) {
        let server_id = server.id.clone();
        // New rows start disabled: the enable probe is armed by the key
        // save that follows, so a pasted key ends in a verified enable.
        server.enabled = false;
        self.apply_action(DesktopAction::SettingsMcpAdded(server), cx);
        if !api_key.is_empty() {
            self.pending_mcp_enable.insert(server_id.clone());
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
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        while self.ask_inputs.len() <= index {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(crate::i18n::t(
                    "Type a free-text answer",
                    "输入自由文本回答",
                ))
            });
            cx.subscribe_in(&input, window, |workspace, _, event, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    workspace.on_submit_free_ask(cx);
                }
            })
            .detach();
            self.ask_inputs.push(input);
        }
        self.ask_inputs[index].clone()
    }

    pub(crate) fn on_pick_ask_choice(
        &mut self,
        index: usize,
        answer: String,
        submit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.ask_inputs.get(index) {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
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
        let Some(rows) = self.vm.pending_ask.as_ref() else {
            return;
        };
        let free_text = self
            .ask_inputs
            .iter()
            .map(|input| input.read(cx).value().to_string())
            .collect::<Vec<_>>();
        match collect_ask_answers(rows, &self.vm.ask_answers, &free_text) {
            Ok(answers) => self.on_answer_ask(answers, cx),
            Err(index) => self.push_toast(
                format!(
                    "{} {} {}",
                    crate::i18n::t("Answer question", "请先回答第"),
                    index + 1,
                    crate::i18n::t("before submitting.", "题后再提交"),
                ),
                crate::workspace::ToastKind::Info,
                cx,
            ),
        }
    }

    pub(crate) fn on_dismiss_ask(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        self.ask_inputs.clear();
        self.apply_action(DesktopAction::AskAnswered, cx);
        self.dispatch(BridgeCommand::AskDismiss { session_id }, cx);
    }

    fn on_answer_ask(&mut self, answers: Vec<String>, cx: &mut Context<Self>) {
        let Some(session_id) = self.open_session_id() else {
            return;
        };
        // Each question owns its text; the next request starts with empty fields.
        self.ask_inputs.clear();
        self.apply_action(DesktopAction::AskAnswered, cx);
        self.dispatch(
            BridgeCommand::AskAnswer {
                session_id,
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
                DismissLayer::Picker => self.on_picker_cancel(cx),
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
                self.vm.web_detail.is_some(),
                self.vm.mcp_subview,
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
                    self.apply_action(DesktopAction::ShowMainView(MainView::Chat), cx);
                }
            }
            dismissed = true;
        }
        if !dismissed && self.vm.pending_ask.is_some() {
            self.on_dismiss_ask(cx);
            dismissed = true;
        }
        // A queue is the nearest layer: Escape takes it back into the
        // composer for editing. The next Escape stops the turn, and a stop
        // never sends what is queued.
        if !dismissed && !self.vm.queued.is_empty() {
            self.on_recall_queued(None, window, cx);
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

/// One input per question preserves arbitrary text, including shell pipes,
/// and does not let an answer to one question occupy another question's slot.
fn collect_ask_answers(
    rows: &[mycode_app::AskPrompt],
    picks: &[String],
    free_text: &[String],
) -> Result<Vec<String>, usize> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let text = free_text
                .get(index)
                .map(|text| text.trim())
                .unwrap_or_default();
            let answer = if text.is_empty() {
                picks.get(index).map(|text| text.trim()).unwrap_or_default()
            } else {
                text
            };
            if answer.is_empty() && !row.optional {
                Err(index)
            } else {
                Ok(answer.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DismissLayer, SettingsBack, settings_back_target, top_dismiss_layer};
    use crate::view_model::{McpSubview, ModelsSubview, SettingsSection};

    fn question(optional: bool) -> mycode_app::AskPrompt {
        mycode_app::AskPrompt {
            question: "Question?".to_owned(),
            choices: vec![],
            optional,
            multiple: false,
        }
    }

    #[test]
    fn free_text_preserves_pipes_and_question_identity() {
        let questions = [question(false), question(false)];
        assert_eq!(
            super::collect_ask_answers(
                &questions,
                &["yes".to_owned()],
                &[String::new(), "a | b".to_owned()]
            ),
            Ok(vec!["yes".to_owned(), "a | b".to_owned()])
        );
        assert_eq!(
            super::collect_ask_answers(&questions[..1], &[], &["a|b|c".to_owned()]),
            Ok(vec!["a|b|c".to_owned()])
        );
        assert_eq!(
            super::collect_ask_answers(&questions, &[], &["only first".to_owned()]),
            Err(1)
        );
        assert_eq!(
            super::collect_ask_answers(&[question(true)], &[], &[]),
            Ok(vec![String::new()])
        );
    }

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
                false,
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
                false,
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
                false,
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
                false,
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
                false,
                McpSubview::List,
            ),
            SettingsBack::ModelsList
        );
        // The OpenDesign page is its own add form: Escape leaves the page,
        // it does not just close the form.
        assert_eq!(
            settings_back_target(
                SettingsSection::Models,
                ModelsSubview::OpenDesign,
                false,
                true,
                false,
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
                true,
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
                    false,
                    subview,
                ),
                SettingsBack::McpList,
                "{subview:?}"
            );
        }
    }
}
