//! The workspace window view: title bar, sessions sidebar, chat column,
//! right inspector, and the full-page settings view.
use std::collections::{HashMap, HashSet};

use gpui_kit::component::Root;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Bounds, Context, Entity, Pixels, Window, WindowBounds};
use gpui_kit::{px, size};
use mycode_app::{HeadStamp, SessionEventId};
use mycode_config::HomeLayout;

use crate::ui::{BackendForm, McpForm, ProviderForm};
use crate::view_model::{DesktopAction, MainView, WorkspaceState, reduce};
use mycode_app::{BridgeCommand, BridgeEventRx, CoreBridge};

mod bridge;
mod git;
mod projects;
mod settings_editor;
mod updates_data;

/// Preferred first-window size. The origin is computed from the primary
/// display so the window opens in the middle of the usable area.
const WINDOW_SIZE: gpui_kit::Size<Pixels> = size(px(1520.), px(960.));

/// Centers the preferred size on the primary display's usable area, shrinking
/// it when the screen is smaller than that size.
fn startup_bounds(cx: &App) -> Bounds<Pixels> {
    let Some(display) = cx.primary_display() else {
        return Bounds {
            origin: gpui_kit::point(px(80.), px(48.)),
            size: WINDOW_SIZE,
        };
    };
    let screen = display.visible_bounds();
    let width = fit_edge(screen.size.width, WINDOW_SIZE.width, px(960.));
    let height = fit_edge(screen.size.height, WINDOW_SIZE.height, px(560.));
    Bounds::centered_at(screen.center(), size(width, height))
}

fn fit_edge(available: Pixels, desired: Pixels, floor: Pixels) -> Pixels {
    let room = (available - px(64.)).max(floor.min(available));
    desired.min(room)
}

/// Opens the main window over one owned home.
///
/// # Panics
///
/// Panics when the window cannot open; the process has no useful headless
/// fallback by design.
pub fn open_window(home: HomeLayout, cx: &mut App) {
    let (bridge, events) = CoreBridge::start(home.clone());
    // The custom titlebar owns dragging and window controls, so the system
    // titlebar is hidden (`appears_transparent`).
    let mut options = gpui_kit::component::TitleBar::window_options();
    // Open as a regular window at the default bounds: the maximize-on-open
    // workaround for gpui's stale-scale sizing is retired by user request
    // (DPI edge cases accepted); the user can maximize manually.
    options.window_bounds = Some(WindowBounds::Windowed(startup_bounds(cx)));
    options.window_min_size = Some(size(px(960.), px(560.)));
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some("MYCode Harness".into());
    }
    cx.open_window(options, |window, cx| {
        // `init` installs the stock light theme. The product paints dark only,
        // then pushes that theme (and its code-block highlighter) into Base.
        Theme::change(ThemeMode::Dark, Some(window), cx);
        crate::ui::desk::apply(Theme::global_mut(cx));
        Theme::sync_base(cx);
        let workspace = Workspace::new(home, bridge, events, window, cx);
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("open the MYCode window");
}

/// One bottom-right notice. It leaves the screen on its own timer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Info,
    Error,
}

pub(crate) struct Toast {
    pub(crate) id: u64,
    pub(crate) text: String,
    pub(crate) kind: ToastKind,
}

/// Scroll position captured before older entries are prepended.
struct ScrollHold {
    offset_y: Pixels,
    content_height: Pixels,
}

