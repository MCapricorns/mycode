//! End-to-end turns over scripted transports: history replay, interrupts,
//! cancels during setup, and the published turn controls.

mod compaction;
mod prefix;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mycode_agent::session::{EventKind, HeadStamp};
use mycode_config::{
    AppSettings, AuthorityRevision, HomeLayout, ProviderSettings, replace_app_settings,
};
use mycode_core::ProviderError;
use mycode_providers::{ResolvedProvider, SseTransport, TransportCall};

use super::{TurnControl, chat_turn, end_turn, run_chat_turn_on};
use crate::state::CoreState;

/// The gateway list a fixture publishes: one `magpie` row.
pub(super) fn test_catalog(models: &[(&str, u64)]) -> mycode_providers::catalog::CatalogDocument {
    use mycode_providers::catalog::{CatalogDocument, CatalogModel, CatalogProvider};
    CatalogDocument {
        providers: vec![CatalogProvider {
            id: "magpie".to_owned(),
            name: "Magpie".to_owned(),
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            models: models
                .iter()
                .map(|(id, context)| CatalogModel {
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    context: *context,
                    ..CatalogModel::default()
                })
                .collect(),
            ..CatalogProvider::default()
        }],
    }
}

pub(super) fn scratch() -> (std::path::PathBuf, HomeLayout) {
    // The clock alone is too coarse on Windows: parallel tests calling
    // this in the same tick shared one home and saw each other's files.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "mycode-turn-compact-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        crate::unix_now().as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("scratch");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let home = HomeLayout::from_root(&root).expect("home");
    (root, home)
}

const PROMPT_A: &str = "PROMPT_A write a snake game";
const REPLY_A: &str = "REPLY_A here is the game";
const PROMPT_B: &str = "PROMPT_B also add a score";
const PARTIAL_A: &str = "PARTIAL_A game loop";

struct HistoryTransport {
    bodies: Mutex<Vec<String>>,
    hang_partial: bool,
    partial_ready: Arc<tokio::sync::Notify>,
}

fn reply_sse(text: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {"content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 3}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn partial_sse(text: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{"delta": {"content": text}}]
    });
    format!("data: {chunk}\n\n")
}

impl SseTransport for HistoryTransport {
    fn post<'a>(
        &'a self,
        call: TransportCall,
        cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<mycode_providers::transport::ByteStream, ProviderError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let body = String::from_utf8_lossy(&call.body).into_owned();
            self.bodies.lock().expect("bodies").push(body.clone());
            let is_b = body.contains(PROMPT_B);
            if !is_b && self.hang_partial {
                let partial = bytes::Bytes::from(partial_sse(PARTIAL_A));
                let ready = Arc::clone(&self.partial_ready);
                let (sender, stream) = tokio::sync::mpsc::channel(1);
                // `reserve` resolves only after the driver received the
                // partial chunk (the buffer slot freed), so the cancel waits
                // until that text was taken — the same handshake the chained
                // streams expressed through their second poll.
                tokio::spawn(async move {
                    sender.send(Ok(partial)).await.expect("queue partial chunk");
                    if sender.reserve().await.is_err() {
                        return;
                    }
                    ready.notify_one();
                    cancel.cancelled().await;
                    let _ = sender
                        .send(Err(ProviderError::new(
                            mycode_core::ProviderErrorKind::Cancelled,
                        )))
                        .await;
                });
                return Ok(stream);
            }
            let reply = if is_b { "REPLY_B ok" } else { REPLY_A };
            let sse = reply_sse(reply);
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            sender
                .send(Ok(bytes::Bytes::from(sse)))
                .await
                .expect("queue reply body");
            Ok(stream)
        })
    }
}

struct TurnFixture {
    root: PathBuf,
    state: Arc<CoreState>,
    settings: AppSettings,
    provider: ProviderSettings,
    resolved: ResolvedProvider,
    cwd: PathBuf,
    session: mycode_agent::session::SessionId,
    branch: mycode_agent::session::BranchId,
}

