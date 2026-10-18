//! Project- and mention-shaped driving: the `@`/`/` composer menus, skill
//! discovery, workspace roots, and the session-to-project bindings that
//! decide which directory a chat's tools run in.
use std::fmt::Write as _;

use mycode_app::BridgeCommand;

use super::{DriverMsg, Effect, FrontendDriver, MENTION_SEARCH_DEBOUNCE, ToastKind};
use crate::view_model::{FrontendAction, MainView, MentionGroup, MentionKind};

impl FrontendDriver {
    /// Fires a project-file search when the active `@` fragment changed.
    /// Called from `act` after every `ComposerChanged`.
    pub(crate) fn refresh_mention_search(&mut self) {
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
        let menu_opened = self
            .mention_query
            .as_ref()
            .is_none_or(|(previous, _)| *previous != kind);
        self.mention_query = Some((kind, fragment));
        if kind == MentionKind::Command {
            // Reload skills so `/` lists them even before Settings was
            // opened — once when the menu opens, not on every keystroke:
            // discovery walks the skill directories on disk. The reducer
            // rebuilds the menu, including enabled MCP servers.
            if menu_opened {
                self.refresh_skills(&mut Vec::new());
            }
            return;
        }
        if kind != MentionKind::File || self.vm.active.is_none() {
            return;
        }
        // Typing `@` walks the project tree. Wait until the fragment
        // settles so each keystroke does not start another full walk.
        self.mention_generation = self.mention_generation.wrapping_add(1);
        let generation = self.mention_generation;
        self.schedule(MENTION_SEARCH_DEBOUNCE, move || {
            DriverMsg::MentionSearchTick { generation }
        });
    }

    /// The debounce elapsed; dispatch the search if the fragment is still
    /// the one this tick was armed for.
    pub(crate) fn fire_mention_search(&mut self) {
        let Some(mention) = self.vm.mention.as_ref() else {
            return;
        };
        if mention.kind != MentionKind::File {
            return;
        }
        let query = mention.fragment.clone();
        let Some(session_id) = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
        else {
            return;
        };
        self.dispatch(BridgeCommand::SearchProjectFiles { session_id, query });
    }