/// The main workspace view.
pub struct Workspace {
    vm: WorkspaceState,
    /// Owned home the process opened with. Role discovery reads `agents/` here.
    home: HomeLayout,
    bridge: CoreBridge,
    /// Root focus so key events (Escape) reach the workspace node even when
    /// no input holds focus.
    focus_handle: gpui_kit::FocusHandle,
    composer: Entity<TextareaState>,
    /// Whether the composer placeholder is the in-turn steer hint.
    pub(crate) composer_steer: bool,
    ua_input: Option<Entity<InputState>>,
    ua_sync_pending: bool,
    provider_form: Option<Entity<ProviderForm>>,
    backend_form: Option<Entity<BackendForm>>,
    mcp_form: Option<Entity<McpForm>>,
    mcp_key_input: Option<Entity<InputState>>,
    mcp_json_input: Option<Entity<TextareaState>>,
    web_key_inputs: HashMap<String, Entity<InputState>>,
    /// Vendor ids whose empty replace-key field is open. A stored key is
    /// never written back into the field.
    web_key_replace: HashSet<String>,
    preset_key_input: Option<Entity<InputState>>,
    preset_search_input: Option<Entity<InputState>>,
    /// Search box inside the model step of the shared picker.
    model_picker_input: Option<Entity<InputState>>,
    /// Top-bar filter for the settings navigation.
    settings_search_input: Option<Entity<InputState>>,
    /// Sidebar filter over session titles already loaded from the index.
    session_filter_input: Option<Entity<InputState>>,
    /// Filter for the add-from-catalog model checklist.
    preset_model_search_input: Option<Entity<InputState>>,
    provider_key_inputs: HashMap<String, Entity<InputState>>,
    /// Provider ids whose empty replace-key field is open.
    provider_key_replace: HashSet<String>,
    /// Endpoint fields for provider detail. Seeded from the base URL only;
    /// a stored API key is never written here.
    provider_endpoint_inputs: HashMap<String, Entity<InputState>>,
    /// Last base URL copied into each endpoint field from settings.
    provider_endpoint_seed: HashMap<String, String>,
    /// Roles resolved from built-ins, the home `agents` directory, and the
    /// open project's `.mycode/agents`. `None` until settings asks for them.
    agent_roles: Option<mycode_config::RoleCatalog>,
    /// Project directory the cached role catalog was built for.
    agent_roles_project: Option<String>,
    /// The Agents section was opened again, or the project changed.
    agent_roles_stale: bool,
    /// Startup veil. A click, Escape, or the timed fade drops it.
    /// Reduced motion dismisses it before the first paint.
    splash: crate::ui::SplashGate,
    /// The next model-step render should clear and focus the search box.
    picker_focus_pending: bool,
    ask_input: Option<Entity<InputState>>,
    pending_project: Option<String>,
    /// Folder the user last chose. Opens for other folders are ignored.
    focused_project: Option<String>,
    /// Rename editor for the active workspace; lives only while the
    /// workspace menu is in rename mode.
    pub(crate) workspace_rename_input: Option<Entity<InputState>>,
    /// Session id of the conversation open currently in flight.
    pending_open: Option<String>,
    /// Drop conversation replies until the next explicit open. Set when the
    /// user leaves a folder's chat without asking for another one.
    suppress_open: bool,
    /// Draft restored by 撤回修改, applied on the next render (needs a window).
    pending_composer_prefill: Option<String>,
    /// Settings edit epoch captured when the in-flight save was dispatched.
    settings_save_epoch: u64,
    /// Last `@` fragment already searched, to dedupe bridge dispatches.
    mention_query: Option<String>,
    /// Bumped on each composer edit so a stale mention timer does not search.
    mention_generation: u64,
    pending_catalog_refresh: bool,
    /// The in-flight check was started from About, so its result may toast.
    manual_update_check: bool,
    toasts: Vec<Toast>,
    next_toast_id: u64,
    /// In-app folder browser. `None` while the native dialog is not used.
    pub(crate) project_picker: Option<crate::ui::project_picker::ProjectPicker>,
    /// Keeps the conversation column glued to the newest entry while a turn
    /// streams; without it new content grows below the fold.
    conversation_scroll: gpui_kit::ScrollHandle,
    /// Captured when older entries are prepended. The chat column applies it
    /// in the same frame, after layout and before the scroller positions its
    /// children. gpui's offset grows more negative toward the bottom, so the
    /// correction subtracts the content-height growth.
    scroll_hold: Option<ScrollHold>,
    /// Latest git status for the open folder.
    git: crate::git_status::GitSnapshot,
    /// Platform watcher for the open folder. Dropping it stops refresh.
    git_watcher: Option<git::Watcher>,
    /// Bumped when the watched folder changes so a stale read is dropped.
    git_generation: u64,
    /// A `git status` process is running.
    git_status_inflight: bool,
    /// The tree changed again while a status read was in flight.
    git_status_pending: bool,
    /// Path whose diff is shown in the changes panel.
    git_diff_path: Option<String>,
    git_diff: String,
    /// Bumped when the diff target changes so a stale diff is dropped.
    git_diff_generation: u64,
}

