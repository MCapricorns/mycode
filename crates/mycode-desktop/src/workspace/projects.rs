//! Project- and session-shaped workspace handlers: the mention/skill
//! pipeline, message recall, session switching, and the project bindings
//! that drive the sidebar grouping.
use gpui_kit::AppContext as _;
use gpui_kit::component::input::InputState;
use gpui_kit::{Context, Window};

use mycode_app::{BranchId, BridgeCommand, SessionId};

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
        if self.mention_query.as_deref() == Some(fragment.as_str()) {
            return;
        }
        self.mention_query = Some(fragment.clone());
        if kind == crate::view_model::MentionKind::Command {
            self.merge_skill_commands(&fragment, cx);
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
        let user_home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from);
        (workspace, user_home)
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

    fn merge_skill_commands(&mut self, fragment: &str, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let skills = mycode_config::discover_skills(&workspace, user_home.as_deref());
        if let Some(mention) = self.vm.mention.as_mut() {
            for skill in skills {
                if !skill.slug.starts_with(fragment) {
                    continue;
                }
                let insert = format!("/{}", skill.slug);
                if mention
                    .items
                    .iter()
                    .any(|(existing, _)| existing == &insert)
                {
                    continue;
                }
                mention
                    .items
                    .push((insert, format!("/{} · {}", skill.slug, skill.title)));
            }
        }
        cx.notify();
    }

    /// Accepts one mention row: rewrites the draft (files) or runs the
    /// command (commands), then closes the menu.
    pub(crate) fn on_accept_mention(
        &mut self,
        insert: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mention) = self.vm.mention.take() else {
            return;
        };
        self.mention_query = None;
        match mention.kind {
            crate::view_model::MentionKind::File => {
                let mut text = self.vm.composer_draft.clone();
                if let Some(position) = text.rfind('@') {
                    text.replace_range(position.., &format!("@{insert} "));
                }
                self.pending_composer_prefill = Some(text);
                cx.notify();
            }
            crate::view_model::MentionKind::Command => {
                if insert == "/new" {
                    self.on_new_session(cx);
                } else if insert == "/settings" {
                    self.on_show_main_view(crate::view_model::MainView::Settings, cx);
                } else if let Some(slug) = insert.strip_prefix('/') {
                    self.insert_skill_draft(slug, cx);
                }
            }
        }
    }

    pub(crate) fn on_refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.refresh_skills(cx);
    }

    pub(crate) fn on_use_skill(&mut self, slug: &str, cx: &mut Context<Self>) {
        self.insert_skill_draft(slug, cx);
        self.on_show_main_view(crate::view_model::MainView::Chat, cx);
    }

    fn insert_skill_draft(&mut self, slug: &str, cx: &mut Context<Self>) {
        let (workspace, user_home) = self.skill_roots();
        let Some(skill) = mycode_config::discover_skills(&workspace, user_home.as_deref())
            .into_iter()
            .find(|skill| skill.slug == slug)
        else {
            return;
        };
        let path = skill.path.display();
        self.pending_composer_prefill = Some(format!(
            "/{slug}\n\nFollow the `{title}` skill. Read `{path}` and apply it before continuing.\n",
            title = skill.title
        ));
        cx.notify();
    }

    /// Rewinds the chat to just before the user message at `index` and
    /// prefills the composer with its text (编辑). Workspace files stay as
    /// they are. The first message has no prior event to rewind to and is
    /// ignored.
    pub(crate) fn on_edit_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, true, cx);
    }

    /// Rewinds the chat to just before the user message at `index` (撤回).
    /// Workspace files stay as they are.
    pub(crate) fn on_recall_message(&mut self, index: usize, cx: &mut Context<Self>) {
        self.recall_at(index, false, cx);
    }

    fn recall_at(&mut self, index: usize, edit: bool, cx: &mut Context<Self>) {
        if self.vm.sending {
            return;
        }
        let Some(conversation) = self.vm.active.clone() else {
            return;
        };
        if index == 0 || index >= conversation.entries.len() {
            return;
        }
        if conversation.entries[index].kind != crate::view_model::EntryKind::UserMessage {
            return;
        }
        let Some(session) = SessionId::parse(&conversation.session_id) else {
            return;
        };
        let Some(branch) = BranchId::parse(&conversation.branch_id) else {
            return;
        };
        let expected_head = crate::workspace::parse_head(&conversation.head);
        let to_event = conversation.entries[index - 1].event_id.clone();
        let edit = if edit {
            Some(conversation.entries[index].text.to_string())
        } else {
            None
        };
        self.dispatch(
            BridgeCommand::RecallMessage {
                session,
                branch,
                expected_head,
                to_event,
                edit,
            },
            cx,
        );
    }

    /// Deletes one session's durable data and drops it if active.
    pub(crate) fn on_delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self
            .vm
            .active
            .as_ref()
            .is_some_and(|conversation| conversation.session_id == session_id)
        {
            self.apply_action(DesktopAction::SessionDeleted, cx);
        }
        // Drop the remembered bindings optimistically: the core forgets them
        // too, and a later UI-state save must not resurrect stale ids.
        self.apply_action(
            DesktopAction::SessionBindingsForgotten(session_id.to_owned()),
            cx,
        );
        self.dispatch(
            BridgeCommand::DeleteSession {
                session_id: session_id.to_owned(),
            },
            cx,
        );
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
            let candidate = format!("{} {serial}", crate::i18n::t("Workspace", "工区"));
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
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(current));
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
                crate::i18n::t("Keep at least one workspace", "至少保留一个工区"),
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
            && let Some(session_id) = self
                .vm
                .active
                .as_ref()
                .map(|conversation| conversation.session_id.clone())
        {
            self.attach_project(&session_id, project, cx);
            return;
        }
        self.persist_ui_state(cx);
    }

    /// Drops one linked folder. The open chat's working directory stays.
    pub(crate) fn on_remove_workspace_root(&mut self, project: &str, cx: &mut Context<Self>) {
        if self
            .vm
            .project_dir
            .as_deref()
            .is_some_and(|current| crate::view_model::same_project_path(current, project))
        {
            self.push_toast(
                "This chat is using that folder",
                crate::workspace::ToastKind::Info,
                cx,
            );
            return;
        }
        self.apply_action(DesktopAction::WorkspaceRootRemoved(project.to_owned()), cx);
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

    pub(crate) fn on_new_session(&mut self, cx: &mut Context<Self>) {
        // A session needs a folder. The empty desk and sidebar hide this
        // action until one is open; the slash command uses the same gate.
        if !crate::view_model::has_open_folder(&self.vm) {
            return;
        }
        // New sessions inherit the active project so the sidebar grouping and
        // the tool working directory follow the project switcher.
        if self.vm.project_dir.is_some() {
            self.pending_project = self.vm.project_dir.clone();
        }
        self.dispatch(BridgeCommand::CreateSession, cx);
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
        self.project_picker = Some(crate::ui::project_picker::ProjectPicker::open());
        cx.notify();
    }

    pub(crate) fn on_picker_cancel(&mut self, cx: &mut Context<Self>) {
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
        self.on_add_workspace_root(&path.to_string_lossy(), cx);
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
        let Some(session_id) = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
        else {
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
            let session_id = self
                .vm
                .active
                .as_ref()
                .map(|conversation| conversation.session_id.clone())
                .expect("active session");
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
            session_projects: self.vm.session_projects.clone(),
            // The legacy root list mirrors the active workspace so a
            // downgrade build still shows a sensible folder set.
            workspace_roots: self.vm.workspace_roots.clone(),
            workspaces: self.vm.workspaces.clone(),
            session_workspaces: self.vm.session_workspaces.clone(),
            trusted_projects: self.vm.trusted_projects.clone(),
            active_workspace: self.vm.active_workspace.clone(),
            recent_models: self.vm.recent_models.clone(),
            starred_models: self.vm.starred_models.clone(),
        };
        self.dispatch(BridgeCommand::SaveUiState { state }, cx);
    }
}
