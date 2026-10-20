//! The headless product driver: one `CoreBridge`, one [`WorkspaceState`], and
//! every product decision that used to live inside the desktop window.
//!
//! A frontend owns exactly one driver. It feeds user intent in as method
//! calls or [`FrontendAction`]s, drains bridge traffic with
//! [`FrontendDriver::poll`]/[`FrontendDriver::next_event`], and interprets the
//! returned [`Effect`]s in its own idiom — GPUI turns them into toasts,
//! scroll corrections, and `cx.notify()`; a TUI turns them into redraws.
//!
//! Replies and internal timers arrive on a `std::sync::mpsc` inbox; each
//! deposit calls the `wake` callback supplied at construction so a UI
//! executor can schedule a drain. Events keep their own channel so a
//! frontend can pace streamed deltas (the desktop's typewriter frames)
//! while control messages are handled immediately.
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::JoinHandle;
use std::time::Duration;

use mycode_app::{
    BranchId, BridgeCommand, BridgeEvent, BridgeReply, CoreBridge, HeadStamp, SessionEventId,
    SessionId,
};
use mycode_config::HomeLayout;

use crate::view_model::{FrontendAction, MainView, MentionKind, WorkspaceState, reduce};

mod events;
mod projects;
mod replies;
mod sessions;
mod settings;
mod turn;
mod updates;

#[cfg(test)]
mod tests;

/// Notice severity. Errors stay visible until explicitly dismissed; the
/// other kinds expire on a reading-time delay in the frontend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    /// A cache collapse or another condition the user should notice.
    Warning,
    Error,
}

/// One thing the driver decided that only the concrete frontend can perform:
/// a notice to show, a composer field to set, a directory to re-watch.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Show a transient notice. Errors stay until dismissed.
    Toast {
        /// Severity — expiry and styling are the frontend's.
        kind: ToastKind,
        /// Text already localized.
        text: String,
    },
    /// Replace the composer's text. `focus` asks for composer focus.
    ComposerSet {
        /// The new draft text.
        text: String,
        /// Move focus into the composer when true.
        focus: bool,
    },
    /// One ask-card free-text field was cleared (a choice replaced typing).
    AskTextCleared {
        /// Question row index.
        index: usize,
    },
    /// Every ask-card field should reset (a new card, the card answered or
    /// dismissed, or the conversation left the screen).
    AskReset,
    /// `state.project_dir` changed; re-read git status for the new folder.
    ProjectChanged,
    /// A settings-loaded theme or an explicit pick should paint now.
    AppearanceTheme(String),
    /// The interface scale changed; repaint the window at the new rem.
    FontSizeChanged,
    /// The interface language flipped; live strings already read the new
    /// language, placeholders captured at creation need a rewrite.
    LanguageChanged,
    /// An older transcript page was prepended. `pinned` means the viewport
    /// was at the tail, so the frontend should keep following it instead of
    /// restoring a scroll offset.
    OlderPagePrepended {
        /// Whether the conversation was following its tail.
        pinned: bool,
    },
    /// Conversation content grew while the viewport followed the tail:
    /// scroll to the newest line. A reader who scrolled up gets nothing.
    ScrollToTail,
    /// The conversation was replaced (open, create, delete, park): any
    /// pending prepend scroll anchor measures the wrong document now.
    ScrollAnchorInvalidated,
    /// The proxy text field should re-seed from `vm.settings.proxy`.
    SyncProxyField,
    /// Focus should return to the composer (chat) or the window root.
    RestoreFocus,
    /// Install staged and armed: quit the process so the swap runs.
    Quit,
}

/// Replies without embedded identity still belong to the request's session.
/// A late cancel, recall, or search must never mutate a newly opened chat.
#[derive(Debug)]
pub(crate) enum ReplyScope {
    Global,
    Open(String),
    Session {
        session: String,
        branch: Option<String>,
    },
    Mention {
        session: String,
        query: String,
    },
    /// A row delete: the reply decides whether the session actually went
    /// away before the UI forgets its bindings.
    DeletedSession(String),
    /// A `LoadOlder` page resolving a rewind target, not a history prepend.
    Recall(Box<RecallRequest>),
}

