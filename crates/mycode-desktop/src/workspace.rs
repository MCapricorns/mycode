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
use mycode_app::BridgeEvent;
use mycode_config::HomeLayout;
use mycode_frontend::driver::{Effect, FrontendDriver};
use mycode_frontend::stream;

use crate::ui::{McpForm, ProviderForm};
use crate::view_model::{FrontendAction, MainView, WorkspaceState};

mod appearance;
mod composer_send;
mod conversation;
mod git;
mod history;
mod menus;
mod model_menu;
mod palette;
mod projects;
mod settings_editor;
mod toasts;
mod updates_data;

pub(crate) use mycode_frontend::forms::normalized_key_input;
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
    // The driver owns the core bridge, the view model, and every product
    // decision. Its wake callback hands a thread-safe sender to the gpui
    // pump, which drains replies, timers, and events on the main thread.
    let (wake_tx, wake_rx) = tokio::sync::mpsc::unbounded_channel();
    let driver = FrontendDriver::start(home.clone(), move || {
        let _ = wake_tx.send(());
    });
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
        let workspace = Workspace::new(driver, wake_rx, window, cx);
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
    /// Shared product driver: the view model, the bridge, replies, timers,
    /// and every UI-agnostic decision. GPUI keeps only window state and
    /// the effects it alone can perform.
    driver: FrontendDriver,
    /// Root focus so key events (Escape) reach the workspace node even when
    /// no input holds focus.
    focus_handle: gpui_kit::FocusHandle,
    composer: Entity<TextareaState>,
    /// Whether the composer placeholder is the in-turn queue hint.
    pub(crate) composer_busy: bool,
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
    /// The next model-step render should clear and focus the search box.
    picker_focus_pending: bool,
    ask_inputs: Vec<Entity<InputState>>,
    /// Keyboard selection and scroll handles for the composer pickers.
    menu_navigation: menus::MenuNavigation,
    /// Composer text + focus request handed down by `Effect::ComposerSet`;
    /// the composer paint consumes it.
    pending_composer_set: Option<(String, bool)>,
    /// `Effect::RestoreFocus`: the next paint focuses the composer (chat)
    /// or the workspace root.
    focus_restore_pending: bool,
    /// The once-a-second repaint that keeps live subagent runtimes counting
    /// between progress events. `true` while that timer loop exists.
    job_tick_running: bool,
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
    /// Recorded thinking traces shown opposite the remembered default,
    /// keyed by event id.
    flipped_thinking: HashSet<String>,
    /// Row delete (`session:<id>`, `provider:<id>`) clicked once and
    /// waiting for the confirming click.
    armed_delete: Option<String>,
    /// Last interface font size applied to the window. Empty until the first frame.
    applied_font_size: String,
    /// Language last written into input placeholders. `u8::MAX` until the
    /// first frame, so a settings load that flips the language refreshes them.
    applied_language: u8,
    /// Ask rows whose free-text field should be cleared on the next paint —
    /// `Effect::AskTextCleared` fires without a window, so the input's own
    /// render performs the reset.
    ask_text_cleared: HashSet<usize>,
}

