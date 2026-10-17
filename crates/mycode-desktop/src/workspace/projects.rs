//! Project- and session-shaped workspace handlers: the mention/skill
//! pipeline, message recall, session switching, and the project bindings
//! that drive the sidebar grouping.
use gpui_kit::AppContext as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{Context, Window};
use std::fmt::Write as _;

use mycode_app::{BridgeCommand, SessionId};

use crate::view_model::DesktopAction;
use crate::workspace::Workspace;

impl Workspace {
    /// Fires a project-file search when the active `@` fragment changed.
    pub(super) fn refresh_mention_search(&mut self, cx: &mut Context<Self>) {
        let (kind, fragment) = match self.vm.mention.as_ref() {
            Some(mention) => (mention.kind, mention.fragment.clone()),
            None => {
                self.mention_query = None;
                return;
            }
        };
        if self.mention_query.as_ref() == Some(&(kind, fragment.clone())) {
            return;
        }
        self.mention_query = Some((kind, fragment));
        if kind == crate::view_model::MentionKind::Command {
            // Reload skills so `/` lists them even before Settings was opened.
            // The reducer rebuilds the menu, including enabled MCP servers.
            self.refresh_skills(cx);
            return;
        }
        if kind != crate::view_model::MentionKind::File {
            return;
        }
        if self.vm.active.is_none() {
            return;
        }
        // Typing `@` walks the project tree. Wait until the fragment settles
        // so each keystroke does not start another full walk.
        self.mention_generation = self.mention_generation.wrapping_add(1);
        let generation = self.mention_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(180))
                .await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.mention_generation != generation {
                    return;
                }
                let Some(mention) = workspace.vm.mention.as_ref() else {
                    return;
                };
                if mention.kind != crate::view_model::MentionKind::File {
                    return;
                }
                let query = mention.fragment.clone();
                let Some(session_id) = workspace
                    .vm
                    .active
                    .as_ref()
                    .map(|conversation| conversation.session_id.clone())
                else {
                    return;
                };
                workspace.dispatch(BridgeCommand::SearchProjectFiles { session_id, query }, cx);
            });
        })
        .detach();
    }

    fn skill_roots(&self) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
        let workspace = self
            .vm
            .project_dir
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        (workspace, mycode_config::os_user_home())
    }

    pub(super) fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let skills = mycode_config::discover_skills(&workspace, user_home.as_deref())
            .into_iter()
            .map(|skill| crate::view_model::SkillEntry {
                slug: skill.slug,
                title: skill.title,
                path: skill.path.to_string_lossy().into_owned(),
                global: skill.global,
            })
            .collect();
        self.apply_action(DesktopAction::SkillsLoaded(skills), cx);
    }

    /// Enter accepts the highlighted file or command instead of sending an
    /// incomplete token. An empty menu still allows an ordinary message.
    pub(super) fn accept_open_mention(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mention) = self.vm.mention.clone() else {
            return false;
        };
        if mention.items.is_empty() {
            return false;
        }
        self.on_accept_mention(self.mention_highlight(), window, cx);
        true
    }

    /// Accepts one mention row: rewrites the draft (files) or runs the
    /// command (commands), then closes the menu.
    pub(crate) fn on_accept_mention(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mention) = self.vm.mention.take() else {
            return;
        };
        self.mention_query = None;
        let Some(item) = mention.items.get(index) else {
            return;
        };
        match item.group {
            crate::view_model::MentionGroup::File => {
                let mut text = self.vm.composer_draft.clone();
                if let Some(position) = text.rfind('@') {
                    text.replace_range(position.., &format!("@{} ", item.insert));
                }
                self.apply_action(DesktopAction::ComposerChanged(text.clone()), cx);
                self.composer
                    .update(cx, |state, cx| state.set_value(text, window, cx));
            }
            crate::view_model::MentionGroup::Skill => {
                if let Some(slug) = item.insert.strip_prefix('/') {
                    self.insert_skill_draft(slug, cx);
                    let draft = self.pending_composer_prefill.clone().unwrap_or_default();
                    self.apply_action(DesktopAction::ComposerChanged(draft), cx);
                }
            }
            _ => self.run_slash_command(&item.insert, cx),
        }
    }

    /// Runs one composer slash token: built-in commands, a skill, or MCP.
    ///
    /// The command palette accepts the same tokens as the `/` menu.
    pub(super) fn run_slash_command(&mut self, insert: &str, cx: &mut Context<Self>) {
        if insert == "/new" {
            if !crate::view_model::has_open_folder(&self.vm) {
                self.push_toast(
                    crate::i18n::t("Open a folder to get started.", "打开一个目录即可开始。"),
                    crate::workspace::ToastKind::Info,
                    cx,
                );
            } else {
                self.on_new_session(cx);
            }
        } else if insert == "/settings" {
            self.on_show_main_view(crate::view_model::MainView::Settings, cx);
        } else if insert == "/compact" {
            self.on_compact(cx);
        } else if let Some(spec) = insert.strip_prefix("mcp:") {
            self.insert_mcp_draft(spec, cx);
        } else if let Some(slug) = insert.strip_prefix('/') {
            self.insert_skill_draft(slug, cx);
        }
        if self.pending_composer_prefill.is_none() {
            self.pending_composer_prefill = Some(String::new());
        }
        let draft = self.pending_composer_prefill.clone().unwrap_or_default();
        self.apply_action(DesktopAction::ComposerChanged(draft), cx);
    }

    pub(crate) fn on_refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.refresh_skills(cx);
    }

    pub(crate) fn on_use_skill(&mut self, slug: &str, cx: &mut Context<Self>) {
        self.insert_skill_draft(slug, cx);
        self.on_show_main_view(crate::view_model::MainView::Chat, cx);
    }

    fn insert_skill_draft(&mut self, slug: &str, cx: &mut Context<Self>) {
        let (title, path) =
            if let Some(cached) = self.vm.skills.iter().find(|skill| skill.slug == slug) {
                (cached.title.clone(), cached.path.clone())
            } else {
                let (workspace, user_home) = self.skill_roots();
                let Some(skill) = mycode_config::discover_skills(&workspace, user_home.as_deref())
                    .into_iter()
                    .find(|skill| skill.slug == slug)
                else {
                    return;
                };
                (skill.title, skill.path.display().to_string())
            };
        // Native tools run directly: ask for a plain read of the skill file.
        // (The retired run_code control channel used to wrap this request.)
        let text = if crate::i18n::is_chinese() {
            format!("/{slug}\n\n遵循「{title}」技能。用 read 工具读取 {path} 并应用，再继续。\n")
        } else {
            format!(
                "/{slug}\n\nFollow the `{title}` skill. Read {path} with the read tool and apply it before continuing.\n"
            )
        };
        self.pending_composer_prefill = Some(text);
        cx.notify();
    }

    /// Prefills an explicit request to use one MCP server or one of its tools.
    fn insert_mcp_draft(&mut self, spec: &str, cx: &mut Context<Self>) {
        let (server, tool) = spec
            .split_once('/')
            .map(|(server, tool)| (server, Some(tool)))
            .unwrap_or((spec, None));
        let text = match tool {
            Some(tool) if crate::i18n::is_chinese() => {
                format!("使用 MCP 服务 `{server}` 上的工具 `{tool}`。\n")
            }
            Some(tool) => format!("Use the MCP tool `{tool}` on server `{server}`.\n"),
            None if crate::i18n::is_chinese() => format!("使用 MCP 服务 `{server}`。\n"),
            None => format!("Use the MCP server `{server}`.\n"),
        };
        self.pending_composer_prefill = Some(text);
        cx.notify();
    }

    /// Opens or closes the sidebar workspace switcher.
    pub(crate) fn on_toggle_workspace_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::WorkspaceMenuToggled(open), cx);
    }

    /// Creates a fresh named workspace and switches the sidebar to it.
    pub(crate) fn on_create_workspace(&mut self, cx: &mut Context<Self>) {
        // The serial skips names already taken so each new workspace is
        // distinguishable in the switcher.
        let mut serial = self.vm.workspaces.len() + 1;
        let name = loop {
            let candidate = format!("{} {serial}", crate::i18n::t("Workspace", "工作区"));
            if self
                .vm
                .workspaces
                .iter()
                .all(|workspace| workspace.name != candidate)
            {
                break candidate;
            }
            serial += 1;
        };
        let workspace = mycode_config::WorkspaceDef::generate(&name, Vec::new());
        self.apply_action(DesktopAction::WorkspaceCreated(workspace), cx);
        self.apply_action(DesktopAction::WorkspaceMenuToggled(false), cx);
        self.persist_ui_state(cx);
    }

    /// Switches the sidebar to another workspace.
    pub(crate) fn on_switch_workspace(&mut self, id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::WorkspaceSwitched(id.to_owned()), cx);
        self.apply_action(DesktopAction::WorkspaceMenuToggled(false), cx);
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    /// Turns the workspace menu into a rename editor for the active
    /// workspace.
    pub(crate) fn on_start_workspace_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) =
            crate::view_model::active_workspace(&self.vm).map(|workspace| workspace.name.clone())
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        cx.subscribe_in(&input, window, |workspace, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                workspace.on_confirm_workspace_rename(cx);
            }
        })
        .detach();
        input.update(cx, |state, cx| state.focus(window, cx));
        self.workspace_rename_input = Some(input);
        self.apply_action(DesktopAction::WorkspaceRenameToggled(true), cx);
    }

    /// Commits the rename editor's text as the active workspace's name.
    pub(crate) fn on_confirm_workspace_rename(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.workspace_rename_input.take() else {
            return;
        };
        let name = input.read(cx).value().trim().to_string();
        if !name.is_empty() {
            self.apply_action(DesktopAction::WorkspaceRenamed(name), cx);
        }
        self.apply_action(DesktopAction::WorkspaceRenameToggled(false), cx);
        self.persist_ui_state(cx);
    }

    /// Abandons the rename editor.
    pub(crate) fn on_cancel_workspace_rename(&mut self, cx: &mut Context<Self>) {
        self.workspace_rename_input = None;
        self.apply_action(DesktopAction::WorkspaceRenameToggled(false), cx);
    }

    /// Removes a workspace. Its sessions move to the first survivor.
    pub(crate) fn on_delete_workspace(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.vm.workspaces.len() < 2 {
            self.push_toast(
                crate::i18n::t("Keep at least one workspace", "至少保留一个工作区"),
                crate::workspace::ToastKind::Info,
                cx,
            );
            return;
        }
        self.apply_action(DesktopAction::WorkspaceRemoved(id.to_owned()), cx);
        self.apply_action(DesktopAction::WorkspaceMenuToggled(false), cx);
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    /// Binds one session to the workspace the sidebar shows. New sessions
    /// call this at creation; opening an unbound legacy session adopts the
    /// workspace it was opened from.
    pub(super) fn bind_session_workspace(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(workspace_id) =
            crate::view_model::active_workspace(&self.vm).map(|workspace| workspace.id.clone())
        else {
            return;
        };
        if self
            .vm
            .session_workspaces
            .iter()
            .any(|(existing, _)| existing == session_id)
        {
            return;
        }
        self.apply_action(
            DesktopAction::SessionWorkspaceBound {
                session_id: session_id.to_owned(),
                workspace_id,
            },
            cx,
        );
        self.persist_ui_state(cx);
    }

    /// Adds one folder to the workspace without moving a chat that already
    /// has its own working directory. An unbound chat adopts the folder.
    pub(crate) fn on_add_workspace_root(&mut self, project: &str, cx: &mut Context<Self>) {
        let project = project.trim();
        if project.is_empty() {
            return;
        }
        self.apply_action(DesktopAction::WorkspaceRootAdded(project.to_owned()), cx);
        if self.active_can_adopt(project)
            && let Some(session_id) = self.open_session_id()
        {
            self.attach_project(&session_id, project, cx);
            return;
        }
        self.persist_ui_state(cx);
    }

    /// Drops one linked folder, including the one the open chat works in.
    ///
    /// Removing that folder parks the chat: the desk comes back and the
    /// session keeps its binding, so opening the chat again re-adds the
    /// folder.
    pub(crate) fn on_remove_workspace_root(&mut self, project: &str, cx: &mut Context<Self>) {
        let chat_uses_it = self
            .vm
            .project_dir
            .as_deref()
            .is_some_and(|current| crate::view_model::same_project_path(current, project));
        self.apply_action(DesktopAction::WorkspaceRootRemoved(project.to_owned()), cx);
        if chat_uses_it {
            if self
                .focused_project
                .as_deref()
                .is_some_and(|focused| crate::view_model::same_project_path(focused, project))
            {
                self.focused_project = None;
            }
            self.park_conversation(cx);
            self.apply_action(DesktopAction::ActiveProjectChanged(None), cx);
            self.refresh_skills(cx);
        }
        self.persist_ui_state(cx);
    }

    /// Removes one directory from the remembered projects list.
    pub(crate) fn on_remove_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.dispatch(
            BridgeCommand::RemoveRecent {
                project: project.to_owned(),
            },
            cx,
        );
        self.apply_action(DesktopAction::RecentRemoved(project.to_owned()), cx);
    }

    pub(crate) fn on_new_session(&mut self, cx: &mut Context<Self>) -> bool {
        // A session needs a folder. The empty desk and sidebar hide this
        // action until one is open; the slash command uses the same gate.
        if !crate::view_model::has_open_folder(&self.vm) {
            return false;
        }
        // The welcome desk stays up while the open chat has no messages.
        // New task is already that chat. Another CreateSession lists a
        // second empty row. `true` means a session is ready: false is only
        // "no folder", which is what drops a held welcome draft.
        if crate::view_model::open_session_is_empty(&self.vm) {
            return true;
        }
        // New sessions inherit the active project so the sidebar grouping and
        // the tool working directory follow the project switcher.
        if self.vm.project_dir.is_some() {
            self.pending_project = self.vm.project_dir.clone();
        }
        self.dispatch(BridgeCommand::CreateSession, cx);
        true
    }

    /// Opens one session and ignores any conversation reply that is not it.
    pub(super) fn request_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.suppress_open = false;
        self.pending_open = Some(session_id.to_owned());
        // Opening adopts an unbound (pre-workspace) session into the
        // workspace it was opened from; bound sessions are untouched.
        self.bind_session_workspace(session_id, cx);
        if let Some(session_id) = SessionId::parse(session_id) {
            self.dispatch(BridgeCommand::OpenSession(session_id), cx);
        }
    }

    /// Makes the sidebar and tool directory follow one session's project.
    /// Other chats keep their own bindings, so several projects can stay open.
    pub(super) fn follow_session_project(&mut self, session_id: &str, cx: &mut Context<Self>) {
        // The sidebar shows the workspace this chat belongs to.
        if let Some(owner) = crate::view_model::workspace_of_session(&self.vm, session_id) {
            let id = owner.id.clone();
            self.apply_action(DesktopAction::WorkspaceSwitched(id), cx);
        }
        let Some(project) = self
            .vm
            .session_projects
            .iter()
            .find(|(id, _)| id == session_id)
            .map(|(_, project)| project.clone())
        else {
            return;
        };
        self.dispatch(
            BridgeCommand::SetProjectDir {
                session_id: session_id.to_owned(),
                path: Some(project.clone()),
            },
            cx,
        );
        self.apply_action(DesktopAction::WorkspaceRootAdded(project.clone()), cx);
        let already = self
            .vm
            .project_dir
            .as_deref()
            .is_some_and(|current| crate::view_model::same_project_path(current, &project));
        if already {
            self.persist_ui_state(cx);
            return;
        }
        self.apply_action(DesktopAction::ActiveProjectChanged(Some(project)), cx);
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    pub(crate) fn on_toggle_project_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProjectMenuToggled(open), cx);
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
        if self.vm.sending {
            self.push_toast(
                "Finish or stop the current turn before changing its folder.",
                crate::workspace::ToastKind::Info,
                cx,
            );
            return;
        }
        self.picker_chat_session = self.open_session_id();
        self.project_picker = Some(crate::ui::project_picker::browse(
            self.vm.project_dir.as_ref().map(std::path::PathBuf::from),
        ));
        cx.notify();
    }

    /// Adds explicit file references; it never silently switches the workspace.
    pub(crate) fn on_attach_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let session = self.open_session_id();
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach file references".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |workspace, cx| {
                if workspace
                    .vm
                    .active
                    .as_ref()
                    .map(|active| &active.session_id)
                    != session.as_ref()
                {
                    return;
                }
                let mut draft = workspace.vm.composer_draft.clone();
                for path in paths {
                    if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
                        draft.push(' ');
                    }
                    let _ = write!(draft, "@{} ", path.display());
                }
                workspace.pending_composer_prefill = Some(draft.clone());
                workspace.apply_action(DesktopAction::ComposerChanged(draft), cx);
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
        if !folder.is_dir() {
            return;
        }
        self.project_picker = None;
        self.on_add_workspace_root(&folder.to_string_lossy(), cx);
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
        if let Some(session) = self.picker_chat_session.take() {
            if !self.vm.sending
                && self
                    .vm
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id == session)
            {
                self.on_focus_workspace_folder(&path.to_string_lossy(), cx);
            } else {
                self.push_toast(
                    "The active conversation changed; its folder was not changed.",
                    crate::workspace::ToastKind::Info,
                    cx,
                );
            }
        } else {
            self.on_add_workspace_root(&path.to_string_lossy(), cx);
        }
    }

    /// Opens one of the remembered recent projects.
    pub(crate) fn on_open_recent(&mut self, project: &str, cx: &mut Context<Self>) {
        self.open_isolated_project(project, cx);
    }

    /// Makes one workspace folder the open chat's working directory.
    ///
    /// The folder stays a member of this workspace. The chat is not parked
    /// and no second session is started.
    pub(crate) fn on_focus_workspace_folder(&mut self, project: &str, cx: &mut Context<Self>) {
        let project = project.trim();
        if project.is_empty() {
            return;
        }
        self.focused_project = Some(project.to_owned());
        self.apply_action(DesktopAction::WorkspaceRootAdded(project.to_owned()), cx);
        self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
        let Some(session_id) = self.open_session_id() else {
            self.pending_project = Some(project.to_owned());
            self.dispatch(BridgeCommand::CreateSession, cx);
            self.refresh_skills(cx);
            self.persist_ui_state(cx);
            return;
        };
        self.apply_action(
            DesktopAction::WorkspaceFolderFocused {
                session_id: session_id.clone(),
                project: project.to_owned(),
            },
            cx,
        );
        self.dispatch(
            BridgeCommand::SetProjectDir {
                session_id: session_id.clone(),
                path: Some(project.to_owned()),
            },
            cx,
        );
        self.dispatch(BridgeCommand::ListResources { session_id }, cx);
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    /// Enters `project` without moving any other session onto it.
    ///
    /// The open chat is left alone when it already belongs here. An empty
    /// chat that has no folder yet can adopt this one. A chat that already
    /// has work, or that belongs to another folder, stays bound where it is;
    /// this folder gets its own session.
    fn open_isolated_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if self.active_is_project(project) {
            self.focused_project = Some(project.to_owned());
            self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
            self.refresh_skills(cx);
            self.persist_ui_state(cx);
            return;
        }
        if self.active_can_adopt(project) {
            let session_id = self.open_session_id().expect("active session");
            self.attach_project(&session_id, project, cx);
            return;
        }
        self.focus_project_chat(project, true, cx);
    }

    /// Shows `project` and either opens its newest chat or, when `create`
    /// is set and it has none, starts a fresh chat bound only to it.
    fn focus_project_chat(&mut self, project: &str, create: bool, cx: &mut Context<Self>) {
        self.focused_project = Some(project.to_owned());
        self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
        self.apply_action(DesktopAction::WorkspaceRootAdded(project.to_owned()), cx);
        if let Some(session_id) = crate::view_model::newest_session_in_project(
            &self.vm.sessions,
            &self.vm.session_projects,
            project,
        ) {
            let session_id = session_id.to_owned();
            if !self.active_is_project(project) {
                self.park_conversation(cx);
            }
            self.request_open_session(&session_id, cx);
        } else if create {
            self.park_conversation(cx);
            self.pending_project = Some(project.to_owned());
            self.dispatch(BridgeCommand::CreateSession, cx);
        } else if !self.active_is_project(project) {
            self.park_conversation(cx);
        }
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    fn active_is_project(&self, project: &str) -> bool {
        let Some(session_id) = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.as_str())
        else {
            return false;
        };
        crate::view_model::project_of_session(&self.vm.session_projects, session_id)
            .is_some_and(|bound| crate::view_model::same_project_path(bound, project))
    }

    /// An empty, idle chat with no folder can take the folder being opened.
    fn active_can_adopt(&self, project: &str) -> bool {
        if self.vm.sending {
            return false;
        }
        let Some(conversation) = self.vm.active.as_ref() else {
            return false;
        };
        if !conversation.entries.is_empty() {
            return false;
        }
        match crate::view_model::project_of_session(
            &self.vm.session_projects,
            &conversation.session_id,
        ) {
            Some(bound) => crate::view_model::same_project_path(bound, project),
            None => true,
        }
    }

    /// Hides the open chat. The session and any turn already running stay
    /// on the folder they were bound to.
    fn park_conversation(&mut self, cx: &mut Context<Self>) {
        self.pending_open = None;
        self.suppress_open = true;
        self.mention_query = None;
        self.pending_composer_prefill = Some(String::new());
        self.apply_action(DesktopAction::ConversationParked, cx);
    }

    /// Binds one new or still-unbound session to `project`.
    pub(super) fn attach_project(
        &mut self,
        session_id: &str,
        project: &str,
        cx: &mut Context<Self>,
    ) {
        self.focused_project = Some(project.to_owned());
        self.apply_action(
            DesktopAction::SessionProjectBound {
                session_id: session_id.to_owned(),
                project: project.to_owned(),
            },
            cx,
        );
        self.apply_action(DesktopAction::ProjectOpened(project.to_owned()), cx);
        self.apply_action(DesktopAction::WorkspaceRootAdded(project.to_owned()), cx);
        self.dispatch(
            BridgeCommand::SetProjectDir {
                session_id: session_id.to_owned(),
                path: Some(project.to_owned()),
            },
            cx,
        );
        self.dispatch(
            BridgeCommand::ListResources {
                session_id: session_id.to_owned(),
            },
            cx,
        );
        self.refresh_skills(cx);
        self.persist_ui_state(cx);
    }

    /// Persists the durable UI state projection.
    pub(super) fn persist_ui_state(&self, cx: &mut Context<Self>) {
        let state = mycode_config::UiState {
            recent_projects: self.vm.recents.clone(),
            last_project: self.vm.project_dir.clone(),
            auto_update: self.vm.auto_update,
            selected_provider: self.vm.selected_provider.clone(),
            selected_model: self.vm.selected_model.clone(),
            selected_reasoning: self.vm.default_reasoning.clone(),
            session_projects: self.vm.session_projects.clone(),
            session_models: self.vm.session_models.clone(),
            workspaces: self.vm.workspaces.clone(),
            session_workspaces: self.vm.session_workspaces.clone(),
            active_workspace: self.vm.active_workspace.clone(),
            recent_models: self.vm.recent_models.clone(),
            thinking_collapsed: self.vm.thinking_collapsed,
            window_size: self.vm.window_size,
        };
        self.dispatch(BridgeCommand::SaveUiState { state }, cx);
    }
}
