//! The headless product driver: one `CoreBridge`, one [`WorkspaceState`], and
//! every product decision the window renders.
//!
//! A frontend owns exactly one driver. It feeds user intent in as method
//! calls or [`FrontendAction`]s, drains bridge traffic with
//! [`FrontendDriver::poll`]/[`FrontendDriver::next_event`], and interprets the
//! returned [`Effect`]s in its own idiom — GPUI turns them into toasts,
//! scroll corrections, and `cx.notify()`.
//!
//! Replies and internal timers arrive on a `std::sync::mpsc` inbox; each
//! deposit calls the `wake` callback supplied at construction so a UI
//! executor can schedule a drain. Events keep their own channel so a
//! frontend can pace streamed deltas (the desktop's typewriter frames)
//! while control messages are handled immediately.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use mycode_app::HomeLayout;
use mycode_app::{
    BranchId, BridgeCommand, BridgeEvent, BridgeReply, CoreBridge, HeadStamp, SessionEventId,
    SessionId,
};

use crate::view_model::{FrontendAction, MainView, MentionKind, WorkspaceState, reduce};

mod events;
mod git;
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
    /// Put picked attachments back in the composer after a send did not
    /// take them: a failed send or a dropped welcome start.
    AttachmentsRestored(mycode_app::Attachments),
    /// One ask-card free-text field was cleared (a choice replaced typing).
    AskTextCleared {
        /// Question row index.
        index: usize,
    },
    /// Every ask-card field should reset (a new card, the card answered or
    /// dismissed, or the conversation left the screen).
    AskReset,
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
    /// Only the newest settings read may replace the editor's binding.
    Settings {
        generation: u64,
    },
    /// A manual refresh owns its toast; a concurrent lazy read does not.
    CatalogRefresh,
    Open(String),
    Session {
        session: String,
        branch: Option<String>,
    },
    CancelSubagent {
        session: String,
        call_id: String,
    },
    Mention {
        session: String,
        query: String,
    },
    /// A row delete: the reply decides whether the session actually went
    /// away before the UI forgets its bindings.
    DeletedSession(String),
    /// A recall result guarded by the requesting view, head, and generation.
    Recall(Box<RecallRequest>),
    /// A steer: a refusal hands `text` back to its chat's queue, on screen
    /// or parked.
    Steer {
        session: String,
        text: String,
    },
    /// A steer retraction: on success the text leaves that chat's pending
    /// strip and goes back to its composer.
    RetractSteer {
        session: String,
        text: String,
    },
}

