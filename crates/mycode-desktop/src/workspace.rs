//! The workspace window view: title bar, sessions sidebar, chat column,
//! right inspector, and the full-page settings view.
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use gpui_kit::component::Root;
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Bounds, Context, Entity, Pixels, Window, WindowBounds};
use gpui_kit::{px, size};
use mycode_app::{HeadStamp, SessionEventId};
use mycode_config::HomeLayout;

use crate::ui::{McpForm, ProviderForm};
use crate::view_model::{DesktopAction, MainView, WorkspaceState, reduce};
use mycode_app::{BridgeCommand, BridgeEventRx, CoreBridge};

mod appearance;
mod bridge;
mod composer_send;
mod conversation;
mod git;
mod history;
mod menus;
mod model_menu;
mod palette;
mod projects;
mod settings_editor;
mod stream_frame;
mod toasts;
mod updates_data;

pub(crate) use settings_editor::normalized_key_input;
pub(crate) use toasts::{Toast, ToastKind};

/// Preferred first-window size. The origin is computed from the primary
/// display so the window opens in the middle of the usable area, and smaller
/// screens shrink the request rather than clipping it.
const WINDOW_SIZE: gpui_kit::Size<Pixels> = size(px(1760.), px(1040.));

/// Centers the preferred size on the primary display's usable area, shrinking
/// it when the screen is smaller than that size. A persisted windowed size
/// wins over the default.
fn startup_bounds(cx: &App, saved: Option<mycode_config::WindowSize>) -> Bounds<Pixels> {
    let desired = saved
        .map(|saved| size(px(saved.width as f32), px(saved.height as f32)))
        .unwrap_or(WINDOW_SIZE);
    let Some(display) = cx.primary_display() else {
        return Bounds {
            origin: gpui_kit::point(px(80.), px(48.)),
            size: desired,
        };
    };
    let screen = display.visible_bounds();
    let width = fit_edge(screen.size.width, desired.width, px(960.));
    let height = fit_edge(screen.size.height, desired.height, px(560.));
    Bounds::centered_at(screen.center(), size(width, height))
}

fn fit_edge(available: Pixels, desired: Pixels, floor: Pixels) -> Pixels {
    let room = (available - px(64.)).max(floor.min(available));
    desired.min(room)
}

/// Position the native macOS controls inside the custom 40px title bar.
const TRAFFIC_LIGHTS: gpui_kit::Point<Pixels> = gpui_kit::point(px(16.), px(14.));

/// Window options for the app-drawn chrome.
///
/// The system title bar stays hidden on every desktop:
/// - Windows: `appears_transparent` extends the client area over the caption.
///   Min, max, and close still go through `WindowControlArea`.
/// - macOS: `appears_transparent` and `app_owns_titlebar_drag` drop the system
///   title bar and its drag/double-click. Native traffic lights stay visible.
fn client_window_options(bounds: Bounds<Pixels>) -> gpui_kit::WindowOptions {
    let mut options = gpui_kit::component::TitleBar::window_options();
    options.window_bounds = Some(WindowBounds::Windowed(bounds));
    options.window_min_size = Some(size(px(960.), px(560.)));
    options.app_owns_titlebar_drag = true;
    let titlebar = options
        .titlebar
        .get_or_insert_with(gpui_kit::component::TitleBar::title_bar_options);
    titlebar.title = Some("MYCode".into());
    titlebar.appears_transparent = true;
    titlebar.traffic_light_position = Some(TRAFFIC_LIGHTS);
    options
}