    /// The directories skill discovery scans: the open project's
    /// `.mycode/skills`-style root and the user's home `.agents`.
    fn skill_roots(&self) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
        let workspace = self
            .vm
            .project_dir
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        (workspace, mycode_config::os_user_home())
    }

    /// Re-scans the skill directories and folds the result into the state.
    pub fn refresh_skills(&mut self, out: &mut Vec<Effect>) {
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
        self.act(FrontendAction::SkillsLoaded(skills), out);
    }

    /// Public surface for "rescan skills" affordances (the Skills page's
    /// refresh, opening that settings section).
    pub fn reload_skills(&mut self, out: &mut Vec<Effect>) {
        self.refresh_skills(out);
    }

    /// Accepts one mention row: rewrites the draft (files) or runs the
    /// command (commands), then closes the menu. The frontend computes the
    /// highlighted index from its own keyboard navigation.
    pub fn accept_mention(&mut self, index: usize, out: &mut Vec<Effect>) {
        let Some(mention) = self.vm.mention.take() else {
            return;
        };
        self.mention_query = None;
        let Some(item) = mention.items.get(index) else {
            return;
        };
        match item.group {
            MentionGroup::File => {
                let mut text = self.vm.composer_draft.clone();
                if let Some(position) = text.rfind('@') {
                    text.replace_range(position.., &format!("@{} ", item.insert));
                }
                self.act(FrontendAction::ComposerChanged(text.clone()), out);
                out.push(Effect::ComposerSet { text, focus: true });
            }
            MentionGroup::Skill => {
                if let Some(slug) = item.insert.strip_prefix('/') {
                    self.insert_skill_draft(slug, out);
                    let draft = self.pending_composer_prefill.clone().unwrap_or_default();
                    self.act(FrontendAction::ComposerChanged(draft), out);
                }
            }
            _ => self.run_slash_command(&item.insert, out),
        }
    }

    /// Runs one composer slash token: built-in commands, a skill, or MCP.
    ///
    /// The command palette accepts the same tokens as the `/` menu.
    pub fn run_slash_command(&mut self, insert: &str, out: &mut Vec<Effect>) {
        if insert == "/new" {
            if !crate::view_model::has_open_folder(&self.vm) {
                self.toast(
                    crate::i18n::t("Open a folder to get started.", "打开一个目录即可开始。"),
                    ToastKind::Info,
                    out,
                );
            } else {
                self.new_session(out);
            }
        } else if insert == "/settings" {
            self.show_main_view(MainView::Settings, out);
        } else if insert == "/compact" {
            self.compact(out);
        } else if let Some(spec) = insert.strip_prefix("mcp:") {
            self.insert_mcp_draft(spec, out);
        } else if let Some(slug) = insert.strip_prefix('/') {
            self.insert_skill_draft(slug, out);
        }
        if self.pending_composer_prefill.is_none() {
            self.pending_composer_prefill = Some(String::new());
        }
        let draft = self.pending_composer_prefill.clone().unwrap_or_default();
        out.push(Effect::ComposerSet {
            text: draft.clone(),
            focus: true,
        });
        self.act(FrontendAction::ComposerChanged(draft), out);
    }

    /// Inserts a skill-use request into the composer without sending.
    pub fn use_skill(&mut self, slug: &str, out: &mut Vec<Effect>) {
        self.insert_skill_draft(slug, out);
        self.show_main_view(MainView::Chat, out);
    }

    /// Prefills an explicit request to apply one skill.
    fn insert_skill_draft(&mut self, slug: &str, out: &mut Vec<Effect>) {
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
        let text = if crate::i18n::is_chinese() {
            format!("/{slug}\n\n遵循「{title}」技能。用 read 工具读取 {path} 并应用，再继续。\n")
        } else {
            format!(
                "/{slug}\n\nFollow the `{title}` skill. Read {path} with the read tool and apply it before continuing.\n"
            )
        };
        self.pending_composer_prefill = Some(text.clone());
        out.push(Effect::ComposerSet { text, focus: false });
    }

    /// Prefills an explicit request to use one MCP server or one of its tools.
    fn insert_mcp_draft(&mut self, spec: &str, out: &mut Vec<Effect>) {
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
        self.pending_composer_prefill = Some(text.clone());
        out.push(Effect::ComposerSet { text, focus: false });
    }

    /// Appends explicit `@file` references to the draft. Never silently
    /// switches the workspace.
    pub fn attach_file_refs(
        &mut self,
        session_id: &str,
        paths: &[std::path::PathBuf],
        out: &mut Vec<Effect>,
    ) {
        if self
            .vm
            .active
            .as_ref()
            .map(|active| active.session_id.as_str())
            != Some(session_id)
        {
            return;
        }
        let mut draft = self.vm.composer_draft.clone();
        for path in paths {
            if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
                draft.push(' ');
            }
            let _ = write!(draft, "@{} ", path.display());
        }
        self.pending_composer_prefill = Some(draft.clone());
        out.push(Effect::ComposerSet {
            text: draft.clone(),
            focus: false,
        });
        self.act(FrontendAction::ComposerChanged(draft), out);
    }

    // ---- workspace roots and session-project bindings ----

    /// Adds one folder to the workspace without moving a chat that already
    /// has its own working directory. An unbound chat adopts the folder.
    pub fn add_workspace_root(&mut self, project: &str, out: &mut Vec<Effect>) {
        let project = project.trim();
        if project.is_empty() {
            return;
        }
        self.act(FrontendAction::WorkspaceRootAdded(project.to_owned()), out);
        if self.active_can_adopt(project)
            && let Some(session_id) = self.open_session_id()
        {
            self.attach_project(&session_id, project, out);
            return;
        }
        self.persist_ui_state();
    }

    /// Drops one linked folder, including the one the open chat works in.
    ///
    /// The folder's chats go with it: every session bound to that directory
    /// is deleted. Sessions bound to other folders are untouched.
    pub fn remove_workspace_root(&mut self, project: &str, out: &mut Vec<Effect>) {
        let chat_uses_it = self
            .vm
            .project_dir
            .as_deref()
            .is_some_and(|current| crate::view_model::same_project_path(current, project));
        let removed_sessions = crate::view_model::sessions_removed_with_root(&self.vm, project);
        self.act(
            FrontendAction::WorkspaceRootRemoved(project.to_owned()),
            out,
        );
        if chat_uses_it {
            self.park_conversation(out);
            self.act(FrontendAction::ActiveProjectChanged(None), out);
            self.refresh_skills(out);
        }
        for session_id in removed_sessions {
            self.delete_session(&session_id);
        }
        self.persist_ui_state();
    }

    /// Removes one directory from the remembered projects list.
    pub fn remove_recent(&mut self, project: &str, out: &mut Vec<Effect>) {
        self.dispatch(BridgeCommand::RemoveRecent {
            project: project.to_owned(),
        });
        self.act(FrontendAction::RecentRemoved(project.to_owned()), out);
    }

    /// Opens one of the remembered recent projects.
    pub fn open_recent(&mut self, project: &str, out: &mut Vec<Effect>) {
        self.open_isolated_project(project, out);
    }

    /// Opens the folder's newest chat, or starts a fresh one bound to it
    /// when the folder has none. Clicking a folder never rebinds the open
    /// chat — moving a chat's folder goes through the folder picker.
    pub fn open_workspace_folder(&mut self, project: &str, out: &mut Vec<Effect>) {
        self.focus_project_chat(project, true, out);
    }

    /// Makes one workspace folder the open chat's working directory.
    ///
    /// The chat is not parked and no second session is started.
    pub fn focus_workspace_folder(&mut self, project: &str, out: &mut Vec<Effect>) {
        let project = project.trim();
        if project.is_empty() {
            return;
        }
        self.act(FrontendAction::WorkspaceRootAdded(project.to_owned()), out);
        self.act(FrontendAction::ProjectOpened(project.to_owned()), out);
        let Some(session_id) = self.open_session_id() else {
            self.pending_open = None;
            self.pending_project = Some(project.to_owned());
            self.dispatch(BridgeCommand::CreateSession);
            self.refresh_skills(out);
            self.persist_ui_state();
            return;
        };
        self.act(
            FrontendAction::WorkspaceFolderFocused {
                session_id: session_id.clone(),
                project: project.to_owned(),
            },
            out,
        );
        self.dispatch(BridgeCommand::SetProjectDir {
            session_id: session_id.clone(),
            path: Some(project.to_owned()),
        });
        self.dispatch(BridgeCommand::ListResources { session_id });
        self.refresh_skills(out);
        self.persist_ui_state();
    }

    /// Enters `project` without moving any other session onto it.
    ///
    /// The open chat is left alone when it already belongs here. An empty
    /// chat that has no folder yet can adopt this one. A chat that already
    /// has work, or that belongs to another folder, stays bound where it is;
    /// this folder gets its own session.
    pub(crate) fn open_isolated_project(&mut self, project: &str, out: &mut Vec<Effect>) {
        if self.active_is_project(project) {
            self.act(FrontendAction::ProjectOpened(project.to_owned()), out);
            self.refresh_skills(out);
            self.persist_ui_state();
            return;
        }
        if self.active_can_adopt(project) {
            let session_id = self.open_session_id().expect("active session");
            self.attach_project(&session_id, project, out);
            return;
        }
        self.focus_project_chat(project, true, out);
    }

    /// Shows `project` and either opens its newest chat or, when `create`
    /// is set and it has none, starts a fresh chat bound only to it.
    pub(crate) fn focus_project_chat(
        &mut self,
        project: &str,
        create: bool,
        out: &mut Vec<Effect>,
    ) {
        self.act(FrontendAction::ProjectOpened(project.to_owned()), out);
        self.act(FrontendAction::WorkspaceRootAdded(project.to_owned()), out);
        if let Some(session_id) = crate::view_model::newest_session_in_project(
            &self.vm.sessions,
            &self.vm.session_projects,
            project,
        ) {
            let session_id = session_id.to_owned();
            if !self.active_is_project(project) {
                self.park_conversation(out);
            }
            self.request_open_session(&session_id);
        } else if create {
            self.park_conversation(out);
            self.pending_project = Some(project.to_owned());
            self.dispatch(BridgeCommand::CreateSession);
        } else if !self.active_is_project(project) {
            self.park_conversation(out);
        }
        self.refresh_skills(out);
        self.persist_ui_state();
    }

    /// Whether the open chat already works inside `project`.
    pub(crate) fn active_is_project(&self, project: &str) -> bool {
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
    pub(crate) fn active_can_adopt(&self, project: &str) -> bool {
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
    /// on the folder they were bound to. Clearing `pending_open` makes the
    /// reply scope drop any in-flight open: the parked desk must not be
    /// replaced by a conversation the user already left.
    pub fn park_conversation(&mut self, out: &mut Vec<Effect>) {
        self.pending_open = None;
        self.mention_query = None;
        self.pending_composer_prefill = Some(String::new());
        out.push(Effect::ComposerSet {
            text: String::new(),
            focus: false,
        });
        self.act(FrontendAction::ConversationParked, out);
    }

    /// Binds one new or still-unbound session to `project`.
    pub(crate) fn attach_project(
        &mut self,
        session_id: &str,
        project: &str,
        out: &mut Vec<Effect>,
    ) {
        self.act(
            FrontendAction::SessionProjectBound {
                session_id: session_id.to_owned(),
                project: project.to_owned(),
            },
            out,
        );
        self.act(FrontendAction::ProjectOpened(project.to_owned()), out);
        self.act(FrontendAction::WorkspaceRootAdded(project.to_owned()), out);
        self.dispatch(BridgeCommand::SetProjectDir {
            session_id: session_id.to_owned(),
            path: Some(project.to_owned()),
        });
        self.dispatch(BridgeCommand::ListResources {
            session_id: session_id.to_owned(),
        });
        self.refresh_skills(out);
        self.persist_ui_state();
    }

    /// The chat-folder picker answered: rebind when it was opened for the
    /// still-active chat, else this is a plain "add folder" pick.
    pub fn folder_picked(
        &mut self,
        path: std::path::PathBuf,
        for_session: Option<String>,
        out: &mut Vec<Effect>,
    ) {
        if !path.is_dir() {
            return;
        }
        match for_session {
            Some(session)
                if !self.vm.sending
                    && self
                        .vm
                        .active
                        .as_ref()
                        .is_some_and(|active| active.session_id == session) =>
            {
                self.focus_workspace_folder(&path.to_string_lossy(), out);
            }
            Some(_) => self.toast(
                "The active conversation changed; its folder was not changed.",
                ToastKind::Info,
                out,
            ),
            None => self.add_workspace_root(&path.to_string_lossy(), out),
        }
    }

    /// Switches the main area between chat and settings. Entering Settings
    /// refreshes skills so the page is current.
    pub fn show_main_view(&mut self, view: MainView, out: &mut Vec<Effect>) {
        self.act(FrontendAction::ShowMainView(view), out);
        if view == MainView::Settings {
            self.refresh_skills(out);
        }
    }
}
