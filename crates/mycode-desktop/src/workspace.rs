//! The workspace window view: title bar, sessions sidebar, chat column,
//! right inspector, and the full-page settings view.
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use gpui_kit::component::Root;
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, AppContext as _, Context, Entity, Pixels, Window, WindowBounds};
use mycode_frontend::BridgeEvent;
use mycode_frontend::HomeLayout;
use mycode_frontend::driver::{Effect, FrontendDriver};
use mycode_frontend::stream;

use crate::ui::McpForm;
use crate::view_model::{FrontendAction, MainView, WorkspaceState};

mod appearance;
mod attachments;
mod clipboard;
mod composer_send;
mod conversation;
mod history;
mod menus;
mod model_menu;
mod palette;
mod projects;
mod settings_editor;
mod toasts;
mod updates_data;

pub(crate) use toasts::{Toast, ToastKind};

/// Seconds a confirmed close stays armed before the next request asks again.
const CLOSE_CONFIRM_WINDOW: Duration = Duration::from_secs(6);

/// Whether a close request quits now: always when idle, and while a turn
/// runs only when the armed confirm is still inside its window. Kept as a
/// pure helper so the rule is testable without a window.
fn close_allowed(busy: bool, armed: Option<Instant>, now: Instant) -> bool {
    !busy || armed.is_some_and(|armed| now.duration_since(armed) < CLOSE_CONFIRM_WINDOW)
}

mod window;