impl Workspace {
    /// Builds the workspace and issues the initial core loads.
    pub fn new(
        driver: FrontendDriver,
        wake_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(crate::ui::composer_placeholder(false))
                .soft_wrap(true)
                // Three fixed rows: pasting a wall of text scrolls inside
                // the field instead of growing the composer card.
                .auto_grow(3, 3)
                .submit_on_enter(true)
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let workspace = cx.new(|_| Self {
            driver,
            focus_handle,
            composer,
            composer_busy: false,
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
            picker_focus_pending: false,
            ask_inputs: Vec::new(),
            menu_navigation: menus::MenuNavigation::default(),
            pending_composer_set: None,
            focus_restore_pending: false,
            job_tick_running: false,
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
            flipped_thinking: HashSet::new(),
            armed_delete: None,
            applied_font_size: String::new(),
            applied_language: u8::MAX,
            ask_text_cleared: HashSet::new(),
        });
        workspace.update(cx, |workspace, cx| {
            let filter = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::t("Search sessions", "搜索会话"))
            });
            cx.subscribe_in(&filter, window, |workspace, entity, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = entity.read(cx).value().to_string();
                    workspace.apply_action(FrontendAction::SessionFilterChanged(text), cx);
                }
            })
            .detach();
            workspace.session_filter_input = Some(filter);
            let composer = workspace.composer.clone();
            cx.subscribe_in(&composer, window, |workspace, _, event, window, cx| {
                workspace.on_composer_event(event, window, cx);
            })
            .detach();
            workspace.spawn_driver_pump(wake_rx, cx);
            {
                let mut effects = Vec::new();
                workspace.driver.bootstrap(&mut effects);
                workspace.apply_effects(effects, cx);
            }
            // Deferred startup work (catalog refresh, update check) starts
            // once the first frame painted; the catalog document itself
            // loads lazily in `ensure_catalog`.
            cx.on_next_frame(window, |workspace, _window, _cx| {
                workspace.driver.ui_ready();
            });
            cx.observe_window_bounds(window, |workspace, window, cx| {
                workspace.on_window_resized(window, cx);
            })
            .detach();
        });
        workspace
    }

    /// Parks until the driver wakes, then paints streaming deltas a frame at
    /// a time.
    ///
    /// A whole turn can already be queued when this task wakes. Applying it
    /// before the next await paints only the finished message. Each slice
    /// stops before `ChatDone` and before the character budget, then waits
    /// one frame so the bubble can grow.
    fn spawn_driver_pump(
        &self,
        mut wake_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let mut buffer: VecDeque<BridgeEvent> = VecDeque::new();
            let mut not_before = Instant::now();
            'outer: while wake_rx.recv().await.is_some() {
                loop {
                    if this
                        .update(cx, |workspace, cx| workspace.drain_driver(&mut buffer, cx))
                        .is_err()
                    {
                        return;
                    }
                    if buffer.is_empty() {
                        continue 'outer;
                    }
                    let rush = this
                        .update(cx, |workspace, _| !workspace.vm().queued.is_empty())
                        .unwrap_or(false);
                    if !rush
                        && buffer.front().is_some_and(stream::is_stream_delta)
                        && let Some(wait) = not_before.checked_duration_since(Instant::now())
                        && !wait.is_zero()
                    {
                        cx.background_executor().timer(wait).await;
                        continue;
                    }
                    let (slice, rest) = if rush {
                        let pending = std::mem::take(&mut buffer);
                        (pending.into_iter().collect(), VecDeque::new())
                    } else {
                        stream::split_stream_frame(std::mem::take(&mut buffer))
                    };
                    buffer = rest;
                    let mut streamed = false;
                    for event in slice {
                        streamed |= stream::is_stream_delta(&event);
                        if this
                            .update(cx, |workspace, cx| workspace.on_driver_event(event, cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                    if streamed && !rush {
                        not_before = Instant::now() + stream::STREAM_FRAME;
                    }
                    if !buffer.is_empty() && !rush {
                        let now = Instant::now();
                        let wait = not_before
                            .checked_duration_since(now)
                            .filter(|wait| !wait.is_zero())
                            .unwrap_or(stream::STREAM_FRAME);
                        cx.background_executor().timer(wait).await;
                    }
                }
            }
        })
        .detach();
    }

    /// Replies, timers, and freshly arrived core events move to the main
    /// thread here; each driver side effect is then performed by the window.
    fn drain_driver(&mut self, buffer: &mut VecDeque<BridgeEvent>, cx: &mut Context<Self>) {
        self.driver_do(
            |driver, effects| {
                driver.poll(effects);
                while let Some(event) = driver.next_event() {
                    buffer.push_back(event);
                }
            },
            cx,
        );
    }

    /// One core event through the driver; its effects land right away.
    fn on_driver_event(&mut self, event: BridgeEvent, cx: &mut Context<Self>) {
        self.driver_do(|driver, effects| driver.on_event(event, effects), cx);
    }

    /// Performs the work the driver delegated to this frontend.
    fn apply_effects(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                Effect::Toast { kind, text } => {
                    let kind = match kind {
                        mycode_frontend::driver::ToastKind::Info => ToastKind::Info,
                        mycode_frontend::driver::ToastKind::Warning => ToastKind::Warning,
                        mycode_frontend::driver::ToastKind::Error => ToastKind::Error,
                    };
                    self.push_toast(text, kind, cx);
                }
                Effect::ComposerSet { text, focus } => {
                    self.pending_composer_set = Some((text, focus));
                    cx.notify();
                }
                Effect::AskTextCleared { index } => {
                    self.ask_text_cleared.insert(index);
                }
                Effect::AskReset => {
                    self.ask_inputs.clear();
                }
                Effect::ProjectChanged => {
                    self.rewatch_git(cx);
                }
                Effect::AppearanceTheme(theme) => {
                    self.capture_appearance_scroll();
                    self.paint_appearance(&theme, cx);
                }
                Effect::FontSizeChanged => {
                    self.applied_font_size.clear();
                }
                Effect::LanguageChanged => {
                    self.applied_language = u8::MAX;
                }
                Effect::OlderPagePrepended { .. } => {
                    self.capture_scroll_hold();
                }
                Effect::ScrollToTail => {
                    if self.vm().view == MainView::Chat {
                        self.conversation_scroll.scroll_to_bottom();
                    }
                }
                Effect::ScrollAnchorInvalidated => {
                    self.scroll_hold = None;
                    self.driver.note_scroll_hold(false);
                }
                Effect::SyncProxyField => {
                    self.proxy_sync_pending = true;
                }
                Effect::RestoreFocus => {
                    self.focus_restore_pending = true;
                }
                Effect::Quit => {
                    cx.quit();
                }
            }
        }
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
                self.apply_action(FrontendAction::ComposerChanged(text), cx);
            }
            InputEvent::PressEnter {
                shift: false,
                secondary,
            } => {
                let draft = self.composer.read(cx).value().to_string();
                if self.accept_open_mention(window, cx) {
                    return;
                }
                // Ctrl+Enter on a busy turn interrupts and sends right away;
                // a plain Enter queues the follow-up inside `submit`.
                let sending = self.vm().sending || self.vm().compacting;
                if *secondary && sending {
                    self.driver_do(|d, fx| d.interrupt_send(&draft, fx), cx);
                } else {
                    self.driver_do(|d, fx| d.submit(&draft, fx), cx);
                }
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// Every state transition enters here so the GPUI-only bookkeeping —
    /// menu selection, scroll-follow, subagent follow — runs beside the
    /// shared reducer inside the driver.
    pub(crate) fn apply_action(&mut self, action: FrontendAction, cx: &mut Context<Self>) {
        self.reset_menu_navigation(&action);
        self.driver_do(|d, fx| d.act(action, fx), cx);
    }

    /// Runs one driver interaction and performs the effects it returns.
    ///
    /// Replies and streamed events land here too, not only user actions, so
    /// the scroll and selection bookkeeping is done around every driver
    /// call: the driver's tail-follow decision reads the position the user
    /// is looking at, and an open subagent window keeps following its log.
    pub(crate) fn driver_do(
        &mut self,
        run: impl FnOnce(&mut FrontendDriver, &mut Vec<Effect>),
        cx: &mut Context<Self>,
    ) {
        // Decide from the scroll position before this update. New text has
        // not been laid out yet, so the handle still describes the frame the
        // user is looking at.
        self.driver
            .set_conversation_at_tail(self.conversation_follows_tail());
        self.driver.note_scroll_hold(self.scroll_hold.is_some());
        let subagent_before = self.subagent_window_mark();
        let menus_before = self.menu_navigation_mark();
        let mut effects = Vec::new();
        run(&mut self.driver, &mut effects);
        self.apply_effects(effects, cx);
        self.reset_menu_navigation_after(menus_before);
        self.follow_subagent_window(subagent_before);
        self.ensure_job_tick(cx);
        cx.notify();
    }

    /// While a turn or a subagent runs, repaint once a second so each
    /// elapsed-time label keeps counting between progress events. The loop
    /// retires itself as soon as nothing live is left.
    fn ensure_job_tick(&mut self, cx: &mut Context<Self>) {
        let idle = self.vm().live_jobs.is_empty() && !self.vm().sending;
        if self.job_tick_running || idle {
            return;
        }
        self.job_tick_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                // Clear the flag in the same update that finds nothing
                // live, so a job starting right after still gets a tick.
                let running = this
                    .update(cx, |workspace, cx| {
                        let idle = workspace.vm().live_jobs.is_empty() && !workspace.vm().sending;
                        if idle {
                            workspace.job_tick_running = false;
                            false
                        } else {
                            cx.notify();
                            true
                        }
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
    }

    /// The sidebar title filter, created with the workspace.
    pub(crate) fn session_filter_input(&self) -> Option<Entity<InputState>> {
        self.session_filter_input.clone()
    }

    // ---- thin nav / toggle dispatchers ----

    pub(crate) fn on_show_settings_section(
        &mut self,
        section: crate::view_model::SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(FrontendAction::ShowSettingsSection(section), cx);
        if section == crate::view_model::SettingsSection::Skills {
            self.driver_do(|d, fx| d.refresh_skills(fx), cx);
        }
        if section == crate::view_model::SettingsSection::Agents {
            self.driver.invalidate_agent_roles();
        }
    }

    /// Resolves delegation roles when settings is showing them.
    pub(crate) fn ensure_agent_roles(&mut self) {
        self.driver.ensure_agent_roles();
    }

    /// Role catalog last resolved for settings. Built-ins only before the
    /// first settings paint.
    pub(crate) fn agent_roles(&self) -> mycode_config::RoleCatalog {
        self.driver.agent_roles()
    }

    /// Switches the Models settings sub-page.
    pub(crate) fn on_show_models_subview(
        &mut self,
        view: crate::view_model::ModelsSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(FrontendAction::ShowModelsSubview(view), cx);
    }

    /// Opens one vendor's Web detail page (key, enable switch, endpoint).
    pub(crate) fn on_open_web_detail(&mut self, id: &str, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::WebDetailOpened(Some(id.to_owned())), cx);
    }

    /// Returns from a Web vendor detail to the backend list.
    pub(crate) fn on_close_web_detail(&mut self, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::WebDetailOpened(None), cx);
    }

    /// Switches the MCP settings sub-page.
    pub(crate) fn on_show_mcp_subview(
        &mut self,
        view: crate::view_model::McpSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(FrontendAction::ShowMcpSubview(view), cx);
    }

    pub(crate) fn on_toggle_provider_kind_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::ProviderKindMenuToggled(open), cx);
    }

    /// Picks the custom provider form's wire protocol.
    pub(crate) fn on_select_provider_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.provider_form.clone() {
            form.update(cx, |form, _| form.kind = kind.to_owned());
        }
        self.apply_action(FrontendAction::ProviderKindMenuToggled(false), cx);
    }

    pub(crate) fn on_toggle_mcp_transport_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::McpTransportMenuToggled(open), cx);
    }

    /// Picks the custom MCP form's transport.
    pub(crate) fn on_select_mcp_transport(&mut self, transport: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.mcp_form.clone() {
            form.update(cx, |form, _| form.transport = transport.to_owned());
        }
        self.apply_action(FrontendAction::McpTransportMenuToggled(false), cx);
    }

    pub(crate) fn on_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        self.driver_do(|d, fx| d.open_session(session_id, fx), cx);
    }

    /// Switches the main area between chat and settings.
    pub(crate) fn on_show_main_view(&mut self, view: MainView, cx: &mut Context<Self>) {
        let entering_settings = view == MainView::Settings && self.vm().view != MainView::Settings;
        self.driver_do(|d, fx| d.show_main_view(view, fx), cx);
        if entering_settings {
            self.settings_focus_pending = true;
        }
    }

    /// Whether this frame should focus the workspace node so Settings Escape
    /// reaches the root key listener.
    pub(crate) fn take_settings_focus(&mut self) -> bool {
        let pending = self.settings_focus_pending && self.vm().view == MainView::Settings;
        self.settings_focus_pending = false;
        pending
    }

    /// Focus the driver asked for after a switch the render could not see.
    pub(crate) fn take_focus_restore(&mut self) -> bool {
        std::mem::take(&mut self.focus_restore_pending)
    }

    /// Opens, closes, or pins the inspector. Does not touch the session.
    /// The pair is persisted so a resident panel survives a restart.
    pub(crate) fn on_set_inspector(&mut self, open: bool, pinned: bool, cx: &mut Context<Self>) {
        self.apply_action(FrontendAction::InspectorChanged { open, pinned }, cx);
        self.persist_ui_state();
    }

    /// Whether a tool row is showing its result body.
    pub(crate) fn tool_row_open(&self, id: &str) -> bool {
        self.expanded_tools.contains(id)
    }

    /// Toggles one tool row between a one-line summary and its result body.
    pub(crate) fn on_toggle_tool_row(&mut self, id: &str, cx: &mut Context<Self>) {
        let pinned = self.conversation_follows_tail();
        if !self.expanded_tools.remove(id) {
            self.expanded_tools.insert(id.to_owned());
        }
        self.keep_tail_pinned(pinned, cx);
    }

    /// Expanding a row at the tail grows the content below the viewport,
    /// which reads as "the reader scrolled up" and stops the tail follow.
    /// A reader who was following keeps following.
    fn keep_tail_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) {
        if pinned && self.vm().view == MainView::Chat {
            self.conversation_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// Whether a collapsed run of tool calls is open.
    pub(crate) fn tool_batch_open(&self, id: &str) -> bool {
        self.expanded_tool_batches.contains(id)
    }

    /// Toggles a one-line tool-run summary.
    pub(crate) fn on_toggle_tool_batch(&mut self, id: &str, cx: &mut Context<Self>) {
        let pinned = self.conversation_follows_tail();
        if !self.expanded_tool_batches.remove(id) {
            self.expanded_tool_batches.insert(id.to_owned());
        }
        self.keep_tail_pinned(pinned, cx);
    }

    /// Whether a thinking trace shows its full text. `event_id` is `None`
    /// for the live trace, which always follows the remembered default.
    pub(crate) fn thinking_open(&self, event_id: Option<&str>) -> bool {
        let flipped = event_id.is_some_and(|id| self.flipped_thinking.contains(id));
        self.vm().thinking_collapsed == flipped
    }

    /// Toggles one thinking trace.
    ///
    /// A recorded trace flips alone: one switch for every trace expanded
    /// each one above the reader too, and the line they clicked was pushed
    /// out of view. The live trace changes the remembered default that new
    /// replies open with, and every recorded trace keeps the state it shows.
    pub(crate) fn on_toggle_thinking(&mut self, event_id: Option<&str>, cx: &mut Context<Self>) {
        let pinned = self.conversation_follows_tail();
        match event_id {
            Some(id) => {
                if !self.flipped_thinking.remove(id) {
                    self.flipped_thinking.insert(id.to_owned());
                }
            }
            None => {
                let recorded: Vec<String> = self
                    .vm()
                    .active
                    .iter()
                    .flat_map(|conversation| &conversation.entries)
                    .filter(|entry| !entry.thinking.is_empty())
                    .map(|entry| entry.event_id.clone())
                    .collect();
                for id in recorded {
                    if !self.flipped_thinking.remove(&id) {
                        self.flipped_thinking.insert(id);
                    }
                }
                self.apply_action(FrontendAction::ThinkingCollapsedToggled, cx);
                self.persist_ui_state();
            }
        }
        self.keep_tail_pinned(pinned, cx);
    }

    /// Persists the durable UI state projection (last project, selections,
    /// inspector, window size, …) through the shared layer.
    fn persist_ui_state(&mut self) {
        self.driver.persist_ui_state();
    }

    // ---- accessors for the render layer ----

    pub(crate) fn vm(&self) -> &WorkspaceState {
        self.driver.vm()
    }

    pub(crate) fn focus_handle(&self) -> &gpui_kit::FocusHandle {
        &self.focus_handle
    }

    pub(crate) fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    /// The composer replacement a `ComposerSet` effect requested, with its
    /// focus flag; consumed by the next composer paint.
    pub(crate) fn take_composer_set(&mut self) -> Option<(String, bool)> {
        self.pending_composer_set.take()
    }

    /// The question row the ask card is showing.
    pub(crate) fn ask_index(&self) -> usize {
        self.driver.ask_index()
    }
}

/// Renders the whole window; split across the `ui` submodule.
impl Workspace {
    /// Persists a resized window's settled size. The driver debounces; only
    /// the last generation wins the write.
    fn on_window_resized(&mut self, window: &Window, _cx: &mut Context<Self>) {
        let bounds = match window.window_bounds() {
            // `Maximized` still reports the restorable windowed bounds, which
            // is exactly the size to restore on the next launch.
            WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => bounds,
            WindowBounds::Fullscreen(_) => return,
        };
        self.driver
            .note_window_resized(bounds.size.width.into(), bounds.size.height.into());
    }

    pub(crate) fn render_root(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        self.driver.ensure_catalog();
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
}