/// The rewind a `LoadOlder` reply is resolving. The page walk chains until
/// it finds the event before the message being recalled.
#[derive(Clone, Debug)]
pub(crate) struct RecallRequest {
    pub(crate) session: SessionId,
    pub(crate) branch: BranchId,
    /// Head at the moment the recall was asked for.
    pub(crate) head: String,
    /// Text to put back in the composer.
    pub(crate) edit: String,
    /// Invalidation token; a newer recall or view change drops this one.
    pub(crate) generation: u64,
}

impl ReplyScope {
    pub(crate) fn for_command(command: &BridgeCommand) -> Self {
        match command {
            BridgeCommand::OpenSession(session) => Self::Open(session.to_string()),
            BridgeCommand::ChatTurn {
                session, branch, ..
            }
            | BridgeCommand::SendMessage {
                session, branch, ..
            }
            | BridgeCommand::RecallMessage {
                session, branch, ..
            }
            | BridgeCommand::LoadOlder {
                session, branch, ..
            } => Self::Session {
                session: session.to_string(),
                branch: Some(branch.to_string()),
            },
            BridgeCommand::CancelChat { session_id }
            | BridgeCommand::CancelSubagent { session_id, .. }
            | BridgeCommand::ListResources { session_id }
            | BridgeCommand::AskAnswer { session_id, .. }
            | BridgeCommand::AskDismiss { session_id } => Self::Session {
                session: session_id.clone(),
                branch: None,
            },
            BridgeCommand::SearchProjectFiles { session_id, query } => Self::Mention {
                session: session_id.clone(),
                query: query.clone(),
            },
            BridgeCommand::DeleteSession { session_id } => Self::DeletedSession(session_id.clone()),
            _ => Self::Global,
        }
    }

    fn applies(&self, vm: &WorkspaceState, pending_open: Option<&str>) -> bool {
        match self {
            Self::Global => true,
            Self::Open(session) => pending_open == Some(session.as_str()),
            Self::Session { session, branch } => vm.active.as_ref().is_some_and(|active| {
                &active.session_id == session
                    && branch
                        .as_ref()
                        .is_none_or(|branch| branch == &active.branch_id)
            }),
            Self::Mention { session, query } => {
                vm.active
                    .as_ref()
                    .is_some_and(|active| &active.session_id == session)
                    && vm.mention.as_ref().is_some_and(|mention| {
                        mention.kind == MentionKind::File && &mention.fragment == query
                    })
            }
            // Delete and recall replies carry their own staleness checks.
            Self::DeletedSession(_) | Self::Recall(_) => true,
        }
    }
}

/// Everything that lands on the driver's work queue besides bridge events.
enum DriverMsg {
    /// A command's reply, tagged with the scope chosen at dispatch.
    /// `BridgeReply` is wide (a whole conversation rides `Conversation`), so
    /// the reply is boxed and the small tick variants stay cheap.
    Reply {
        scope: ReplyScope,
        reply: Box<BridgeReply>,
    },
    /// The mention-search debounce elapsed; the generation must still match.
    MentionSearchTick { generation: u64 },
    /// The settings text-field debounce elapsed; flush the draft document.
    SettingsTextSaveTick { generation: u64 },
    /// The settled window size should persist now.
    UiStateSaveTick { generation: u64 },
    /// Startup config repairs waited out their toast delay.
    RepairToastTick,
    /// A day of runtime passed; re-check for an update when allowed.
    UpdateRecheckTick,
}