/// The view and text associated with one pending message recall.
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
            BridgeCommand::CancelSubagent {
                session_id,
                call_id,
            } => Self::CancelSubagent {
                session: session_id.clone(),
                call_id: call_id.clone(),
            },
            BridgeCommand::CancelChat { session_id }
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
            BridgeCommand::Steer { session_id, text } => Self::Steer {
                session: session_id.clone(),
                text: text.clone(),
            },
            BridgeCommand::RetractSteer { session_id, text } => Self::RetractSteer {
                session: session_id.clone(),
                text: text.clone(),
            },
            _ => Self::Global,
        }
    }

    fn applies(&self, vm: &WorkspaceState, pending_open: Option<&str>) -> bool {
        match self {
            Self::Global | Self::Settings { .. } | Self::CatalogRefresh => true,
            Self::Open(session) => pending_open == Some(session.as_str()),
            Self::Session { session, branch } => vm.active.as_ref().is_some_and(|active| {
                &active.session_id == session
                    && branch
                        .as_ref()
                        .is_none_or(|branch| branch == &active.branch_id)
            }),
            Self::CancelSubagent { session, .. } => vm
                .active
                .as_ref()
                .is_some_and(|active| &active.session_id == session),
            Self::Mention { session, query } => {
                vm.active
                    .as_ref()
                    .is_some_and(|active| &active.session_id == session)
                    && vm.mention.as_ref().is_some_and(|mention| {
                        mention.kind == MentionKind::File && &mention.fragment == query
                    })
            }
            // Delete, recall, and steer replies carry their own staleness
            // checks.
            Self::DeletedSession(_)
            | Self::Recall(_)
            | Self::Steer { .. }
            | Self::RetractSteer { .. } => true,
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
    /// The watched tree changed and its burst of events settled.
    GitTreeChanged { generation: u64 },
    /// A `git status` read finished.
    GitStatusRead {
        generation: u64,
        root: String,
        snapshot: Box<crate::git_status::GitSnapshot>,
    },
    /// A `git diff` read finished.
    GitDiffRead {
        generation: u64,
        path: String,
        diff: String,
    },
}

mod pump;
use self::pump::block_on;

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

/// A stop the user asked for, and the draft that goes out once the turn is
/// gone. The draft waits here instead of the composer, so a message sent
/// beside the cancel cannot become the turn that cancel kills.
pub(crate) struct HeldInterrupt {
    /// Session whose turn is stopping.
    session_id: String,
    /// Draft to send once the turn is done. `None` for a bare stop.
    send: Option<String>,
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
    /// Attachments that ride the held welcome draft. They are not in the
    /// view model, so they live beside the session that will carry them.
    welcome_attachments: mycode_app::Attachments,
    /// The held welcome text is only the attachment note synthesized so the
    /// first message has a title; the durable message must not repeat it.
    welcome_send_is_note: bool,
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
    /// Invalidates settings reads started before a newer catalog arrived.
    settings_load_generation: u64,
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
    /// `GetCatalog` was already sent this session. Surface reads start
    /// lazily; every published gateway update also reloads the cached list.
    catalog_requested: bool,
    /// A successful manual refresh awaits its matching settings projection.
    catalog_refresh_toast_pending: bool,
    /// The in-flight check was started by the user, so its result may toast.
    manual_update_check: bool,
    /// The staged payload's exit swap is already armed; staging the same
    /// payload again (a manual install click, say) must not spawn another
    /// helper copy.
    update_armed: bool,
    /// Version a self-update installed since the last launch. Toasted once
    /// the first settings reply lands, so it renders in the UI language.
    applied_update: Option<String>,
    /// Invalidates asynchronous recall lookups when their conversation
    /// changes.
    recall_generation: u64,
    /// Config files reset during this launch; their names toast once.
    repaired_configs: Vec<String>,
    /// How many of [`Self::repaired_configs`] a toast already named.
    repairs_toasted: usize,
    /// A single toast for the untoasted repairs is already scheduled.
    repair_toast_armed: bool,
    /// Bundle of the import in flight.
    import_request: Option<PathBuf>,
    /// Roles resolved from built-ins, the home `agents` directory, and the
    /// open project's `.mycode/agents`. `None` until a surface asks for them.
    agent_roles: Option<Arc<mycode_app::RoleCatalog>>,
    /// Project directory the cached role catalog was built for.
    agent_roles_project: Option<String>,
    /// The roles surface was opened again, or the project changed.
    agent_roles_stale: bool,
    /// The `LoadUiState` reply landed (either way), so the remembered
    /// project no longer overwrites one the frontend picks.
    ui_state_loaded: bool,
    /// The install-magpie notice has been shown; persisted in `ui.json`.
    magpie_prompted: bool,
    /// Session opened while its turn was running. Events committed between
    /// the core reading that snapshot and the reply landing here were
    /// dropped as belonging to another chat, so the transcript reloads from
    /// the ledger once the turn ends.
    resync_on_turn_end: Option<String>,
    /// A `SendMessage` waiting for a reopen of the same chat to land. That
    /// reopen's snapshot predates the message; applied after the send's
    /// reply, it would wipe the message off the screen.
    held_send: Option<BridgeCommand>,
    /// Attachments of sends still waiting for their `Sent` reply, keyed by
    /// session. A failed send hands them back instead of dropping them.
    pending_send_attachments: BTreeMap<String, mycode_app::Attachments>,
    /// The turn being stopped, and the draft that goes out once it is gone;
    /// see [`FrontendDriver::interrupt_send`].
    held_interrupt: Option<HeldInterrupt>,
    /// The open folder's git status and opened diff; see [`git`].
    git: git::GitWatch,
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
            welcome_attachments: mycode_app::Attachments::default(),
            welcome_send_is_note: false,
            pending_open: None,
            pending_project: None,
            pending_composer_prefill: None,
            conversation_at_tail: true,
            scroll_hold_pending: false,
            settings_load_generation: 0,
            settings_save_epoch: 0,
            settings_text_save_generation: 0,
            settings_text_save_pending: false,
            ui_state_save_generation: 0,
            mention_query: None,
            mention_generation: 0,
            catalog_requested: false,
            catalog_refresh_toast_pending: false,
            manual_update_check: false,
            update_armed: false,
            applied_update: None,
            recall_generation: 0,
            repaired_configs: Vec::new(),
            repairs_toasted: 0,
            repair_toast_armed: false,
            import_request: None,
            agent_roles: None,
            agent_roles_project: None,
            agent_roles_stale: false,
            ui_state_loaded: false,
            magpie_prompted: false,
            resync_on_turn_end: None,
            held_send: None,
            pending_send_attachments: BTreeMap::new(),
            held_interrupt: None,
            git: git::GitWatch::default(),
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
        self.reload_settings();
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

    /// Reports the reader's follow intent. Layout changes alone must not
    /// clear it; only explicit scrolling away from the tail does.
    pub fn set_conversation_at_tail(&mut self, at_tail: bool) {
        self.conversation_at_tail = at_tail;
    }

    /// Whether new content and viewport changes should stay at the tail.
    #[must_use]
    pub fn conversation_at_tail(&self) -> bool {
        self.conversation_at_tail
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
        let size = mycode_app::WindowSize {
            width: width.round().max(0.) as u32,
            height: height.round().max(0.) as u32,
        };
        if size.width == 0
            || size.height == 0
            || size.width > mycode_app::MAX_WINDOW_EDGE_PX
            || size.height > mycode_app::MAX_WINDOW_EDGE_PX
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

    /// Shows the one-time install notice when MCode has no magpie binding.
    /// Persisted so a user who dismissed it is not told again on every start.
    pub(crate) fn prompt_magpie_install(&mut self, out: &mut Vec<Effect>) {
        if self.magpie_prompted || !self.ui_state_loaded {
            return;
        }
        let active = self
            .vm
            .settings
            .as_ref()
            .is_some_and(|settings| settings.magpie.is_active());
        if active {
            return;
        }
        self.magpie_prompted = true;
        self.toast(
            crate::i18n::t(
                "Can't reach the magpie gateway. Install or start magpie (https://usemagpie.ai), \
                 then run: magpie plugin add github:MCapricorns/magpie-mycode",
                "连接不到 magpie 网关。请安装或启动 magpie（https://usemagpie.ai），\
                 然后运行：magpie plugin add github:MCapricorns/magpie-mycode",
            )
            .to_owned(),
            ToastKind::Warning,
            out,
        );
        self.persist_ui_state();
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

    /// Runs `work` on a worker thread; its message lands on this inbox.
    fn in_background(&self, work: impl FnOnce() -> DriverMsg + Send + 'static) {
        let tx = self.inbox_tx.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            if tx.send(work()).is_ok() {
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
                DriverMsg::GitTreeChanged { generation } => self.on_git_tree_changed(generation),
                DriverMsg::GitStatusRead {
                    generation,
                    root,
                    snapshot,
                } => self.finish_git_status(generation, &root, *snapshot),
                DriverMsg::GitDiffRead {
                    generation,
                    path,
                    diff,
                } => self.finish_git_diff(generation, &path, diff),
            }
        }
    }

    /// Applies one action to the view-model state, then runs the product
    /// consequences that follow from the transition (autosave, toasts,
    /// project-change side effects, tail-follow scroll, mention search).
    pub fn act(&mut self, action: FrontendAction, out: &mut Vec<Effect>) {
        let changed_conversation = match &action {
            FrontendAction::ConversationOpened(next) => {
                self.vm.active.as_ref().is_none_or(|active| {
                    active.session_id != next.session_id || active.branch_id != next.branch_id
                })
            }
            FrontendAction::SessionCreated(_)
            | FrontendAction::SessionDeleted
            | FrontendAction::ConversationParked => true,
            _ => false,
        };
        if changed_conversation {
            self.conversation_at_tail = true;
        }
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
        // A streamed fragment is the per-token path and cannot move the open
        // project, so the snapshot (a String copy) is skipped for it.
        let previous_project = (!matches!(
            action,
            FrontendAction::ChatDelta(_) | FrontendAction::ChatThinkingDelta(_)
        ))
        .then(|| self.vm.project_dir.clone());
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
        // magpie rewrites its own `mcp.json`; reading it when its page opens
        // shows what the Library has now, not what it had at startup.
        if matches!(
            action,
            FrontendAction::ShowSettingsSection(crate::view_model::SettingsSection::Mcp)
        ) {
            self.reload_mcp_library(out);
        }
        reduce(&mut self.vm, action);
        if let Some(previous_project) = previous_project
            && self.vm.project_dir != previous_project
        {
            self.agent_roles_stale = true;
            self.rewatch_git();
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
        self.agent_roles = Some(Arc::new(mycode_app::discover_roles(&self.home, root)));
        self.agent_roles_project = project;
        self.agent_roles_stale = false;
    }

    /// Role catalog last resolved for settings. Built-ins only before the
    /// first resolution. Shared, so a settings paint does not copy every
    /// role prompt.
    pub fn agent_roles(&self) -> Arc<mycode_app::RoleCatalog> {
        self.agent_roles
            .clone()
            .unwrap_or_else(|| Arc::new(mycode_app::builtin_roles()))
    }

    /// Marks the roles cache dirty so the next read re-scans.
    pub fn invalidate_agent_roles(&mut self) {
        self.agent_roles_stale = true;
    }

    /// Reads the MCP servers magpie's Library writes, so the settings page,
    /// its nav count, and the composer's `/` menu list one catalog.
    pub(crate) fn reload_mcp_library(&mut self, out: &mut Vec<Effect>) {
        let (servers, error) = match crate::global_mcp_servers(&self.home) {
            Ok(servers) => (servers, None),
            // The page shows the reason; an unreadable file must not look
            // like an empty Library.
            Err(message) => (Vec::new(), Some(message)),
        };
        self.act(FrontendAction::McpLibraryLoaded { servers, error }, out);
    }

    /// Loads the cached gateway catalog the first time a surface needs it.
    pub fn ensure_catalog(&mut self) {
        if self.catalog_requested || self.vm.catalog.is_some() || !self.catalog_needed() {
            return;
        }
        self.catalog_requested = true;
        self.dispatch(BridgeCommand::GetCatalog);
    }

    /// A manual catalog reload; its reply scope owns the refreshed toast.
    pub fn refresh_catalog(&mut self) {
        self.dispatch_scoped(BridgeCommand::RefreshCatalog, ReplyScope::CatalogRefresh);
    }

    /// Whether a mounted surface reads the provider catalog document.
    ///
    /// The composer chips and the context meter consult it on every chat
    /// paint — the model label, the thinking rungs, and the window that
    /// sizes the ring all resolve through it — so a selected model counts
    /// as a need with no menu open. An unsolicited `CatalogUpdated` always
    /// reads the new document and binding, even before the first selection.
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
    pub fn note_config_repairs(&mut self, repairs: &[mycode_app::DocumentRepair]) {
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