/// Removes the 1px DWM border Windows 11 draws around borderless windows.
///
/// `appears_transparent` already drops the caption; without this call DWM
/// still strokes a system-colored edge. `DWMWA_COLOR_NONE` suppresses it.
/// Older Windows versions lack the attribute and simply return an error.
#[cfg(windows)]
fn strip_dwm_border(window: &gpui_kit::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xFF_FF_FF_FE;
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    unsafe {
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as _,
            &DWMWA_COLOR_NONE as *const u32 as *const core::ffi::c_void,
            size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn strip_dwm_border(_: &gpui_kit::Window) {}

/// Opens the main window over one owned home.
///
/// # Panics
///
/// Panics when the window cannot open; the process has no useful headless
/// fallback by design.
pub fn open_window(home: HomeLayout, cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "secondary-k",
        crate::ui::ToggleCommandPalette,
        None,
    )]);
    let (bridge, events) = CoreBridge::start(home.clone());
    // Open as a regular window at the persisted (or default) bounds: the
    // maximize-on-open workaround for gpui's stale-scale sizing is retired
    // by user request (DPI edge cases accepted); the user can maximize
    // manually.
    let saved_size = mycode_config::read_ui_state(&home)
        .ok()
        .and_then(|state| state.window_size);
    let options = client_window_options(startup_bounds(cx, saved_size));
    cx.open_window(options, |window, cx| {
        // Platform blur behind translucent chrome. GPUI has no per-element
        // backdrop filter; macOS and Windows implement this appearance, and a
        // system without blur simply composites the translucent fills.
        window.set_background_appearance(gpui_kit::WindowBackgroundAppearance::Blurred);
        strip_dwm_border(window);
        // Start in dark mode until the saved appearance arrives, then sync
        // the Desk colors into Base.
        Theme::change(ThemeMode::Dark, Some(window), cx);
        crate::ui::desk::apply(Theme::global_mut(cx));
        Theme::sync_base(cx);
        let workspace = Workspace::new(home, bridge, events, window, cx);
        cx.new(|cx| Root::new(workspace, window, cx))
    })
    .expect("open the MYCode window");
}

/// Scroll position captured before older entries are prepended.
struct ScrollHold {
    offset_y: Pixels,
    content_height: Pixels,
}