async fn turn_fixture() -> TurnFixture {
    let (root, home) = scratch();
    let settings = AppSettings {
        magpie: mycode_config::MagpieSettings {
            enabled: true,
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            api_key: "test-key".to_owned(),
            model: "test-model".to_owned(),
            hidden_models: Vec::new(),
        },
        ..AppSettings::default()
    };
    replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("settings");
    let cwd = home.root().join(mycode_config::SCRATCH_DIR);
    std::fs::create_dir_all(&cwd).expect("cwd");
    let state = Arc::new(CoreState::new(home, Vec::new()));
    *state.catalog.write().expect("catalog") = Arc::new(test_catalog(&[("test-model", 200_000)]));
    let created = state.service.create(None).await.expect("session");
    let provider = settings
        .magpie
        .is_active()
        .then(|| {
            crate::magpie::provider_row(&settings.magpie, &state.models()).expect("gateway row")
        })
        .expect("magpie is active");
    let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
        .expect("resolve");
    TurnFixture {
        root,
        state,
        settings,
        provider,
        resolved,
        cwd,
        session: created.session_id,
        branch: created.branch_id,
    }
}

fn parse_head(spelling: &str) -> HeadStamp {
    match mycode_agent::session::SessionEventId::parse(spelling) {
        Some(id) => HeadStamp::Event(id),
        None => HeadStamp::Empty,
    }
}

async fn run_scripted_turn(
    fixture: &TurnFixture,
    transport: Arc<dyn SseTransport>,
) -> (Result<(), String>, Vec<crate::BridgeEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = run_turn_to_end(fixture, &tx, transport).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (outcome, events)
}

/// What `chat_turn` does once setup resolved the provider: run the turn on
/// `transport`, then report its end.
async fn run_turn_to_end(
    fixture: &TurnFixture,
    tx: &crate::BridgeEventTx,
    transport: Arc<dyn SseTransport>,
) -> Result<(), String> {
    let mut control = TurnControl::register(&fixture.state, fixture.session.as_str());
    let outcome = run_chat_turn_on(
        &fixture.state,
        tx,
        fixture.session.as_str(),
        fixture.session.clone(),
        fixture.branch.clone(),
        "test-model",
        None,
        fixture.cwd.clone(),
        fixture.resolved.clone(),
        fixture.settings.clone(),
        fixture.provider.clone(),
        transport,
        &control,
    )
    .await;
    end_turn(
        &fixture.state,
        tx,
        fixture.session.as_str(),
        &fixture.session,
        &fixture.branch,
        &mut control,
        outcome,
    )
    .await
}

fn prompt_b_body(transport: &HistoryTransport) -> String {
    transport
        .bodies
        .lock()
        .expect("bodies")
        .iter()
        .rev()
        .find(|body| body.contains(PROMPT_B))
        .cloned()
        .expect("B request was sent")
}

fn assert_cache_key(body: &str, session_id: &str) {
    assert!(
        body.contains(&format!("\"prompt_cache_key\":\"{session_id}\"")),
        "prompt cache key missing: {body}"
    );
}

/// Queue path: A finishes (ChatDone), then B is sent with that head.
#[tokio::test]
async fn queued_follow_up_after_chat_done_carries_history() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(HistoryTransport {
        bodies: Mutex::new(Vec::new()),
        hang_partial: false,
        partial_ready: Arc::new(tokio::sync::Notify::new()),
    });
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &HeadStamp::Empty,
        PROMPT_A,
    )
    .await
    .expect("prompt A");
    let (outcome, events) = run_scripted_turn(&fixture, transport.clone()).await;
    outcome.expect("turn A");
    let done_head = events
        .iter()
        .find_map(|event| match event {
            crate::BridgeEvent::ChatDone { head, .. } => Some(head.clone()),
            _ => None,
        })
        .expect("ChatDone");
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &parse_head(&done_head),
        PROMPT_B,
    )
    .await
    .expect("prompt B");
    let (outcome, _) = run_scripted_turn(&fixture, transport.clone()).await;
    outcome.expect("turn B");
    let body = prompt_b_body(&transport);
    assert!(body.contains(PROMPT_A), "B lost A's prompt: {body}");
    assert!(body.contains(REPLY_A), "B lost A's reply: {body}");
    assert!(body.contains(PROMPT_B), "B lost its own prompt: {body}");
    assert_cache_key(&body, fixture.session.as_str());
    let _ = std::fs::remove_dir_all(&fixture.root);
}