use window::{client_window_options, startup_geometry, strip_dwm_border};
/// Opens the main window over one owned home. `applied_update` is the
/// version a self-update just installed; it is announced with a toast.
///
/// # Panics
///
/// Panics when the window cannot open; the process has no useful headless
/// fallback by design.
pub fn open_window(home: HomeLayout, applied_update: Option<String>, cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        "secondary-k",
        crate::ui::ToggleCommandPalette,
        None,
    )]);
    // The driver owns the core bridge, the view model, and every product
    // decision. Its wake callback hands a thread-safe sender to the gpui
    // pump, which drains replies, timers, and events on the main thread.
    let (wake_tx, wake_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut driver = FrontendDriver::start(home.clone(), move || {
        let _ = wake_tx.send(());
    });
    // The changes panel and the diff panel show it.
    driver.watch_git();
    if let Some(version) = applied_update {
        driver.note_update_applied(version);
    }
    // Open as a regular window at the persisted (or default) bounds: the
    // maximize-on-open workaround for gpui's stale-scale sizing is retired
    // by user request (DPI edge cases accepted); the user can maximize
    // manually.
    let saved_size = mycode_frontend::read_ui_state(&home)
        .ok()
        .and_then(|state| state.window_size);
    let (bounds, min_size) = startup_geometry(cx, saved_size);
    let options = client_window_options(bounds, min_size);
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
        crate::ui::syntax::sync_base(cx);
        let workspace = Workspace::new(driver, wake_rx, window, cx);
        window.on_window_should_close(cx, {
            let workspace = workspace.clone();
            move |window, cx| {
                workspace.update(cx, |workspace, cx| workspace.on_close_request(window, cx))
            }
        });
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
    /// Whether the Magpie settings page's model list is expanded. Collapsed
    /// by default; the summary row keeps the count and the current name.
    pub(crate) magpie_models_open: bool,
    /// Pictures and code files waiting to ride the next prompt.
    pub(crate) attachments: mycode_frontend::Attachments,
    /// Root focus so key events (Escape) reach the workspace node even when
    /// no input holds focus.
    focus_handle: gpui_kit::FocusHandle,
    composer: Entity<TextareaState>,
    /// Whether the composer placeholder is the in-turn queue hint.
    pub(crate) composer_busy: bool,
    proxy_input: Option<Entity<InputState>>,
    /// The proxy field shows the stored URL on its next render.
    proxy_sync_pending: bool,
    mcp_form: Option<Entity<McpForm>>,

    mcp_json_input: Option<Entity<TextareaState>>,
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
    /// Tool rows the transcript is showing in full. Empty means one-line summaries.
    expanded_tools: HashSet<String>,
    /// Consecutive tool runs the transcript has expanded. Keyed by the run's
    /// first call event id.
    expanded_tool_batches: HashSet<String>,
    /// Compaction summary cards the transcript is showing in full.
    expanded_summaries: HashSet<String>,
    /// A window-close request armed the confirm at this instant. A second
    /// request inside [`CLOSE_CONFIRM_WINDOW`] quits; a later one asks again.
    close_armed: Option<Instant>,
    /// Row-delete key clicked once and waiting for the confirming click.
    /// Session, provider, MCP-server, and folder-root rows all share it.
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
            magpie_models_open: false,
            attachments: mycode_frontend::Attachments::default(),
            focus_handle,
            composer,
            composer_busy: false,
            proxy_input: None,
            proxy_sync_pending: false,
            mcp_form: None,

            mcp_json_input: None,
            model_picker_input: None,
            settings_search_input: None,
            session_filter_input: None,
            command_palette: None,
            palette_focus_pending: false,
            settings_focus_pending: false,
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
            expanded_tools: HashSet::new(),
            expanded_tool_batches: HashSet::new(),
            expanded_summaries: HashSet::new(),
            close_armed: None,
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
                    if buffer.front().is_some_and(stream::is_stream_delta)
                        && let Some(wait) = not_before.checked_duration_since(Instant::now())
                        && !wait.is_zero()
                    {
                        cx.background_executor().timer(wait).await;
                        continue;
                    }
                    let (slice, rest) = stream::split_stream_frame(std::mem::take(&mut buffer));
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
                    if streamed {
                        not_before = Instant::now() + stream::STREAM_FRAME;
                    }
                    if !buffer.is_empty() {
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
                Effect::AttachmentsRestored(attachments) => {
                    self.on_attachments_restored(attachments, cx);
                }
                Effect::AskTextCleared { index } => {
                    self.ask_text_cleared.insert(index);
                }
                Effect::AskReset => {
                    self.ask_inputs.clear();
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
        self.driver.git()
    }

    pub(crate) fn git_diff_path(&self) -> Option<&str> {
        self.driver.git_diff_path()
    }

    pub(crate) fn git_diff(&self) -> &str {
        self.driver.git_diff()
    }

    /// The selected file's diff is open in its own panel, separate from the
    /// changes list.
    pub(crate) fn git_diff_panel_open(&self) -> bool {
        self.driver.git_diff_open()
    }

    /// Opens one dirty path in the dedicated diff panel.
    ///
    /// The changes list (inspector preview or the review-all drawer) only
    /// names files. The diff itself is a separate panel so the patch is
    /// readable. Clicking a file always opens that panel, including when
    /// the full changes drawer is already open.
    pub(crate) fn on_select_git_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.driver.select_git_file(path);
        cx.notify();
    }

    /// Closes the dedicated file-diff panel. The changes list stays as it was.
    pub(crate) fn on_close_git_diff_panel(&mut self, cx: &mut Context<Self>) {
        self.driver.close_git_diff();
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
                // Ctrl+Enter on a busy turn stops it and sends right after;
                // a plain Enter steers it into the running turn (`submit`).
                let sending = self.vm().sending || self.vm().compacting;
                if *secondary && sending {
                    if !self.attachments.is_empty() {
                        // A send-now is text only; the chips wait for the
                        // next normal send instead of riding this one.
                        self.push_toast(
                            crate::i18n::t(
                                "Attachments wait for the next send.",
                                "附件等下一次发送再带上。",
                            ),
                            ToastKind::Info,
                            cx,
                        );
                    }
                    self.driver_do(|d, fx| d.interrupt_send(&draft, fx), cx);
                } else {
                    // Enter takes the same path as the send arrow so the
                    // waiting attachments ride this message.
                    self.on_send(window, cx);
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
        self.driver_take(run, cx);
    }

    /// [`Self::driver_do`] for a driver call that hands a value back, such as
    /// a submit returning attachments the send did not take.
    pub(crate) fn driver_take<T>(
        &mut self,
        run: impl FnOnce(&mut FrontendDriver, &mut Vec<Effect>) -> T,
        cx: &mut Context<Self>,
    ) -> T {
        // Decide from the scroll position before this update. New text has
        // not been laid out yet, so the handle still describes the frame the
        // user is looking at.
        self.driver
            .set_conversation_at_tail(self.conversation_follows_tail());
        self.driver.note_scroll_hold(self.scroll_hold.is_some());
        let subagent_before = self.subagent_window_mark();
        let menus_before = self.menu_navigation_mark();
        let mut effects = Vec::new();
        let value = run(&mut self.driver, &mut effects);
        self.apply_effects(effects, cx);
        self.reset_menu_navigation_after(menus_before);
        self.follow_subagent_window(subagent_before);
        self.ensure_job_tick(cx);
        cx.notify();
        value
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
    pub(crate) fn agent_roles(&self) -> std::sync::Arc<mycode_frontend::RoleCatalog> {
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

    /// Switches the MCP settings sub-page.
    pub(crate) fn on_show_mcp_subview(
        &mut self,
        view: crate::view_model::McpSubview,
        cx: &mut Context<Self>,
    ) {
        self.apply_action(FrontendAction::ShowMcpSubview(view), cx);
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

    /// Whether a compaction summary card shows its text.
    pub(crate) fn summary_open(&self, id: &str) -> bool {
        self.expanded_summaries.contains(id)
    }

    /// Toggles a compaction summary card between its header and its text.
    pub(crate) fn on_toggle_summary(&mut self, id: &str, cx: &mut Context<Self>) {
        let pinned = self.conversation_follows_tail();
        if !self.expanded_summaries.remove(id) {
            self.expanded_summaries.insert(id.to_owned());
        }
        self.keep_tail_pinned(pinned, cx);
    }

    /// Whether a thinking trace shows its full text. One global switch —
    /// recorded and live traces alike — persisted in `ui.json`.
    pub(crate) fn thinking_open(&self) -> bool {
        !self.vm().thinking_collapsed
    }

    /// Flips the global thinking-trace switch. Every trace follows it, so
    /// a collapsed trace never reopens alone; the switch itself decides.
    pub(crate) fn on_toggle_thinking(&mut self, cx: &mut Context<Self>) {
        let pinned = self.conversation_follows_tail();
        self.driver_do(|driver, fx| driver.toggle_thinking_collapsed(fx), cx);
        self.keep_tail_pinned(pinned, cx);
    }

    /// The settings page's thinking-display switch. `collapsed` is the new
    /// global state, persisted with the durable UI state.
    pub(crate) fn on_set_thinking_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.driver_do(
            |driver, fx| driver.set_thinking_collapsed(collapsed, fx),
            cx,
        );
    }

    /// A window-close request (the caption ×, Alt+F4, the taskbar button).
    /// Idle, it quits at once. While a turn or compaction runs, the first
    /// one arms a timed confirm and stays open, and the second inside
    /// [`CLOSE_CONFIRM_WINDOW`] quits. Programmatic quits never see this.
    pub(crate) fn on_close_request(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let busy = self.vm().sending || self.vm().compacting;
        if close_allowed(busy, self.close_armed, Instant::now()) {
            self.close_armed = None;
            return true;
        }
        self.close_armed = Some(Instant::now());
        // Warning outlives the armed window: an Info toast expires a
        // second early, leaving a quit armed with no hint on screen.
        self.push_toast(
            crate::i18n::t(
                "A task is running. Click × again to quit",
                "任务运行中，再点一次 × 退出",
            ),
            ToastKind::Warning,
            cx,
        );
        false
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
mod tests;
