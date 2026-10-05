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
                    InputState::new(window, cx)
                        .placeholder("Type a free-text answer for every question (separate with |)")
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

    fn on_answer_ask(&mut self, answers: Vec<String>, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
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

    /// Escape dismisses the topmost layer: the folder picker, then other
    /// floating UI. With nothing open it leaves the settings view.
    pub(crate) fn on_escape(&mut self, cx: &mut Context<Self>) {
        if self.project_picker.is_some() {
            self.project_picker = None;
            cx.notify();
            return;
        }
        if self.vm.changes_panel_open {
            self.apply_action(DesktopAction::ChangesPanelToggled(false), cx);
            return;
        }
        if self.vm.inspector_open && !self.vm.inspector_pinned {
            self.apply_action(
                DesktopAction::InspectorChanged {
                    open: false,
                    pinned: false,
                },
                cx,
            );
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
        if !dismissed
            && self.vm.view == MainView::Settings
            && self.vm.settings_section == crate::view_model::SettingsSection::Models
            && self.vm.models_subview != crate::view_model::ModelsSubview::List
        {
            self.apply_action(
                DesktopAction::ShowModelsSubview(crate::view_model::ModelsSubview::List),
                cx,
            );
            dismissed = true;
        }
        if !dismissed
            && self.vm.view == MainView::Settings
            && self.vm.settings_section == crate::view_model::SettingsSection::Models
            && self.vm.provider_detail.is_some()
        {
            self.apply_action(DesktopAction::ProviderDetailOpened(None), cx);
            dismissed = true;
        }
        if !dismissed
            && self.vm.view == MainView::Settings
            && self.vm.settings_section == crate::view_model::SettingsSection::Web
            && self.vm.web_subview != crate::view_model::WebSubview::List
        {
            self.apply_action(
                DesktopAction::ShowWebSubview(crate::view_model::WebSubview::List),
                cx,
            );
            dismissed = true;
        }
        if !dismissed
            && self.vm.view == MainView::Settings
            && self.vm.settings_section == crate::view_model::SettingsSection::Mcp
            && self.vm.mcp_subview != crate::view_model::McpSubview::List
        {
            self.apply_action(
                DesktopAction::ShowMcpSubview(crate::view_model::McpSubview::List),
                cx,
            );
            dismissed = true;
        }
        if !dismissed && self.vm.view == MainView::Settings {
            self.apply_action(DesktopAction::ShowMainView(MainView::Chat), cx);
        }
        if !dismissed && self.vm.sending {
            self.on_cancel_chat(cx);
        }
    }

    // ---- data export / import ----

    /// Exports product data (settings, UI state, session ledgers) to
    /// a file chosen in a save dialog. Secrets never travel.
    pub(crate) fn on_export_data(&mut self, cx: &mut Context<Self>) {
        let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
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

    /// Applies one export bundle chosen in an open dialog. Existing sessions
    /// are never overwritten.
    pub(crate) fn on_import_data(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a MYCode export bundle".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.first().cloned() else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.dispatch(BridgeCommand::ImportData { path }, cx);
            });
        })
        .detach();
    }
}