/// Interrupt path: A streams a partial, the user cancels, then B is sent.
#[tokio::test]
async fn interrupt_then_send_keeps_partial_and_cache_key() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(HistoryTransport {
        bodies: Mutex::new(Vec::new()),
        hang_partial: true,
        partial_ready: Arc::new(tokio::sync::Notify::new()),
    });
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &HeadStamp::Empty,
        PROMPT_A,
    )
    .await
    .expect("prompt A");
    let state = Arc::clone(&fixture.state);
    let session = fixture.session.clone();
    let branch = fixture.branch.clone();
    let cwd = fixture.cwd.clone();
    let resolved = fixture.resolved.clone();
    let settings = fixture.settings.clone();
    let provider = fixture.provider.clone();
    let running = transport.clone();
    let session_id = session.as_str().to_owned();
    let turn_a = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut control = TurnControl::register(&state, &session_id);
        let outcome = run_chat_turn_on(
            &state,
            &tx,
            &session_id,
            session.clone(),
            branch.clone(),
            "test-model",
            None,
            cwd,
            resolved,
            settings,
            provider,
            running,
            &control,
        )
        .await;
        let outcome = end_turn(
            &state,
            &tx,
            &session_id,
            &session,
            &branch,
            &mut control,
            outcome,
        )
        .await;
        drop(tx);
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        (outcome, events)
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        transport.partial_ready.notified(),
    )
    .await
    .expect("partial text was not streamed");
    let token = fixture
        .state
        .turn_cancels
        .lock()
        .get(fixture.session.as_str())
        .cloned()
        .expect("turn token");
    token.cancel();
    let (outcome, events) = tokio::time::timeout(std::time::Duration::from_secs(10), turn_a)
        .await
        .expect("turn A did not unwind")
        .expect("turn task");
    outcome.expect("cancelled turn still returns");
    let failed = events.iter().find_map(|event| match event {
        crate::BridgeEvent::ChatFailed { message, head, .. } => {
            Some((message.clone(), head.clone()))
        }
        _ => None,
    });
    let (message, failed_head) = failed.expect("ChatFailed");
    assert_eq!(message, crate::protocol::CHAT_CANCELLED);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, crate::BridgeEvent::ChatDone { .. })),
        "a user cancel must not emit ChatDone"
    );
    assert!(
        !failed_head.is_empty(),
        "cancel must report the ledger head"
    );
    let history = crate::ledger::ledger_history(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &parse_head(&failed_head),
    )
    .await
    .expect("ledger");
    let rendered = history
        .iter()
        .map(|message| match message.as_ref() {
            mycode_core::Message::Assistant(assistant) => assistant.text(),
            mycode_core::Message::User(user) => super::pump::plain_user_text(user),
            mycode_core::Message::ToolResult(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains(PARTIAL_A),
        "ledger dropped the partial: {rendered}"
    );
    assert!(
        rendered.contains("[error] the response was interrupted: interrupted by user"),
        "ledger dropped the interruption line: {rendered}"
    );
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &parse_head(&failed_head),
        PROMPT_B,
    )
    .await
    .expect("prompt B");
    let (outcome, _) = run_scripted_turn(&fixture, transport.clone()).await;
    outcome.expect("turn B");
    let body = prompt_b_body(&transport);
    assert!(body.contains(PROMPT_A), "B lost A's prompt: {body}");
    assert!(body.contains(PARTIAL_A), "B lost the partial: {body}");
    assert!(
        body.contains("[error] the response was interrupted: interrupted by user"),
        "B lost the interruption marker: {body}"
    );
    assert!(body.contains(PROMPT_B), "B lost its own prompt: {body}");
    assert_cache_key(&body, fixture.session.as_str());
    let _ = std::fs::remove_dir_all(&fixture.root);
}

/// The UI reopens a session when it hears the turn end. A token still
/// published then reads as a running turn: the composer sticks on Stop and
/// every later send queues behind a turn that is gone. So the turn leaves
/// both registries before its terminal event and the returned steers.
#[tokio::test]
async fn a_turn_is_unpublished_before_its_end_is_reported() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(HistoryTransport {
        bodies: Mutex::new(Vec::new()),
        hang_partial: false,
        partial_ready: Arc::new(tokio::sync::Notify::new()),
    });
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &HeadStamp::Empty,
        PROMPT_A,
    )
    .await
    .expect("prompt A");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::clone(&fixture.state);
    let session_id = fixture.session.as_str().to_owned();
    // Runs as soon as the terminal event is queued, before the turn task
    // gets the thread back.
    let watcher = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if matches!(
                event,
                crate::BridgeEvent::ChatDone { .. } | crate::BridgeEvent::ChatFailed { .. }
            ) {
                return (
                    matches!(event, crate::BridgeEvent::ChatDone { .. }),
                    state.live_work(&session_id),
                    state.steer_inboxes.lock().contains_key(&session_id),
                );
            }
        }
        panic!("the turn never reported its end");
    });
    run_turn_to_end(&fixture, &tx, transport)
        .await
        .expect("turn A");
    drop(tx);
    let (done, live, steerable) = tokio::time::timeout(std::time::Duration::from_secs(10), watcher)
        .await
        .expect("watcher timed out")
        .expect("watcher");
    assert!(done, "the turn should have finished");
    assert_eq!(
        live, None,
        "ChatDone went out while the turn was still listed"
    );
    assert!(!steerable, "ChatDone went out while the turn took steers");
    let _ = std::fs::remove_dir_all(&fixture.root);
}

