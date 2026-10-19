//! Project- and session-shaped workspace handlers. The mention pipeline,
//! slash commands, session switching, and project bindings all live in the
//! frontend driver; this file keeps the gpui folder picker, the file dialog,
//! and drag-drop plumbing.
use gpui_kit::{Context, Window};

use crate::view_model::FrontendAction;
use crate::workspace::Workspace;

impl Workspace {
    /// Enter accepts the highlighted file or command instead of sending an
    /// incomplete token. An empty menu still allows an ordinary message.
    pub(super) fn accept_open_mention(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mention) = self.vm().mention.as_ref() else {
            return false;
        };
        if mention.items.is_empty() {
            return false;
        }
        let index = self.mention_highlight();
        self.driver_do(|d, fx| d.accept_mention(index, fx), cx);
        true
    }

    /// Accepts one mention row: rewrites the draft (files) or runs the
    /// command (commands), then closes the menu.
    pub(crate) fn on_accept_mention(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.driver_do(|d, fx| d.accept_mention(index, fx), cx);
    }

    /// Runs one composer slash token: built-in commands, a skill, or MCP.
    ///
    /// The command palette accepts the same tokens as the `/` menu.
    pub(super) fn run_slash_command(&mut self, insert: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.run_slash_command(insert, fx), cx);
    }

    pub(crate) fn on_refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.refresh_skills(fx), cx);
    }

    pub(crate) fn on_use_skill(&mut self, slug: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.use_skill(slug, fx), cx);
    }

    /// Adds one folder to the workspace without moving a chat that already
    /// has its own working directory. An unbound chat adopts the folder.
    pub(crate) fn on_add_workspace_root(&mut self, project: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.add_workspace_root(project, fx), cx);
    }

    /// Drops one linked folder, including the one the open chat works in.
    ///
    /// The folder's chats go with it: every session bound to that directory
    /// is deleted. Sessions bound to other folders are untouched. A folder
    /// that still has sessions takes the same second click as a session
    /// delete; an empty one goes at once.
    pub(crate) fn on_remove_workspace_root(&mut self, project: &str, cx: &mut Context<Self>) {
        let deletes_sessions =
            !crate::view_model::sessions_removed_with_root(self.vm(), project).is_empty();
        if deletes_sessions && !self.confirm_delete(&root_delete_key(project), cx) {
            return;
        }
        self.driver_do(|d, fx| d.remove_workspace_root(project, fx), cx);
    }

    /// The folder whose remove button is waiting for its second click.
    pub(crate) fn armed_root_delete(&self) -> Option<&str> {
        self.vm()
            .workspace_roots
            .iter()
            .map(String::as_str)
            .find(|root| self.delete_armed(&root_delete_key(root)))
    }

    /// Removes one directory from the remembered projects list.
    pub(crate) fn on_remove_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.driver_do(|d, fx| d.remove_recent(project, fx), cx);
    }

    /// Starts a fresh chat (the New-task affordance). `false` when no folder
    /// is open — the only case that drops a held welcome draft.
    pub(crate) fn on_new_session(&mut self, cx: &mut Context<Self>) -> bool {
        let mut started = false;
        self.driver_do(
            |d, fx| {
                started = d.new_session(fx);
            },
            cx,
        );
        started
    }

    pub(crate) fn on_toggle_project_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ProjectMenuToggled(open), cx);
    }

    // ---- project selection ----

    /// Opens the in-app folder browser and binds the chosen directory.
    pub(crate) fn on_open_project_dialog(&mut self, cx: &mut Context<Self>) {
        self.picker_chat_session = None;
        self.project_picker = Some(crate::ui::project_picker::ProjectPicker::open());
        cx.notify();
    }

    /// Unlike Add folder, this explicit action changes this chat's directory.
    pub(crate) fn on_open_chat_folder_picker(&mut self, cx: &mut Context<Self>) {
        if self.vm().sending {
            self.push_toast(
                crate::i18n::t(
                    "Finish or stop the current turn before changing its folder.",
                    "请先完成或停止当前轮次，再更换目录。",
                ),
                crate::workspace::ToastKind::Info,
                cx,
            );
            return;
        }
        self.picker_chat_session = self.driver.open_session_id();
        self.project_picker = Some(crate::ui::project_picker::browse(
            self.vm().project_dir.as_ref().map(std::path::PathBuf::from),
        ));
        cx.notify();
    }

    /// Adds explicit file references; it never silently switches the workspace.
    pub(crate) fn on_attach_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let session = self.driver.open_session_id();
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(crate::i18n::t("Attach file references", "附加文件引用").into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(session) = session else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                workspace.driver_do(|d, fx| d.attach_file_refs(&session, &paths, fx), cx);
            });
        })
        .detach();
    }

    pub(crate) fn on_picker_cancel(&mut self, cx: &mut Context<Self>) {
        self.picker_chat_session = None;
        self.project_picker = None;
        cx.notify();
    }

    pub(crate) fn on_picker_enter(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.project_picker = Some(crate::ui::project_picker::browse(Some(path)));
        cx.notify();
    }

    pub(crate) fn on_picker_up(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self
            .project_picker
            .as_ref()
            .and_then(|picker| picker.current.clone())
        else {
            return;
        };
        self.project_picker = Some(crate::ui::project_picker::browse(
            crate::ui::project_picker::parent_folder(&current),
        ));
        cx.notify();
    }

    pub(crate) fn on_picker_home(&mut self, cx: &mut Context<Self>) {
        self.project_picker = Some(crate::ui::project_picker::ProjectPicker::home());
        cx.notify();
    }

    /// Binds the first dropped directory, or the parent of a dropped file.
    pub(crate) fn on_drop_project(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        let folder = paths
            .iter()
            .find(|path| path.is_dir())
            .cloned()
            .or_else(|| {
                paths
                    .first()
                    .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            });
        let Some(folder) = folder else {
            return;
        };
        self.project_picker = None;
        self.driver_do(|d, fx| d.folder_picked(folder, None, fx), cx);
    }

    pub(crate) fn on_picker_roots(&mut self, cx: &mut Context<Self>) {
        self.project_picker = Some(crate::ui::project_picker::browse(None));
        cx.notify();
    }

    pub(crate) fn on_picker_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .project_picker
            .as_ref()
            .and_then(|picker| picker.current.clone())
        else {
            return;
        };
        if !path.is_dir() {
            return;
        }
        self.project_picker = None;
        let for_session = self.picker_chat_session.take();
        self.driver_do(|d, fx| d.folder_picked(path, for_session, fx), cx);
    }

    /// Opens one of the remembered recent projects.
    pub(crate) fn on_open_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.open_recent(project, fx), cx);
    }

    /// Opens the folder's newest chat, or starts a fresh one bound to it
    /// when the folder has none. Clicking a folder never rebinds the open
    /// chat — moving a chat's folder goes through the folder picker.
    pub(crate) fn on_open_workspace_folder(&mut self, project: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.open_workspace_folder(project, fx), cx);
    }
}

/// Two-click key for removing a folder together with its sessions.
fn root_delete_key(project: &str) -> String {
    format!("root:{project}")
}