/// Scroll position captured before a theme change. Applied the next time the
/// conversation column is painted, including when that paint is a later view.
struct AppearanceScroll {
    pinned: bool,
    offset_y: Pixels,
}

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
    proxy_input: Option<Entity<InputState>>,
    /// The proxy field shows the stored URL on its next render.
    proxy_sync_pending: bool,
    provider_form: Option<Entity<ProviderForm>>,
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
    /// Search field and keyboard state for the global command palette.
    command_palette: Option<Entity<CommandState>>,
    /// The next frame should focus the palette search field.
    palette_focus_pending: bool,
    /// The next frame should focus the workspace node. Set when settings
    /// opens, because the previously focused composer is no longer mounted.
    settings_focus_pending: bool,
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
    /// The next model-step render should clear and focus the search box.
    picker_focus_pending: bool,
    ask_inputs: Vec<Entity<InputState>>,
    /// Keyboard selection and scroll handles for the composer pickers.
    menu_navigation: menus::MenuNavigation,
    /// Invalidates asynchronous recall lookups when their conversation changes.
    recall_generation: u64,
    pending_project: Option<String>,
    /// Session created to carry [`crate::view_model::WorkspaceState::pending_welcome_send`].
    welcome_send_session: Option<String>,
    /// Rename editor for the active workspace; lives only while the
    /// workspace menu is in rename mode.
    pub(crate) workspace_rename_input: Option<Entity<InputState>>,
    /// Session whose conversation-open is in flight. Conversation replies
    /// for anything else are dropped, so a parked desk stays parked.
    pending_open: Option<String>,
    /// Draft restored by edit-and-resend, applied on the next render (needs a window).
    pending_composer_prefill: Option<String>,
    /// Settings edit epoch captured when the in-flight save was dispatched.
    settings_save_epoch: u64,
    /// Bumped when a text-field save is scheduled or flushed, so a stale
    /// debounce timer does not write an older draft.
    settings_text_save_generation: u64,
    /// A User-Agent (or similar) debounce is waiting to write `settings.json`.
    settings_text_save_pending: bool,
    /// Bumped on each window-size change so only the settled size persists.
    window_size_generation: u64,
    /// Last mention fragment already searched, to dedupe repeat lookups.
    mention_query: Option<(crate::view_model::MentionKind, String)>,
    /// Bumped on each composer edit so a stale mention timer does not search.
    mention_generation: u64,
    pending_catalog_refresh: bool,
    /// `GetCatalog` was already sent this session. The catalog is loaded
    /// lazily: nothing asks for it until a surface that needs the document
    /// (a model or reasoning picker, the command palette, the Models or
    /// About settings page) opens.
    catalog_requested: bool,
    /// MCP server ids whose enable switch waits on a successful tools
    /// probe; the switch only lands `on` when the probe returns tools.
    pending_mcp_enable: HashSet<String>,
    /// Web backend ids whose enable switch waits on a successful search
    /// probe; the switch only lands `on` when the probe returns `Ok`.
    pending_web_enable: HashSet<String>,
    /// The in-flight check was started from About, so its result may toast.
    manual_update_check: bool,
    /// Config files reset during this launch. Their names are toasted once.
    repaired_configs: Vec<String>,
    /// A single toast for [`Self::repaired_configs`] is already scheduled.
    repair_toast_armed: bool,
    toasts: Vec<Toast>,
    next_toast_id: u64,
    /// In-app folder browser. `None` while the picker is closed.
    pub(crate) project_picker: Option<crate::ui::project_picker::ProjectPicker>,
    /// Only the explicit chat-folder chip may rebind an existing conversation.
    picker_chat_session: Option<String>,
    /// Keeps the conversation column glued to the newest entry while a turn
    /// streams; without it new content grows below the fold.
    conversation_scroll: gpui_kit::ScrollHandle,
    /// The subagent window's body. It follows the newest step the same way
    /// the conversation follows its tail.
    subagent_scroll: gpui_kit::ScrollHandle,
    /// Captured when older entries are prepended. The chat column applies it
    /// in the same frame, after layout and before the scroller positions its
    /// children. gpui's offset grows more negative toward the bottom, so the
    /// correction subtracts the content-height growth.
    scroll_hold: Option<ScrollHold>,
    /// Restored on the next conversation paint after a theme change.
    appearance_scroll: Option<AppearanceScroll>,
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
    /// Path whose diff is shown in the dedicated diff panel.
    git_diff_path: Option<String>,
    git_diff: String,
    /// Bumped when the diff target changes so a stale diff is dropped.
    git_diff_generation: u64,
    /// The selected file's diff is open in its own panel, separate from the
    /// changes list. A file click always opens it.
    git_diff_panel_open: bool,
    /// Tool rows the transcript is showing in full. Empty means one-line summaries.
    expanded_tools: HashSet<String>,
    /// Consecutive tool runs the transcript has expanded. Keyed by the run's
    /// first call event id.
    expanded_tool_batches: HashSet<String>,
    /// Last interface font size applied to the window. Empty until the first frame.
    applied_font_size: String,
    /// A user stop is unwinding: steers the turn hands back join a held
    /// queue instead of starting a new turn. Cleared when a turn starts.
    queue_hold_on_return: bool,
    /// Language last written into input placeholders. `u8::MAX` until the
    /// first frame, so a settings load that flips the language refreshes them.
    applied_language: u8,
    /// Bundle of the import in flight, kept so a refusal can be confirmed.
    import_request: Option<std::path::PathBuf>,
    /// Bundle whose import changes MCP server commands and waits for the
    /// user's explicit confirmation in the Data settings card.
    import_confirm: Option<std::path::PathBuf>,
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
                .placeholder(crate::ui::composer_placeholder(false))
                .soft_wrap(true)
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
            proxy_input: None,
            proxy_sync_pending: false,
            provider_form: None,
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
            command_palette: None,
            palette_focus_pending: false,
            settings_focus_pending: false,
            preset_model_search_input: None,
            provider_key_inputs: HashMap::new(),
            provider_key_replace: HashSet::new(),
            provider_endpoint_inputs: HashMap::new(),
            provider_endpoint_seed: HashMap::new(),
            agent_roles: None,
            agent_roles_project: None,
            agent_roles_stale: false,
            picker_focus_pending: false,
            ask_inputs: Vec::new(),
            menu_navigation: menus::MenuNavigation::default(),
            recall_generation: 0,
            pending_project: None,
            welcome_send_session: None,
            workspace_rename_input: None,
            pending_open: None,
            pending_composer_prefill: None,
            settings_save_epoch: 0,
            settings_text_save_generation: 0,
            settings_text_save_pending: false,
            window_size_generation: 0,
            mention_query: None,
            mention_generation: 0,
            pending_catalog_refresh: false,
            catalog_requested: false,
            pending_mcp_enable: HashSet::new(),
            pending_web_enable: HashSet::new(),
            manual_update_check: false,
            repaired_configs: Vec::new(),
            repair_toast_armed: false,
            toasts: Vec::new(),
            next_toast_id: 0,
            project_picker: None,
            picker_chat_session: None,
            conversation_scroll: gpui_kit::ScrollHandle::new(),
            subagent_scroll: gpui_kit::ScrollHandle::new(),
            scroll_hold: None,
            appearance_scroll: None,
            git: crate::git_status::GitSnapshot::empty(crate::i18n::t("No folder", "未打开目录")),
            git_watcher: None,
            git_generation: 0,
            git_status_inflight: false,
            git_status_pending: false,
            git_diff_path: None,
            git_diff: String::new(),
            git_diff_generation: 0,
            git_diff_panel_open: false,
            expanded_tools: HashSet::new(),
            expanded_tool_batches: HashSet::new(),
            applied_font_size: String::new(),
            applied_language: u8::MAX,
            queue_hold_on_return: false,
            import_request: None,
            import_confirm: None,
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
            // Deferred startup work (catalog refresh, update check) starts
            // once the first frame painted; the catalog document itself
            // loads lazily in `ensure_catalog`.
            cx.on_next_frame(window, |workspace, _window, cx| {
                workspace.dispatch(BridgeCommand::UiReady, cx);
            });
            cx.observe_window_bounds(window, |workspace, window, cx| {
                workspace.on_window_resized(window, cx);
            })
            .detach();
        });
        workspace
    }

    /// Parks until the core sends, then paints streaming deltas a frame at a time.
    ///
    /// A whole turn can already be queued when this task wakes. Applying it
    /// before the next await paints only the finished message. Each slice
    /// stops before `ChatDone` and before the character budget, then waits
    /// one frame so the bubble can grow.
    fn spawn_event_pump(&self, mut events: BridgeEventRx, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut buffer: VecDeque<mycode_app::BridgeEvent> = VecDeque::new();
            let mut not_before = Instant::now();
            loop {
                if buffer.is_empty() {
                    match events.recv().await {
                        Some(event) => buffer.push_back(event),
                        None => return,
                    }
                }
                while let Ok(event) = events.try_recv() {
                    buffer.push_back(event);
                }
                // A queued follow-up must not wait out the typewriter. The
                // frame budget holds `ChatDone` until every delta has been
                // painted (~80 characters / 64ms), so a long reply kept the
                // queue parked for the whole animation. When something is
                // waiting, apply the burst now and let the next send start.
                let rush = this
                    .update(cx, |workspace, _| !workspace.vm.queued.is_empty())
                    .unwrap_or(false);
                if !rush
                    && buffer.front().is_some_and(stream_frame::is_stream_delta)
                    && let Some(wait) = not_before.checked_duration_since(Instant::now())
                    && !wait.is_zero()
                {
                    cx.background_executor().timer(wait).await;
                    while let Ok(event) = events.try_recv() {
                        buffer.push_back(event);
                    }
                }
                let (slice, rest) = if rush {
                    let pending = std::mem::take(&mut buffer);
                    (pending.into_iter().collect(), VecDeque::new())
                } else {
                    stream_frame::split_stream_frame(std::mem::take(&mut buffer))
                };
                buffer = rest;
                let mut streamed = false;
                for event in slice {
                    streamed |= stream_frame::is_stream_delta(&event);
                    if this
                        .update(cx, |workspace, cx| workspace.apply_event(event, cx))
                        .is_err()
                    {
                        return;
                    }
                }
                if streamed && !rush {
                    not_before = Instant::now() + stream_frame::STREAM_FRAME;
                }
                if !buffer.is_empty() && !rush {
                    let now = Instant::now();
                    let wait = not_before
                        .checked_duration_since(now)
                        .filter(|wait| !wait.is_zero())
                        .unwrap_or(stream_frame::STREAM_FRAME);
                    cx.background_executor().timer(wait).await;
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

    pub(crate) fn git_diff_panel_open(&self) -> bool {
        self.git_diff_panel_open
    }

    /// Closes the dedicated file-diff panel. The changes list stays as it was.
    pub(crate) fn on_close_git_diff_panel(&mut self, cx: &mut Context<Self>) {
        self.git_diff_panel_open = false;
        cx.notify();
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
                let draft = self.composer.read(cx).value().to_string();
                if draft != self.vm.composer_draft {
                    self.apply_action(DesktopAction::ComposerChanged(draft), cx);
                }
                if self.accept_open_mention(window, cx) {
                    return;
                }
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
        self.reset_menu_navigation(&action);
        if drop_hold {
            self.recall_generation = self.recall_generation.wrapping_add(1);
        }
        if drop_hold
            || matches!(
                action,
                DesktopAction::AskRequested(_)
                    | DesktopAction::ChatDone { .. }
                    | DesktopAction::ChatFailed { .. }
            )
        {
            self.ask_inputs.clear();
        }
        let previous_error = self.vm.error.clone();
        let previous_project = self.vm.project_dir.clone();
        // Decide from the scroll position before this update. New text has
        // not been laid out yet, so the handle still describes the frame the
        // user is looking at.
        let follow_tail = grew && self.conversation_follows_tail();
        let subagent_before = self.subagent_window_mark();
        let text_edit = matches!(
            action,
            DesktopAction::SettingsProxyChanged { typed: true, .. }
        );
        let epoch_before = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.edit_epoch);
        if matches!(action, DesktopAction::ShowMainView(view) if view != MainView::Settings)
            && self.vm.view == MainView::Settings
        {
            self.flush_settings_text_save(cx);
        }
        if let DesktopAction::SettingsMcpRemoved(index) = &action
            && let Some(id) = self
                .vm
                .settings
                .as_ref()
                .and_then(|settings| settings.mcp_servers.get(*index))
                .map(|server| server.id.clone())
        {
            self.pending_mcp_enable.remove(&id);
        }
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
        // Stick to the newest line while the user is already there. A reader
        // who scrolled up into history keeps that position. Prepending older
        // history is not in this set: that path compensates the scroll offset.
        if self.vm.view == MainView::Chat && follow_tail {
            self.conversation_scroll.scroll_to_bottom();
        }
        self.follow_subagent_window(subagent_before);
        let epoch_after = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.edit_epoch);
        let edited = match (epoch_before, epoch_after) {
            // A reload replaces the projection with epoch 0. That is not a
            // local edit. The first real edit moves the epoch off 0.
            (Some(before), Some(after)) => after != before && after != 0,
            _ => false,
        };
        if edited {
            if text_edit {
                self.schedule_settings_text_save(cx);
            } else {
                self.on_save_settings(cx);
            }
        }
        cx.notify();
    }

    /// The sidebar title filter, created with the workspace.
    pub(crate) fn session_filter_input(&self) -> Option<Entity<InputState>> {
        self.session_filter_input.clone()
    }

    fn dispatch(&self, command: BridgeCommand, cx: &mut Context<Self>) {
        let scope = bridge::ReplyScope::for_command(&command);
        let request = self.bridge.request(command);
        cx.spawn(async move |this, cx| {
            let reply = request.await;
            let _ = this.update(cx, |workspace, cx| {
                if scope.applies(&workspace.vm, workspace.pending_open.as_deref()) {
                    workspace.apply_reply(reply, cx);
                }
            });
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

    /// Switches the Models settings sub-page.
    pub(crate) fn on_show_models_subview(
        &mut self,
        view: crate::view_model::ModelsSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(DesktopAction::ShowModelsSubview(view), cx);
    }

    /// Opens one vendor's Web detail page (key, enable switch, endpoint).
    pub(crate) fn on_open_web_detail(&mut self, id: &str, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::WebDetailOpened(Some(id.to_owned())), cx);
    }

    /// Returns from a Web vendor detail to the backend list.
    pub(crate) fn on_close_web_detail(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::WebDetailOpened(None), cx);
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
        if crate::view_model::project_of_session(&self.vm.session_projects, session_id).is_none() {
            self.apply_action(DesktopAction::ActiveProjectChanged(None), cx);
        }
        self.follow_session_project(session_id, cx);
        self.request_open_session(session_id, cx);
    }

    /// Switches the main area between chat and settings.
    pub(crate) fn on_show_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        let entering_settings = view == MainView::Settings && self.vm.view != MainView::Settings;
        self.apply_action(DesktopAction::ShowMainView(view), cx);
        if entering_settings {
            self.settings_focus_pending = true;
            self.refresh_skills(cx);
        }
    }

    /// Whether this frame should focus the workspace node so Settings Escape
    /// reaches the root key listener.
    pub(crate) fn take_settings_focus(&mut self) -> bool {
        let pending = self.settings_focus_pending && self.vm.view == MainView::Settings;
        if self.settings_focus_pending {
            self.settings_focus_pending = false;
        }
        pending
    }

    /// Opens, closes, or pins the inspector. Does not touch the session.
    /// The pair is persisted so a resident panel survives a restart.
    pub(crate) fn on_set_inspector(&mut self, open: bool, pinned: bool, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::InspectorChanged { open, pinned }, cx);
        self.persist_ui_state(cx);
    }

    /// Whether a tool row is showing its result body.
    pub(crate) fn tool_row_open(&self, id: &str) -> bool {
        self.expanded_tools.contains(id)
    }

    /// Toggles one tool row between a one-line summary and its result body.
    pub(crate) fn on_toggle_tool_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tools.remove(id) {
            self.expanded_tools.insert(id.to_owned());
        }
        cx.notify();
    }

    /// Whether a collapsed run of tool calls is open.
    pub(crate) fn tool_batch_open(&self, id: &str) -> bool {
        self.expanded_tool_batches.contains(id)
    }

    /// Toggles a one-line tool-run summary.
    pub(crate) fn on_toggle_tool_batch(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tool_batches.remove(id) {
            self.expanded_tool_batches.insert(id.to_owned());
        }
        cx.notify();
    }

    /// Whether thinking traces show their full text. One switch covers
    /// every trace, live and recorded.
    pub(crate) fn thinking_open(&self) -> bool {
        !self.vm.thinking_collapsed
    }

    /// Expands or collapses every thinking trace and remembers the choice.
    pub(crate) fn on_toggle_thinking(&mut self, cx: &mut Context<Self>) {
        self.apply_action(DesktopAction::ThinkingCollapsedToggled, cx);
        self.persist_ui_state(cx);
    }

    /// Session, branch, and head of the open conversation, read without
    /// cloning its transcript. `None` without an open chat or when its ids
    /// do not parse.
    fn open_turn_target(&self) -> Option<(mycode_app::SessionId, mycode_app::BranchId, HeadStamp)> {
        let active = self.vm.active.as_ref()?;
        Some((
            mycode_app::SessionId::parse(&active.session_id)?,
            mycode_app::BranchId::parse(&active.branch_id)?,
            parse_head(&active.head),
        ))
    }

    /// The open conversation's session id.
    fn open_session_id(&self) -> Option<String> {
        self.vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
    }

    // ---- accessors for the render layer ----

    pub(crate) fn vm(&self) -> &WorkspaceState {
        &self.vm
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
pub(crate) fn parse_token_field(raw: &str) -> Result<Option<u64>, ()> {
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
    for word in mycode_config::split_command_line(&space_separate_env(line)) {
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

/// Maps commas to spaces outside quotes so [`mycode_config::split_command_line`]
/// sees one word per `KEY=VALUE` while a quoted value keeps its commas.
fn space_separate_env(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(open) if ch == open => {
                quote = None;
                out.push(ch);
            }
            // Mirror split_command_line: a backslash escapes `"` and `\`
            // inside double quotes, so `\",` is not a comma outside quotes.
            Some('"') if ch == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            Some(_) => out.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                out.push(ch);
            }
            None if ch == ',' => out.push(' '),
            None => out.push(ch),
        }
    }
    out
}