/// A reopen during `/compact` must show the compaction, not a running turn:
/// `CompactFinished` ends a compaction on screen, never a chat turn.
#[tokio::test]
async fn live_work_tells_a_compaction_from_a_turn() {
    let fixture = turn_fixture().await;
    let session_id = fixture.session.as_str();
    let mut compaction = TurnControl::register_cancel_only(&fixture.state, session_id);
    assert_eq!(
        fixture.state.live_work(session_id),
        Some(crate::state::WorkKind::Compaction)
    );
    assert!(compaction.retire().is_empty());
    assert_eq!(fixture.state.live_work(session_id), None);
    let turn = TurnControl::register(&fixture.state, session_id);
    assert_eq!(
        fixture.state.live_work(session_id),
        Some(crate::state::WorkKind::Turn)
    );
    drop(turn);
    assert_eq!(fixture.state.live_work(session_id), None);
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[tokio::test]
async fn chat_turn_publishes_its_controls_before_setup_runs() {
    let fixture = turn_fixture().await;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let session_id = fixture.session.as_str();
    let turn = chat_turn(
        Arc::clone(&fixture.state),
        tx,
        fixture.session.clone(),
        fixture.branch.clone(),
        "local".to_owned(),
        "test-model".to_owned(),
        None,
    );
    // Not polled yet: a cancel or steer sent now must still find the turn.
    assert!(fixture.state.turn_cancels.lock().contains_key(session_id));
    assert!(fixture.state.steer_inboxes.lock().contains_key(session_id));
    drop(turn);
    assert!(!fixture.state.turn_cancels.lock().contains_key(session_id));
    assert!(!fixture.state.steer_inboxes.lock().contains_key(session_id));
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[tokio::test]
async fn a_turn_cancelled_during_setup_never_reaches_the_model() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(HistoryTransport {
        bodies: Mutex::new(Vec::new()),
        hang_partial: false,
        partial_ready: Arc::new(tokio::sync::Notify::new()),
    });
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &HeadStamp::Empty,
        PROMPT_A,
    )
    .await
    .expect("prompt A");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut control = TurnControl::register(&fixture.state, fixture.session.as_str());
    control.cancel.cancel();
    let outcome = run_chat_turn_on(
        &fixture.state,
        &tx,
        fixture.session.as_str(),
        fixture.session.clone(),
        fixture.branch.clone(),
        "test-model",
        None,
        fixture.cwd.clone(),
        fixture.resolved.clone(),
        fixture.settings.clone(),
        fixture.provider.clone(),
        transport.clone(),
        &control,
    )
    .await;
    let outcome = end_turn(
        &fixture.state,
        &tx,
        fixture.session.as_str(),
        &fixture.session,
        &fixture.branch,
        &mut control,
        outcome,
    )
    .await;
    assert_eq!(outcome, Err(crate::protocol::CHAT_CANCELLED.to_owned()));
    assert!(transport.bodies.lock().expect("bodies").is_empty());
    assert!(
        matches!(
            rx.try_recv(),
            Ok(crate::BridgeEvent::ChatFailed { message, .. })
                if message == crate::protocol::CHAT_CANCELLED
        ),
        "a turn cancelled during setup still ends quietly"
    );
    let _ = std::fs::remove_dir_all(&fixture.root);
}

/// Hangs every compaction summary until its request is cancelled and
/// records whether the model was ever asked for a reply.
struct HangingSummaryTransport {
    summary_started: Arc<tokio::sync::Notify>,
    kinds: Mutex<Vec<&'static str>>,
}