/// Typing `@` walks the project tree. Wait until the fragment settles so
/// each keystroke does not start another full walk.
const MENTION_SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);
/// Writes a typed settings text field (the proxy URL) about 400ms after
/// the last keystroke. A newer edit or a blur supersedes the timer.
const SETTINGS_TEXT_SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
/// Persists a resized window's settled size after this quiet period.
const UI_STATE_SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
/// Config-repair names collect for half a second so startup toasts arrive once.
const REPAIR_TOAST_DELAY: Duration = Duration::from_millis(500);
/// Wakes once a day of runtime, then checks for an update when that is enabled.
const UPDATE_RECHECK_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// A thread that can be unparked by a [`Waker`]. Lets reply futures — a
/// tokio oneshot each — resolve on plain threads without a runtime.
struct Parker(std::thread::Thread);

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Resolves one future to completion on the calling thread.
fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(Parker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// The shared frontend controller. Owns the core bridge, the view-model
/// state, and the product bookkeeping; renders nothing itself.
pub struct FrontendDriver {
    bridge: CoreBridge,
    /// Streaming events forwarded onto a `std` channel for paced draining.
    events: Receiver<BridgeEvent>,
    inbox: Receiver<DriverMsg>,
    inbox_tx: Sender<DriverMsg>,
    /// Called after every inbox deposit and forwarded event — a frontend's
    /// redraw hook.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Forwarder thread: tokio receiver → `std` channel → `wake`.
    _events_worker: JoinHandle<()>,
    /// Whole-window state; product transitions go through [`reduce`].
    vm: WorkspaceState,
    /// Owned home the process opened with. Role discovery reads `agents/`
    /// under it; skill discovery scans the open project and the OS user home.
    home: HomeLayout,
    /// Session created to carry [`WorkspaceState::pending_welcome_send`].
    welcome_send_session: Option<String>,
    /// Session whose conversation-open is in flight. Conversation replies
    /// for anything else are dropped, so a parked desk stays parked.
    pending_open: Option<String>,
    /// Project the next created session binds to.
    pending_project: Option<String>,
    /// Draft restored by edit-and-resend or a slash insert, applied by the
    /// frontend's next composer paint (the widget needs a window on GPUI).
    pending_composer_prefill: Option<String>,
    /// The frontend last reported the conversation viewport at its tail.
    /// `Older` replies and `grew` actions read it to decide whether the
    /// viewport follows the stream or keeps the reader's position.
    conversation_at_tail: bool,
    /// The frontend is holding a scroll anchor for a page still painting.
    /// `request_older` refuses to start another read on top of it.
    scroll_hold_pending: bool,
    /// Settings edit epoch captured when the in-flight save was dispatched.
    settings_save_epoch: u64,
    /// Bumped when a text-field save is scheduled or flushed, so a stale
    /// debounce tick does not write an older draft.
    settings_text_save_generation: u64,
    /// A typed settings text-field debounce is waiting to write `settings.json`.
    settings_text_save_pending: bool,
    /// Bumped on each window-size change so only the settled size persists.
    ui_state_save_generation: u64,
    /// Last mention fragment already searched, to dedupe repeat lookups.
    mention_query: Option<(MentionKind, String)>,
    /// Bumped on each file-fragment change so a stale mention tick does not
    /// search.
    mention_generation: u64,
    /// `GetCatalog` was already sent this session. The catalog is loaded
    /// lazily: nothing asks for it until a surface that needs the document
    /// opens.
    catalog_requested: bool,
    pending_catalog_refresh: bool,
    /// MCP server ids whose enable switch waits on a successful tools probe.
    pending_mcp_enable: HashSet<String>,
    /// Web backend ids whose enable switch waits on a successful probe.
    pending_web_enable: HashSet<String>,
    /// The in-flight check was started by the user, so its result may toast.
    manual_update_check: bool,
    /// The staged payload's exit swap is already armed; staging the same
    /// payload again (a manual install click, say) must not spawn another
    /// helper copy.
    update_armed: bool,
    /// Version a self-update installed since the last launch. Toasted once
    /// the first settings reply lands, so it renders in the UI language.
    applied_update: Option<String>,
    /// A user stop is unwinding: steers the turn hands back join a held
    /// queue instead of starting a new turn. Cleared when a turn starts.
    queue_hold_on_return: bool,
    /// Invalidates asynchronous recall lookups when their conversation
    /// changes.
    recall_generation: u64,
    /// Config files reset during this launch; their names toast once.
    repaired_configs: Vec<String>,
    /// How many of [`Self::repaired_configs`] a toast already named.
    repairs_toasted: usize,
    /// A single toast for the untoasted repairs is already scheduled.
    repair_toast_armed: bool,
    /// Bundle of the import in flight, kept so a refusal can be confirmed.
    import_request: Option<PathBuf>,
    /// Bundle whose import changes MCP server commands and waits for the
    /// user's explicit confirmation.
    import_confirm: Option<PathBuf>,
    /// Roles resolved from built-ins, the home `agents` directory, and the
    /// open project's `.mycode/agents`. `None` until a surface asks for them.
    agent_roles: Option<Arc<mycode_config::RoleCatalog>>,
    /// Project directory the cached role catalog was built for.
    agent_roles_project: Option<String>,
    /// The roles surface was opened again, or the project changed.
    agent_roles_stale: bool,
    /// The `LoadUiState` reply landed (either way), so the remembered
    /// project no longer overwrites one the frontend picks.
    ui_state_loaded: bool,
    /// Session opened while its turn was running. Events committed between
    /// the core reading that snapshot and the reply landing here were
    /// dropped as belonging to another chat, so the transcript reloads from
    /// the ledger once the turn ends.
    resync_on_turn_end: Option<String>,
    /// A `SendMessage` waiting for a reopen of the same chat to land. That
    /// reopen's snapshot predates the message; applied after the send's
    /// reply, it would wipe the message off the screen.
    held_send: Option<BridgeCommand>,
    /// Session whose interrupt waits for its automatic compaction to end;
    /// see [`FrontendDriver::interrupt_send`].
    interrupt_after_compaction: Option<String>,
    /// Free-text answers to the pending ask card, aligned with its rows.
    pub ask_texts: Vec<String>,
    /// The question the ask card is showing; answers step one at a time.
    pub ask_index: usize,
}

impl FrontendDriver {
    /// Starts the core and the forwarder thread. `wake` runs after each
    /// inbox deposit and after each forwarded event.
    ///
    /// # Panics
    /// Panics when the core bridge thread cannot start.
    #[must_use]
    pub fn start(home: HomeLayout, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (bridge, mut bridge_events) = CoreBridge::start(home.clone());
        let (events_tx, events) = std::sync::mpsc::channel::<BridgeEvent>();
        let (inbox_tx, inbox) = std::sync::mpsc::channel::<DriverMsg>();
        let wake = Arc::new(wake) as Arc<dyn Fn() + Send + Sync>;
        let events_wake = wake.clone();
        let events_worker = std::thread::Builder::new()
            .name("mycode-driver-events".into())
            .spawn(move || {
                // `blocking_recv` is the sync-context receiver; it parks
                // until the core sends or every sender is dropped.
                while let Some(event) = bridge_events.blocking_recv() {
                    if events_tx.send(event).is_err() {
                        return;
                    }
                    events_wake();
                }
            })
            .expect("driver events thread");
        let driver = Self {
            bridge,
            events,
            inbox,
            inbox_tx,
            wake,
            _events_worker: events_worker,
            vm: WorkspaceState::default(),
            home,
            welcome_send_session: None,
            pending_open: None,
            pending_project: None,
            pending_composer_prefill: None,
            conversation_at_tail: true,
            scroll_hold_pending: false,
            settings_save_epoch: 0,
            settings_text_save_generation: 0,
            settings_text_save_pending: false,
            ui_state_save_generation: 0,
            mention_query: None,
            mention_generation: 0,
            catalog_requested: false,
            pending_catalog_refresh: false,
            pending_mcp_enable: HashSet::new(),
            pending_web_enable: HashSet::new(),
            manual_update_check: false,
            update_armed: false,
            applied_update: None,
            queue_hold_on_return: false,
            recall_generation: 0,
            repaired_configs: Vec::new(),
            repairs_toasted: 0,
            repair_toast_armed: false,
            import_request: None,
            import_confirm: None,
            agent_roles: None,
            agent_roles_project: None,
            agent_roles_stale: false,
            ui_state_loaded: false,
            resync_on_turn_end: None,
            held_send: None,
            interrupt_after_compaction: None,
            ask_texts: Vec::new(),
            ask_index: 0,
        };
        driver.spawn_update_recheck();
        driver
    }

    /// The view-model state the frontend renders.
    pub fn vm(&self) -> &WorkspaceState {
        &self.vm
    }

    /// The owned home this process opened with.
    pub fn home(&self) -> &HomeLayout {
        &self.home
    }

    /// Initial loads every frontend issues at startup.
    pub fn bootstrap(&mut self, effects: &mut Vec<Effect>) {
        self.dispatch(BridgeCommand::ListSessions);
        self.dispatch(BridgeCommand::LoadSettings);
        self.dispatch(BridgeCommand::LoadUiState);
        let _ = effects;
    }

    /// Deferred startup work: catalog refresh and the update check start
    /// once the first frame painted.
    pub fn ui_ready(&mut self) {
        self.dispatch(BridgeCommand::UiReady);
    }

    /// The open conversation's session id.
    pub fn open_session_id(&self) -> Option<String> {
        self.vm
            .active
            .as_ref()
            .map(|conversation| conversation.session_id.clone())
    }

    /// Session, branch, and head of the open conversation, read without
    /// cloning its transcript. `None` without an open chat or when its ids
    /// do not parse.
    pub fn open_turn_target(&self) -> Option<(SessionId, BranchId, HeadStamp)> {
        let active = self.vm.active.as_ref()?;
        Some((
            SessionId::parse(&active.session_id)?,
            BranchId::parse(&active.branch_id)?,
            parse_head(&active.head),
        ))
    }

    /// Draft restored by edit-and-resend, applied by the frontend's next
    /// composer paint.
    pub fn take_composer_prefill(&mut self) -> Option<String> {
        self.pending_composer_prefill.take()
    }

    /// The frontend reports whether the conversation viewport sits at its
    /// tail. Drivers that cannot scroll (a CLI) leave this `true`.
    pub fn set_conversation_at_tail(&mut self, at_tail: bool) {
        self.conversation_at_tail = at_tail;
    }

    /// The frontend holds a scroll anchor for an older page still painting.
    /// Cleared when the column consumes it, and on any conversation switch
    /// via [`Effect::ScrollAnchorInvalidated`].
    pub fn note_scroll_hold(&mut self, pending: bool) {
        self.scroll_hold_pending = pending;
    }

    /// Records the version a self-update installed since the last launch;
    /// it is announced after the first settings load.
    pub fn note_update_applied(&mut self, version: String) {
        self.applied_update = Some(version);
    }

    /// Persists a resized window's settled size. The frontend reports every
    /// bounds change; only a size difference arms the debounce and only the
    /// last generation wins the write.
    pub fn note_window_resized(&mut self, width: f32, height: f32) {
        let size = mycode_config::WindowSize {
            width: width.round().max(0.) as u32,
            height: height.round().max(0.) as u32,
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
        self.ui_state_save_generation = self.ui_state_save_generation.wrapping_add(1);
        let generation = self.ui_state_save_generation;
        self.schedule(UI_STATE_SAVE_DEBOUNCE, move || DriverMsg::UiStateSaveTick {
            generation,
        });
    }

    /// Whether the startup `LoadUiState` reply has been folded in. A
    /// frontend that opens its own folder (the terminal's working
    /// directory) waits for this, or the remembered project replaces it.
    pub fn ui_state_loaded(&self) -> bool {
        self.ui_state_loaded
    }

    /// The bundle waiting for the MCP-command confirmation, if any.
    pub fn import_confirm(&self) -> Option<&Path> {
        self.import_confirm.as_deref()
    }

    /// The question row the ask card is showing.
    pub fn ask_index(&self) -> usize {
        self.ask_index
    }

    /// Escape's first level: close whichever floating menu is open. Returns
    /// true when something closed so the caller stops dismissing.
    pub fn dismiss_floating(&mut self) -> bool {
        crate::view_model::close_floating_menus(&mut self.vm)
    }

    /// Enqueues one command immediately; its reply lands on the inbox
    /// tagged with its scope.
    pub fn dispatch(&self, command: BridgeCommand) {
        let scope = ReplyScope::for_command(&command);
        self.dispatch_scoped(command, scope);
    }

    /// Dispatch with an explicit scope — the recall page walk tags its
    /// `LoadOlder` replies so they resolve the rewind instead of prepending.
    pub(crate) fn dispatch_scoped(&self, command: BridgeCommand, scope: ReplyScope) {
        let request = self.bridge.request(command);
        let tx = self.inbox_tx.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let reply = block_on(request);
            if tx
                .send(DriverMsg::Reply {
                    scope,
                    reply: Box::new(reply),
                })
                .is_ok()
            {
                wake();
            }
        });
    }

    /// One timer step landing later on this same inbox.
    fn schedule(&self, after: Duration, build: impl FnOnce() -> DriverMsg + Send + 'static) {
        let tx = self.inbox_tx.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            if tx.send(build()).is_ok() {
                wake();
            }
        });
    }

    /// The next bridge event, if the forwarder already delivered one.
    /// Frontends call this inside their wake handler to pace streamed
    /// deltas; `poll` covers replies and timers.
    pub fn next_event(&mut self) -> Option<BridgeEvent> {
        self.events.try_recv().ok()
    }

    /// Drains replies and expired timers, folding each into the state.
    /// Effects accumulate in `out` in the order they were decided.
    pub fn poll(&mut self, out: &mut Vec<Effect>) {
        while let Ok(message) = self.inbox.try_recv() {
            match message {
                DriverMsg::Reply { scope, reply } => {
                    if scope.applies(&self.vm, self.pending_open.as_deref()) {
                        self.on_reply(scope, *reply, out);
                    }
                    // Any reply can end the reopen a held send waits on,
                    // including one that superseded it.
                    self.release_held_send();
                }
                DriverMsg::MentionSearchTick { generation } => {
                    if generation == self.mention_generation {
                        self.fire_mention_search();
                    }
                }
                DriverMsg::SettingsTextSaveTick { generation } => {
                    if generation == self.settings_text_save_generation {
                        self.settings_text_save_pending = false;
                        self.save_settings(out);
                    }
                }
                DriverMsg::UiStateSaveTick { generation } => {
                    if generation == self.ui_state_save_generation {
                        self.persist_ui_state();
                    }
                }
                DriverMsg::RepairToastTick => {
                    // Re-arm for a repair a later settings load makes; the
                    // names already shown stay out of the next toast.
                    self.repair_toast_armed = false;
                    let names = self.repaired_configs[self.repairs_toasted..].join(", ");
                    self.repairs_toasted = self.repaired_configs.len();
                    self.toast(
                        format!(
                            "{} {names}. {}",
                            crate::i18n::t("Reset unreadable config:", "已重置无法读取的配置："),
                            crate::i18n::t(
                                "The previous files were kept beside them.",
                                "原文件已留在旁边。",
                            ),
                        ),
                        ToastKind::Info,
                        out,
                    );
                }
                DriverMsg::UpdateRecheckTick => {
                    if self.vm.auto_update {
                        self.check_update(false, out);
                    }
                    // A one-shot timer fired: re-arm so the daily cadence
                    // continues for the process's whole uptime.
                    self.spawn_update_recheck();
                }
            }
        }
    }

    /// Applies one action to the view-model state, then runs the product
    /// consequences that follow from the transition (autosave, toasts,
    /// project-change side effects, tail-follow scroll, mention search).
    pub fn act(&mut self, action: FrontendAction, out: &mut Vec<Effect>) {
        let grew = matches!(
            action,
            FrontendAction::MessageSent { .. }
                | FrontendAction::TurnArmed
                | FrontendAction::ChatDelta(_)
                | FrontendAction::ChatThinkingDelta(_)
                | FrontendAction::ToolPreparing { .. }
                | FrontendAction::ToolStarted { .. }
                | FrontendAction::ToolProgress { .. }
                | FrontendAction::ToolResultAppended(_)
                | FrontendAction::AssistantStepCommitted(_)
                | FrontendAction::SteerCommitted { .. }
                | FrontendAction::SteersReturned(_)
                | FrontendAction::SummaryShown(_)
                | FrontendAction::MessageQueued(_)
                | FrontendAction::AskRequested(_)
                | FrontendAction::CompactArmed
                | FrontendAction::CompactFinished { .. }
                | FrontendAction::ChatDone { .. }
                | FrontendAction::ChatFailed { .. }
                | FrontendAction::ConversationOpened(_)
                | FrontendAction::SessionCreated(_)
        );
        let drop_hold = matches!(
            action,
            FrontendAction::ConversationOpened(_)
                | FrontendAction::SessionCreated(_)
                | FrontendAction::SessionDeleted
                | FrontendAction::ConversationParked
        );
        if drop_hold {
            self.recall_generation = self.recall_generation.wrapping_add(1);
        }
        if drop_hold
            || matches!(
                action,
                FrontendAction::AskRequested(_)
                    | FrontendAction::ChatDone { .. }
                    | FrontendAction::ChatFailed { .. }
            )
        {
            self.ask_texts.clear();
            self.ask_index = 0;
            out.push(Effect::AskReset);
        }
        let previous_error = self.vm.error.clone();
        let previous_project = self.vm.project_dir.clone();
        let composer_changed = matches!(action, FrontendAction::ComposerChanged(_));
        let text_edit = matches!(
            action,
            FrontendAction::SettingsProxyChanged { typed: true, .. }
        );
        let epoch_before = self
            .vm
            .settings
            .as_ref()
            .map(|settings| settings.edit_epoch);
        if matches!(action, FrontendAction::ShowMainView(view) if view != MainView::Settings)
            && self.vm.view == MainView::Settings
        {
            self.flush_settings_text_save(out);
        }
        if let FrontendAction::SettingsMcpRemoved(index) = &action
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
            out.push(Effect::ProjectChanged);
        }
        if self.vm.error.is_some()
            && self.vm.error != previous_error
            && let Some(message) = self.vm.error.take()
        {
            self.toast(message, ToastKind::Error, out);
        }
        if drop_hold {
            self.scroll_hold_pending = false;
            out.push(Effect::ScrollAnchorInvalidated);
        }
        // Stick to the newest line while the user is already there. A reader
        // who scrolled up into history keeps that position. Prepending older
        // history is not in this set: that path compensates the scroll offset.
        if self.vm.view == MainView::Chat && grew && self.conversation_at_tail {
            out.push(Effect::ScrollToTail);
        }
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
                self.schedule_settings_text_save();
            } else {
                self.save_settings(out);
            }
        }
        if composer_changed {
            self.refresh_mention_search();
        }
    }

    /// Convenience for one-off pushes.
    pub(crate) fn toast(&self, text: impl Into<String>, kind: ToastKind, out: &mut Vec<Effect>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        out.push(Effect::Toast { kind, text });
    }

    /// Roles for the Agents settings page: built-ins first, then home and
    /// project `agents` directories. Disk is read only when the cache is
    /// empty, the open project changed, or the page was opened again.
    pub fn ensure_agent_roles(&mut self) {
        let project = self.vm.project_dir.clone();
        if !self.agent_roles_stale
            && self.agent_roles.is_some()
            && self.agent_roles_project == project
        {
            return;
        }
        let root = project.as_deref().map(Path::new);
        self.agent_roles = Some(Arc::new(mycode_config::discover_roles(&self.home, root)));
        self.agent_roles_project = project;
        self.agent_roles_stale = false;
    }

    /// Role catalog last resolved for settings. Built-ins only before the
    /// first resolution. Shared, so a settings paint does not copy every
    /// role prompt.
    pub fn agent_roles(&self) -> Arc<mycode_config::RoleCatalog> {
        self.agent_roles
            .clone()
            .unwrap_or_else(|| Arc::new(mycode_config::builtin_roles()))
    }

    /// Marks the roles cache dirty so the next read re-scans.
    pub fn invalidate_agent_roles(&mut self) {
        self.agent_roles_stale = true;
    }

    /// Loads the provider catalog the first time a surface needs it. The
    /// document stays unparsed in the core until this request lands.
    pub fn ensure_catalog(&mut self) {
        if self.catalog_requested || self.vm.catalog.is_some() || !self.catalog_needed() {
            return;
        }
        self.catalog_requested = true;
        self.dispatch(BridgeCommand::GetCatalog);
    }

    /// A manual catalog reload; the reply is marked by
    /// `pending_catalog_refresh` so it can surface a refreshed toast.
    pub fn refresh_catalog(&mut self) {
        self.pending_catalog_refresh = true;
        self.dispatch(BridgeCommand::RefreshCatalog);
    }

    /// Whether a mounted surface reads the provider catalog document.
    ///
    /// The composer chips and the context meter consult it on every chat
    /// paint — the model label, the thinking rungs, and the window that
    /// sizes the ring all resolve through it — so a selected model counts
    /// as a need with no menu open. With nothing selected there is nothing
    /// to resolve and the fetch stays lazy.
    fn catalog_needed(&self) -> bool {
        let vm = &self.vm;
        vm.model_menu_open
            || vm.reasoning_menu_open
            || vm.subagent_menu.is_some()
            || vm.command_palette_open
            || (vm.view == MainView::Settings
                && matches!(
                    vm.settings_section,
                    crate::view_model::SettingsSection::Models
                        | crate::view_model::SettingsSection::About
                ))
            || (vm.view == MainView::Chat
                && (vm.selected_provider.is_some() || vm.selected_model.is_some()))
    }

    /// Records documents reset at startup and toasts their names once the
    /// settings and UI replies have had a chance to arrive.
    pub fn note_config_repairs(&mut self, repairs: &[mycode_config::DocumentRepair]) {
        let mut added = false;
        for repair in repairs {
            if self
                .repaired_configs
                .iter()
                .any(|existing| existing == repair.path)
            {
                continue;
            }
            self.repaired_configs.push(repair.path.to_owned());
            added = true;
        }
        if added && !self.repair_toast_armed {
            self.repair_toast_armed = true;
            self.schedule(REPAIR_TOAST_DELAY, || DriverMsg::RepairToastTick);
        }
    }

    /// Stops the core thread and waits for it to finish.
    pub fn shutdown(&mut self) {
        self.bridge.shutdown();
    }
}

impl Drop for FrontendDriver {
    fn drop(&mut self) {
        // The command sender must close before join; `shutdown` is explicit
        // so a panicking unwind cannot leave the core running. It is
        // idempotent, so an earlier explicit call makes this a no-op.
        self.shutdown();
    }
}

pub(crate) fn parse_head(spelling: &str) -> HeadStamp {
    if spelling == "empty" {
        HeadStamp::Empty
    } else {
        SessionEventId::parse(spelling)
            .map(HeadStamp::Event)
            .unwrap_or(HeadStamp::Empty)
    }
}