/// Renders the whole window; split across the `ui` submodule.
impl Workspace {
    /// Whether an open surface reads the provider catalog document.
    fn catalog_needed(&self) -> bool {
        let vm = &self.vm;
        vm.model_menu_open
            || vm.reasoning_menu_open
            || vm.subagent_menu.is_some()
            || vm.command_palette_open
            || (vm.view == crate::view_model::MainView::Settings
                && matches!(
                    vm.settings_section,
                    crate::view_model::SettingsSection::Models
                        | crate::view_model::SettingsSection::About
                ))
    }

    /// Persists a resized window's settled size. The observer fires on every
    /// bounds change (moves included), so only a size difference arms the
    /// debounce and only the last generation wins the write.
    fn on_window_resized(&mut self, window: &Window, cx: &mut Context<Self>) {
        let bounds = match window.window_bounds() {
            // `Maximized` still reports the restorable windowed bounds, which
            // is exactly the size to restore on the next launch.
            WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => bounds,
            WindowBounds::Fullscreen(_) => return,
        };
        let size = mycode_config::WindowSize {
            width: f32::from(bounds.size.width).round().max(0.) as u32,
            height: f32::from(bounds.size.height).round().max(0.) as u32,
        };
        if size.width == 0
            || size.height == 0
            || size.width > mycode_config::MAX_WINDOW_EDGE_PX
            || size.height > mycode_config::MAX_WINDOW_EDGE_PX
            || self.vm.window_size == Some(size)
        {
            return;
        }
        self.vm.window_size = Some(size);
        self.window_size_generation = self.window_size_generation.wrapping_add(1);
        let generation = self.window_size_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.window_size_generation == generation {
                    workspace.persist_ui_state(cx);
                }
            });
        })
        .detach();
    }

    /// Loads the provider catalog the first time a surface needs it. The
    /// document stays unparsed in the core until this request lands, keeping
    /// the multi-megabyte decode off the startup path.
    fn ensure_catalog(&mut self, cx: &mut Context<Self>) {
        if self.catalog_requested || self.vm.catalog.is_some() || !self.catalog_needed() {
            return;
        }
        self.catalog_requested = true;
        self.dispatch(BridgeCommand::GetCatalog, cx);
    }

    pub(crate) fn render_root(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        self.ensure_catalog(cx);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_chrome_hides_the_system_title_bar() {
        let options = client_window_options(Bounds {
            origin: gpui_kit::point(px(0.), px(0.)),
            size: WINDOW_SIZE,
        });
        assert!(options.app_owns_titlebar_drag);
        let titlebar = options.titlebar.expect("custom title bar");
        assert!(titlebar.appears_transparent);
        assert_eq!(titlebar.title.as_ref().map(AsRef::as_ref), Some("MYCode"));
        assert_eq!(titlebar.traffic_light_position, Some(TRAFFIC_LIGHTS));
        assert!(f32::from(TRAFFIC_LIGHTS.x) >= 0.);
        assert!(f32::from(TRAFFIC_LIGHTS.y) >= 0.);
    }

    #[test]
    fn env_line_keeps_commas_inside_quoted_values() {
        let env = parse_env_line(r#"A=1, B="x,y", C='p,q' D=2"#).unwrap();
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("x,y"));
        assert_eq!(env.get("C").map(String::as_str), Some("p,q"));
        assert_eq!(env.get("D").map(String::as_str), Some("2"));
    }

    #[test]
    fn env_line_rejects_a_bare_word() {
        assert!(parse_env_line("A=1 stray").is_err());
        assert!(parse_env_line("=value").is_err());
    }
}