impl SseTransport for HangingSummaryTransport {
    fn post<'a>(
        &'a self,
        call: TransportCall,
        cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<mycode_providers::transport::ByteStream, ProviderError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let body = String::from_utf8_lossy(&call.body).into_owned();
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                self.kinds.lock().expect("kinds").push("summary");
                self.summary_started.notify_one();
                tokio::spawn(async move {
                    cancel.cancelled().await;
                    let _ = sender
                        .send(Err(ProviderError::new(
                            mycode_core::ProviderErrorKind::Cancelled,
                        )))
                        .await;
                });
                return Ok(stream);
            }
            self.kinds.lock().expect("kinds").push("reply");
            sender
                .send(Ok(bytes::Bytes::from(reply_sse("unexpected reply"))))
                .await
                .expect("queue reply body");
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn a_cancel_inside_the_first_request_hook_still_ends_the_turn() {
    let (root, home) = scratch();
    let settings = AppSettings {
        magpie: mycode_config::MagpieSettings {
            enabled: true,
            base_url: "http://127.0.0.1:3425/v1".to_owned(),
            api_key: "test-key".to_owned(),
            model: "test-model".to_owned(),
            hidden_models: Vec::new(),
        },
        ..AppSettings::default()
    };
    replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("settings");
    let cwd = home.root().join(mycode_config::SCRATCH_DIR);
    std::fs::create_dir_all(&cwd).expect("cwd");
    let state = Arc::new(CoreState::new(home, Vec::new()));
    *state.catalog.write().expect("catalog") = Arc::new(test_catalog(&[("test-model", 26_000)]));
    let created = state.service.create(None).await.expect("session");
    let mut head = HeadStamp::Empty;
    // The head before the prompt must outweigh the summary that replaces
    // it, or the hook keeps the request whole.
    for text in ["prior work", &"Q".repeat(40_000), &"P".repeat(60_000)] {
        let reservation = state
            .service
            .reserve_event(
                &created.session_id,
                &created.branch_id,
                EventKind::Message,
                None,
                text.as_bytes(),
            )
            .await
            .expect("reserve");
        head = state
            .service
            .append(&created.session_id, &created.branch_id, &head, &reservation)
            .await
            .expect("append")
            .head;
    }
    let transport = Arc::new(HangingSummaryTransport {
        summary_started: Arc::new(tokio::sync::Notify::new()),
        kinds: Mutex::new(Vec::new()),
    });
    let provider =
        crate::magpie::provider_row(&settings.magpie, &state.models()).expect("gateway row");
    let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
        .expect("resolve");
    let session_id = created.session_id.as_str().to_owned();
    let turn = tokio::spawn({
        let state = Arc::clone(&state);
        let transport: Arc<dyn SseTransport> = transport.clone();
        let session_id = session_id.clone();
        async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut control = TurnControl::register(&state, &session_id);
            let outcome = run_chat_turn_on(
                &state,
                &tx,
                &session_id,
                created.session_id.clone(),
                created.branch_id.clone(),
                "test-model",
                None,
                cwd,
                resolved,
                settings,
                provider,
                transport,
                &control,
            )
            .await;
            let outcome = end_turn(
                &state,
                &tx,
                &session_id,
                &created.session_id,
                &created.branch_id,
                &mut control,
                outcome,
            )
            .await;
            drop(tx);
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            (outcome, events)
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        transport.summary_started.notified(),
    )
    .await
    .expect("the request hook never started a summary");
    let token = state
        .turn_cancels
        .lock()
        .get(&session_id)
        .cloned()
        .expect("turn token");
    token.cancel();
    let (outcome, events) = tokio::time::timeout(std::time::Duration::from_secs(10), turn)
        .await
        .expect("a cancel inside the first request hook left the turn hanging")
        .expect("turn task");
    outcome.expect("a cancelled turn still returns");
    assert!(
        events.iter().any(|event| matches!(
            event,
            crate::BridgeEvent::ChatFailed { message, .. }
                if message == crate::protocol::CHAT_CANCELLED
        )),
        "the desktop never heard the turn end: {events:?}"
    );
    assert_eq!(
        *transport.kinds.lock().expect("kinds"),
        ["summary"],
        "a cancelled turn must not ask the model for a reply"
    );
    assert!(
        !state.turn_cancels.lock().contains_key(&session_id),
        "the finished turn must unpublish its token"
    );
    let _ = std::fs::remove_dir_all(root);
}

mod pairing;