impl Workspace {
    /// Builds the workspace and issues the initial core loads.
    pub fn new(
        home: HomeLayout,
        bridge: CoreBridge,
        events: BridgeEventRx,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message MYCode")
                .auto_grow(1, 10)
                .submit_on_enter(true)
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let workspace = cx.new(|_| Self {
            vm: WorkspaceState::default(),
            home,
            bridge,
            focus_handle,
            composer,
            composer_steer: false,
            ua_input: None,
            ua_sync_pending: false,
            provider_form: None,
            backend_form: None,
            mcp_form: None,
            mcp_key_input: None,
            mcp_json_input: None,
            web_key_inputs: HashMap::new(),
            web_key_replace: HashSet::new(),
            preset_key_input: None,
            preset_search_input: None,
            model_picker_input: None,
            settings_search_input: None,
            session_filter_input: None,
            preset_model_search_input: None,
            provider_key_inputs: HashMap::new(),
            provider_key_replace: HashSet::new(),
            provider_endpoint_inputs: HashMap::new(),
            provider_endpoint_seed: HashMap::new(),
            agent_roles: None,
            agent_roles_project: None,
            agent_roles_stale: false,
            splash: crate::ui::SplashGate::new(),
            picker_focus_pending: false,
            ask_input: None,
            pending_project: None,
            focused_project: None,
            workspace_rename_input: None,
            pending_open: None,
            suppress_open: false,
            pending_composer_prefill: None,
            settings_save_epoch: 0,
            mention_query: None,
            mention_generation: 0,
            pending_catalog_refresh: false,
            manual_update_check: false,
            toasts: Vec::new(),
            next_toast_id: 0,
            project_picker: None,
            conversation_scroll: gpui_kit::ScrollHandle::new(),
            scroll_hold: None,
            git: crate::git_status::GitSnapshot::empty(crate::i18n::t("No folder", "未打开目录")),
            git_watcher: None,
            git_generation: 0,
            git_status_inflight: false,
            git_status_pending: false,
            git_diff_path: None,
            git_diff: String::new(),
            git_diff_generation: 0,
        });
        workspace.update(cx, |workspace, cx| {
            let filter = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::t("Search sessions", "搜索会话"))
            });
            cx.subscribe_in(&filter, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(DesktopAction::SessionFilterChanged(text), cx);
                }
            })
            .detach();
            workspace.session_filter_input = Some(filter);
            let composer = workspace.composer.clone();
            cx.subscribe_in(&composer, window, |workspace, _, event, window, cx| {
                workspace.on_composer_event(event, window, cx);
            })
            .detach();
            workspace.spawn_event_pump(events, cx);
            workspace.spawn_update_recheck(cx);
            workspace.dispatch(BridgeCommand::ListSessions, cx);
            workspace.dispatch(BridgeCommand::LoadSettings, cx);
            workspace.dispatch(BridgeCommand::LoadUiState, cx);
            workspace.dispatch(BridgeCommand::GetCatalog, cx);
        });
        workspace
    }

    /// Parks until the core sends, then folds that burst into state.
    fn spawn_event_pump(&self, events: BridgeEventRx, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(_) => return,
                };
                if this
                    .update(cx, |workspace, cx| workspace.apply_event(event, cx))
                    .is_err()
                {
                    return;
                }
                while let Ok(event) = events.try_recv() {
                    if this
                        .update(cx, |workspace, cx| workspace.apply_event(event, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
        .detach();
    }

    pub(crate) fn git(&self) -> &crate::git_status::GitSnapshot {
        &self.git
    }

    pub(crate) fn git_diff_path(&self) -> Option<&str> {
        self.git_diff_path.as_deref()
    }

    pub(crate) fn git_diff(&self) -> &str {
        &self.git_diff
    }

    fn on_composer_event(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let text = self.composer.read(cx).value().to_string();
                self.apply_action(DesktopAction::ComposerChanged(text), cx);
                self.refresh_mention_search(cx);
            }
            InputEvent::PressEnter { shift: false, .. } => {
                self.on_send(window, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    pub(crate) fn apply_action(&mut self, action: DesktopAction, cx: &mut Context<Self>) {
        let grew = matches!(
            action,
            DesktopAction::MessageSent { .. }
                | DesktopAction::TurnArmed
                | DesktopAction::ChatDelta(_)
                | DesktopAction::ChatThinkingDelta(_)
                | DesktopAction::ToolStarted { .. }
                | DesktopAction::ToolProgress { .. }
                | DesktopAction::ToolResultAppended(_)
                | DesktopAction::ChatDone { .. }
                | DesktopAction::UsageRecorded { .. }
                | DesktopAction::ConversationOpened(_)
                | DesktopAction::SessionCreated(_)
        );
        let drop_hold = matches!(
            action,
            DesktopAction::ConversationOpened(_)
                | DesktopAction::SessionCreated(_)
                | DesktopAction::SessionDeleted
                | DesktopAction::ConversationParked
        );
        let previous_error = self.vm.error.clone();
        let previous_project = self.vm.project_dir.clone();
        reduce(&mut self.vm, action);
        if self.vm.project_dir != previous_project {
            self.agent_roles_stale = true;
            self.rewatch_git(cx);
        }
        if self.vm.error.is_some()
            && self.vm.error != previous_error
            && let Some(message) = self.vm.error.take()
        {
            self.push_toast(message, ToastKind::Error, cx);
        }
        if drop_hold {
            self.scroll_hold = None;
        }
        // Transcript-growing actions keep the conversation scrolled to the
        // newest content, the way chat clients behave while streaming.
        // Opening a session lands on the tail. Prepending older history is
        // not in this set: that path compensates the scroll offset instead.
        if self.vm.view == MainView::Chat && grew {
            self.conversation_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// The sidebar title filter, created with the workspace.
    pub(crate) fn session_filter_input(&self) -> Option<Entity<InputState>> {
        self.session_filter_input.clone()
    }

    /// Previous offset and content height for one prepend, consumed by the
    /// chat column on the frame that paints the new rows.
    pub(crate) fn take_scroll_anchor(&mut self) -> Option<(Pixels, Pixels)> {
        self.scroll_hold
            .take()
            .map(|hold| (hold.offset_y, hold.content_height))
    }

    /// The conversation column's scroll handle.
    pub(crate) fn conversation_scroll_handle(&self) -> &gpui_kit::ScrollHandle {
        &self.conversation_scroll
    }

    fn dispatch(&self, command: BridgeCommand, cx: &mut Context<Self>) {
        let request = self.bridge.request(command);
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |workspace, cx| workspace.apply_reply(reply, cx));
        })
        .detach();
    }

    // ---- thin nav / toggle dispatchers ----

    pub(crate) fn on_show_settings_section(
        &mut self,
        section: crate::view_model::SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowSettingsSection(section), cx);
        if section == crate::view_model::SettingsSection::Skills {
            self.refresh_skills(cx);
        }
        if section == crate::view_model::SettingsSection::Agents {
            self.agent_roles_stale = true;
        }
    }

    /// Resolves delegation roles when settings is showing them.
    ///
    /// Disk is read only when the cache is empty, the open project changed,
    /// or the Agents section was opened again.
    pub(crate) fn ensure_agent_roles(&mut self) {
        let project = self.vm.project_dir.clone();
        if !self.agent_roles_stale
            && self.agent_roles.is_some()
            && self.agent_roles_project == project
        {
            return;
        }
        let root = project.as_deref().map(std::path::Path::new);
        self.agent_roles = Some(mycode_config::discover_roles(&self.home, root));
        self.agent_roles_project = project;
        self.agent_roles_stale = false;
    }

    /// Role catalog last resolved for settings. Built-ins only before the
    /// first settings paint.
    pub(crate) fn agent_roles(&self) -> mycode_config::RoleCatalog {
        self.agent_roles
            .clone()
            .unwrap_or_else(mycode_config::builtin_roles)
    }

    /// Starts the startup veil on the first frame, unless motion is reduced.
    ///
    /// A preference that flips on while the veil is up drops it on the next
    /// frame, so the fade does not keep covering the desk.
    pub(crate) fn begin_splash(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = self.splash.on_frame(cx.reduce_motion()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.splash.dismiss() {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Drops the startup veil immediately. Click and Escape use this.
    pub(crate) fn dismiss_splash(&mut self, cx: &mut Context<Self>) {
        if self.splash.dismiss() {
            cx.notify();
        }
    }

    /// Whether the startup veil is still covering the desk.
    pub(crate) fn splash_visible(&self) -> bool {
        self.splash.visible()
    }

    /// Switches the Models settings sub-page.
    pub(crate) fn on_show_models_subview(
        &mut self,
        view: crate::view_model::ModelsSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowModelsSubview(view), cx);
    }

    /// Switches the Web search settings sub-page.
    pub(crate) fn on_show_web_subview(
        &mut self,
        view: crate::view_model::WebSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowWebSubview(view), cx);
    }

    /// Switches the MCP settings sub-page.
    pub(crate) fn on_show_mcp_subview(
        &mut self,
        view: crate::view_model::McpSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowMcpSubview(view), cx);
    }

    pub(crate) fn on_toggle_provider_kind_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderKindMenuToggled(open), cx);
    }

    /// Picks the custom provider form's wire protocol.
    pub(crate) fn on_select_provider_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.provider_form.clone() {
            form.update(cx, |form, _| form.kind = kind.to_owned());
        }
        self.apply_action(DesktopAction::ProviderKindMenuToggled(false), cx);
    }

    pub(crate) fn on_toggle_mcp_transport_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::McpTransportMenuToggled(open), cx);
    }

    /// Picks the custom MCP form's transport.
    pub(crate) fn on_select_mcp_transport(&mut self, transport: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.mcp_form.clone() {
            form.update(cx, |form, _| form.transport = transport.to_owned());
        }
        self.apply_action(DesktopAction::McpTransportMenuToggled(false), cx);
    }

    pub(crate) fn on_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let project = crate::view_model::project_of_session(&self.vm.session_projects, session_id)
            .map(str::to_owned);
        self.focused_project = project.clone();
        if project.is_none() {
            self.apply_action(DesktopAction::ActiveProjectChanged(None), cx);
        }
        self.follow_session_project(session_id, cx);
        self.request_open_session(session_id, cx);
    }

    pub(crate) fn on_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if draft != self.vm.composer_draft {
            self.apply_action(DesktopAction::ComposerChanged(draft.clone()), cx);
        }
        if self.vm.sending {
            if !draft.trim().is_empty() {
                self.enqueue_follow_up(draft, window, cx);
            }
            return;
        }
        if !draft.trim().is_empty() {
            self.send(draft, window, cx);
            return;
        }
        self.pump_queued_send(cx);
    }

    pub(crate) fn on_remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuedMessageRemoved(index), cx);
    }

    /// Stops the in-flight turn and sends one queued follow-up immediately.
    pub(crate) fn on_open_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let next = if call_id.is_empty() || self.vm.subagent_window.as_deref() == Some(call_id) {
            None
        } else {
            Some(call_id.to_owned())
        };
        self.apply_action(DesktopAction::SubagentWindowChanged(next), cx);
    }

    /// Opens or closes the full working-tree changes drawer.
    pub(crate) fn on_toggle_changes_panel(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ChangesPanelToggled(open), cx);
    }

    pub(crate) fn on_interrupt_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.apply_action(DesktopAction::QueuedMessagePromoted(index), cx);
        if self.vm.sending {
            self.on_cancel_chat(cx);
            return;
        }
        self.pump_queued_send(cx);
    }

    /// Aborts the in-flight turn; the bridge answers with a `cancelled`
    /// failure event that resets the sending state.
    /// Stops one running subagent. The parent turn and the other children stay.
    pub(crate) fn on_cancel_subagent(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let Some(session_id) = self
            .vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
        else {
            return;
        };
        if call_id.is_empty() {
            return;
        }
        self.apply_action(DesktopAction::SubagentDismissed(call_id.to_owned()), cx);
        self.dispatch(
            BridgeCommand::CancelSubagent {
                session_id,
                call_id: call_id.to_owned(),
            },
            cx,
        );
    }

    pub(crate) fn on_cancel_chat(&mut self, cx: &mut Context<Self>) {
        let Some(conversation) = self.vm.active.as_ref() else {
            return;
        };
        if !self.vm.sending {
            return;
        }
        self.dispatch(
            BridgeCommand::CancelChat {
                session_id: conversation.session_id.clone(),
            },
            cx,
        );
    }

    /// Switches the main area between chat and settings.
    pub(crate) fn on_show_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShowMainView(view), cx);
        if view == MainView::Settings {
            self.refresh_skills(cx);
        }
    }

    pub(crate) fn on_toggle_model_menu(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ModelMenuToggled(open), cx);
        if open && let Some(input) = self.model_picker_input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    /// Moves the open picker into one provider, or back to the provider step.
    ///
    /// Entering a provider asks the next render to focus the search box.
    /// Does not record a model selection.
    pub(crate) fn on_browse_model_provider(
        &mut self,
        provider_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.picker_focus_pending = provider_id.is_some();
        self.apply_action(
            DesktopAction::ModelMenuBrowse(provider_id.map(str::to_owned)),
            cx,
        );
    }

    /// Whether the model step should focus its search box this frame.
    pub(crate) fn take_model_step_focus(&mut self) -> bool {
        if self.vm.model_menu_browse.is_none() {
            return false;
        }
        let pending = self.picker_focus_pending;
        self.picker_focus_pending = false;
        pending
    }

    /// Enter in the model search box selects the preferred row.
    pub(crate) fn accept_model_search(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.vm.model_menu_browse.clone() else {
            return;
        };
        let choices = crate::ui::model_picker::provider_model_choices(&self.vm, &provider);
        let filtered =
            crate::ui::model_picker::filter_model_choices(&choices, &self.vm.picker_query);
        if filtered.is_empty() {
            return;
        }
        let role = self
            .vm
            .subagent_menu
            .clone()
            .filter(|(_, field)| field == "model");
        let selected = if let Some((role, _)) = &role {
            self.vm
                .settings
                .as_ref()
                .and_then(|settings| settings.subagents.role(role))
                .filter(|entry| entry.provider.as_deref() == Some(provider.as_str()))
                .and_then(|entry| entry.model.clone())
        } else if self.vm.selected_provider.as_deref() == Some(provider.as_str()) {
            self.vm.selected_model.clone()
        } else {
            None
        };
        let ids: Vec<String> = filtered.iter().map(|choice| choice.id.clone()).collect();
        let index = crate::ui::model_picker::preferred_index(
            &ids,
            &self.vm.picker_query,
            selected.as_deref(),
        );
        let Some(model) = ids.get(index).cloned() else {
            return;
        };
        if let Some((role, _)) = role {
            self.on_set_subagent_route(&role, Some(provider), Some(model), cx);
            self.apply_action(DesktopAction::SubagentMenuToggled(None), cx);
        } else if self.vm.model_menu_open {
            self.on_select_model_on(&provider, &model, cx);
        }
    }

    /// Stars or unstars one model and persists the pin list.
    pub(crate) fn on_toggle_model_star(
        &mut self,
        provider: &str,
        model: &str,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(
            DesktopAction::ModelStarToggled {
                provider: provider.to_owned(),
                model: model.to_owned(),
            },
            cx,
        );
        self.persist_ui_state(cx);
    }

    pub(crate) fn on_open_provider_detail(&mut self, id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderDetailOpened(Some(id.to_owned())), cx);
    }

    pub(crate) fn on_close_provider_detail(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderDetailOpened(None), cx);
    }

    pub(crate) fn on_toggle_reasoning_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ReasoningMenuToggled(open), cx);
    }

    /// Applies and persists a palette. Light and dark stay independent.
    pub(crate) fn on_select_palette(&mut self, palette: &str, cx: &mut Context<Self>) {
        let palette = crate::ui::desk::normalize_palette(palette);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.palette == palette)
        {
            return;
        }
        crate::ui::desk::apply_palette(Theme::global_mut(cx), palette);
        Theme::sync_base(cx);
        self.apply_action(
            DesktopAction::SettingsPaletteSelected(palette.to_owned()),
            cx,
        );
        self.on_save_settings(cx);
    }

    /// Applies and persists the UI language. The whole window re-renders on
    /// the next frame, so no theme-style refresh is needed.
    pub(crate) fn on_select_language(&mut self, language: &str, cx: &mut Context<Self>) {
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.language == language)
        {
            return;
        }
        self.apply_action(
            DesktopAction::SettingsLanguageSelected(language.to_owned()),
            cx,
        );
        self.on_save_settings(cx);
    }

    pub(crate) fn on_toggle_shell_kind_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ShellKindMenuToggled(open), cx);
    }

    pub(crate) fn on_toggle_language_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::LanguageMenuToggled(open), cx);
    }

    pub(crate) fn on_select_provider(&mut self, provider_id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ProviderSelected(provider_id.to_owned()), cx);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.dirty)
        {
            self.on_save_settings(cx);
        }
        self.persist_ui_state(cx);
    }

    /// Picks one model of the selected provider. The reducer appends an
    /// unknown model to the provider row (so the next turn can use it) or
    /// refuses the pick at the per-provider cap.
    pub(crate) fn on_select_model_on(
        &mut self,
        provider_id: &str,
        model_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.vm.selected_provider.as_deref() != Some(provider_id) {
            self.on_select_provider(provider_id, cx);
        }
        self.on_select_model(model_id, cx);
    }

    pub(crate) fn on_select_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ModelSelected(model_id.to_owned()), cx);
        if self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.dirty)
        {
            self.on_save_settings(cx);
        }
        self.persist_ui_state(cx);
    }

    /// Persists the requested reasoning effort through the settings doc.
    pub(crate) fn on_select_reasoning(&mut self, level: &str, cx: &mut Context<Self>) {
        self.apply_action(
            DesktopAction::SettingsReasoningChanged(level.to_owned()),
            cx,
        );
        self.on_save_settings(cx);
    }

    /// Loads the next older page when the viewport is within a short distance
    /// of the top. A transcript that still fits does not chain-load.
    pub(crate) fn on_conversation_scrolled(&mut self, cx: &mut Context<Self>) {
        if self.vm.history_loading || self.scroll_hold.is_some() || !self.near_history_top() {
            return;
        }
        self.request_older(cx);
    }

    /// Chip click. Works even when the tail still fits on screen.
    pub(crate) fn on_load_older(&mut self, cx: &mut Context<Self>) {
        self.request_older(cx);
    }

    fn near_history_top(&self) -> bool {
        let max = self.conversation_scroll.max_offset().y;
        if max >= px(-1.) {
            return false;
        }
        self.conversation_scroll.offset().y > px(-64.)
    }

    fn conversation_pinned(&self) -> bool {
        let offset = self.conversation_scroll.offset().y;
        let max = self.conversation_scroll.max_offset().y;
        offset - max < px(24.)
    }

    fn capture_scroll_hold(&mut self) {
        let offset = self.conversation_scroll.offset();
        let height = self
            .conversation_scroll
            .bounds_for_item(0)
            .map(|bounds| bounds.size.height)
            .unwrap_or(px(0.));
        self.scroll_hold = Some(ScrollHold {
            offset_y: offset.y,
            content_height: height,
        });
    }

    fn request_older(&mut self, cx: &mut Context<Self>) {
        // `scroll_hold` is the frame where a previous page is still being
        // anchored. Starting another read here would measure the wrong height.
        if self.vm.history_loading || self.scroll_hold.is_some() {
            return;
        }
        let Some(active) = self.vm.active.clone() else {
            return;
        };
        let Some(before) = active.older_before.clone() else {
            return;
        };
        let Some(session) = mycode_app::SessionId::parse(&active.session_id) else {
            return;
        };
        let Some(branch) = mycode_app::BranchId::parse(&active.branch_id) else {
            return;
        };
        let expected_head = parse_head(&active.head);
        self.apply_action(DesktopAction::HistoryLoadStarted, cx);
        self.dispatch(
            BridgeCommand::LoadOlder {
                session,
                branch,
                expected_head,
                before,
            },
            cx,
        );
    }

    // ---- accessors for the render layer ----

    pub(crate) fn vm(&self) -> &WorkspaceState {
        &self.vm
    }

    pub(crate) fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Shows a notice for three seconds. A full stack drops the oldest.
    pub(crate) fn push_toast(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        self.toasts.push(Toast { id, text, kind });
        const MAX_TOASTS: usize = 4;
        if self.toasts.len() > MAX_TOASTS {
            let drop_count = self.toasts.len() - MAX_TOASTS;
            self.toasts.drain(0..drop_count);
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.toasts.retain(|toast| toast.id != id);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn take_manual_update_check(&mut self) -> bool {
        let manual = self.manual_update_check;
        self.manual_update_check = false;
        manual
    }

    pub(crate) fn focus_handle(&self) -> &gpui_kit::FocusHandle {
        &self.focus_handle
    }

    pub(crate) fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    /// Takes the composer prefill restored by edit-and-resend.
    pub(crate) fn take_composer_prefill(&mut self) -> Option<String> {
        self.pending_composer_prefill.take()
    }
}

fn parse_head(spelling: &str) -> HeadStamp {
    if spelling == "empty" {
        HeadStamp::Empty
    } else {
        SessionEventId::parse(spelling)
            .map(HeadStamp::Event)
            .unwrap_or(HeadStamp::Empty)
    }
}

/// Parses one optional token-count field: empty keeps `None`, a plain number
/// overrides; anything else is an error.
fn parse_token_field(raw: &str) -> Result<Option<u64>, ()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u64>().map(Some).map_err(|_| ())
}

/// Parses the stdio form's environment line: `KEY=VALUE` pairs separated by
/// commas or whitespace, values optionally quoted. Empty input is no env.
pub(crate) fn parse_env_line(
    line: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut env = std::collections::BTreeMap::new();
    for word in mycode_config::split_command_line(&line.replace(',', " ")) {
        let Some((key, value)) = word.split_once('=') else {
            return Err(format!("env entry '{word}' must look like KEY=VALUE"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err("env entry has an empty variable name".to_owned());
        }
        env.insert(key.to_owned(), value.to_owned());
    }
    if env.len() > mycode_config::MAX_MCP_ENV_VARS {
        return Err(format!(
            "at most {} env entries per server",
            mycode_config::MAX_MCP_ENV_VARS
        ));
    }
    Ok(env)
}

/// Renders the whole window; split across the `ui` submodule.
impl Workspace {
    pub(crate) fn render_root(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        crate::ui::render_root(self, window, cx)
    }
}

impl gpui_kit::Render for Workspace {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        self.render_root(window, cx)
    }
}
