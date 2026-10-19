//! End-to-end turns over scripted transports: history replay, interrupts,
//! cancels during setup, and the published turn controls.

mod compaction;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mycode_agent::session::{EventKind, HeadStamp};
use mycode_config::{
    AppSettings, AuthorityRevision, HomeLayout, ProviderSecrets, ProviderSettings,
    replace_app_settings, replace_provider_secrets,
};
use mycode_core::ProviderError;
use mycode_providers::{ResolvedProvider, SseTransport, TransportCall};

use super::{TurnControl, chat_turn, run_chat_turn_on};
use crate::state::CoreState;

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
    let mut settings = AppSettings::default();
    settings.providers.push(ProviderSettings {
        id: "local".to_owned(),
        kind: "openai-completions".to_owned(),
        base_url: "https://example.com/v1".to_owned(),
        models: vec!["test-model".to_owned()],
        enabled: true,
        context_limit: Some(200_000),
        max_output: None,
        hidden_models: Vec::new(),
    });
    replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("settings");
    replace_provider_secrets(
        &home,
        AuthorityRevision::ABSENT,
        &ProviderSecrets::new().with_key("local", Some("test-key")),
    )
    .expect("secrets");
    let cwd = home.root().join(mycode_config::SCRATCH_DIR);
    std::fs::create_dir_all(&cwd).expect("cwd");
    let catalog = mycode_providers::catalog::current(&home);
    let state = Arc::new(CoreState::new(home, catalog, Vec::new()));
    let created = state.service.create(None).await.expect("session");
    let provider = settings.providers[0].clone();
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
    let control = TurnControl::register(&fixture.state, fixture.session.as_str());
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
        transport,
        &control,
    )
    .await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (outcome, events)
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
        let control = TurnControl::register(&state, &session_id);
        let outcome = run_chat_turn_on(
            &state,
            &tx,
            &session_id,
            session,
            branch,
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
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let control = TurnControl::register(&fixture.state, fixture.session.as_str());
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
    assert_eq!(outcome, Err(crate::protocol::CHAT_CANCELLED.to_owned()));
    assert!(transport.bodies.lock().expect("bodies").is_empty());
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
    let mut settings = AppSettings::default();
    settings.providers.push(ProviderSettings {
        id: "local".to_owned(),
        kind: "openai-completions".to_owned(),
        base_url: "https://example.com/v1".to_owned(),
        models: vec!["test-model".to_owned()],
        enabled: true,
        // ~22k token auto threshold. The prior history fits, so setup
        // does not compact; the request hook also counts the large
        // prompt and starts a summary before the turn has started.
        context_limit: Some(26_000),
        max_output: None,
        hidden_models: Vec::new(),
    });
    replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("settings");
    replace_provider_secrets(
        &home,
        AuthorityRevision::ABSENT,
        &ProviderSecrets::new().with_key("local", Some("test-key")),
    )
    .expect("secrets");
    let cwd = home.root().join(mycode_config::SCRATCH_DIR);
    std::fs::create_dir_all(&cwd).expect("cwd");
    let catalog = mycode_providers::catalog::current(&home);
    let state = Arc::new(CoreState::new(home, catalog, Vec::new()));
    let created = state.service.create(None).await.expect("session");
    let mut head = HeadStamp::Empty;
    for text in ["prior work", &"P".repeat(120_000)] {
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
    let provider = settings.providers[0].clone();
    let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
        .expect("resolve");
    let session_id = created.session_id.as_str().to_owned();
    let turn = tokio::spawn({
        let state = Arc::clone(&state);
        let transport: Arc<dyn SseTransport> = transport.clone();
        let session_id = session_id.clone();
        async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let control = TurnControl::register(&state, &session_id);
            let outcome = run_chat_turn_on(
                &state,
                &tx,
                &session_id,
                created.session_id,
                created.branch_id,
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

/// A crash mid-tool leaves an assistant call with no result in the ledger.
/// The next turn must still send a request the wire accepts.
#[tokio::test]
async fn a_call_the_ledger_never_answered_is_paired_on_the_next_turn() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(HistoryTransport {
        bodies: Mutex::new(Vec::new()),
        hang_partial: false,
        partial_ready: Arc::new(tokio::sync::Notify::new()),
    });
    let writer = crate::ledger::HeadWriter::new(
        fixture.state.service.clone(),
        fixture.session.clone(),
        fixture.branch.clone(),
        HeadStamp::Empty,
    );
    writer
        .write(EventKind::Message, PROMPT_A.as_bytes())
        .await
        .expect("prompt A");
    let step = mycode_core::AssistantMessage {
        blocks: vec![mycode_core::ContentBlock::ToolCall(
            mycode_core::ToolCall::new("call_lost", "read", serde_json::json!({"path": "a.txt"})),
        )],
        usage: None,
        stop_reason: mycode_core::StopReason::ToolUse,
    };
    writer
        .write(
            EventKind::Message,
            &serde_json::to_vec(&step).expect("step"),
        )
        .await
        .expect("step");
    // The process died here: the call opened, its result never landed.
    writer
        .open_call("call_lost", "read", "a.txt")
        .await
        .expect("open call");
    writer
        .write(EventKind::Message, PROMPT_B.as_bytes())
        .await
        .expect("prompt B");
    let (outcome, _) = run_scripted_turn(&fixture, transport.clone()).await;
    outcome.expect("turn B");
    let body: serde_json::Value =
        serde_json::from_str(&prompt_b_body(&transport)).expect("request JSON");
    let messages = body["messages"].as_array().expect("messages");
    let answer = messages
        .iter()
        .position(|message| message["role"] == "tool" && message["tool_call_id"] == "call_lost")
        .expect("the lost call was answered");
    let call = messages
        .iter()
        .position(|message| message["role"] == "assistant")
        .expect("assistant step");
    assert_eq!(answer, call + 1, "the answer follows its call: {body}");
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[tokio::test]
async fn chat_turn_fails_fast_when_the_branch_is_missing() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(HistoryTransport {
        bodies: Mutex::new(Vec::new()),
        hang_partial: false,
        partial_ready: Arc::new(tokio::sync::Notify::new()),
    });
    let wrong_branch = mycode_agent::session::BranchId::generate().expect("branch id");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let control = TurnControl::register(&fixture.state, fixture.session.as_str());
    let outcome = run_chat_turn_on(
        &fixture.state,
        &tx,
        fixture.session.as_str(),
        fixture.session.clone(),
        wrong_branch,
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
    assert_eq!(outcome, Err("not found".to_owned()));
    assert!(transport.bodies.lock().expect("bodies").is_empty());
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[tokio::test]
async fn cancel_and_steer_guards_unpublish_under_lock_contention() {
    let fixture = turn_fixture().await;
    let session_id = fixture.session.as_str().to_owned();
    let control = TurnControl::register(&fixture.state, &session_id);
    let cancels = Arc::clone(&fixture.state.turn_cancels);
    let inboxes = Arc::clone(&fixture.state.steer_inboxes);
    let cancel_hold = cancels.lock();
    let inbox_hold = inboxes.lock();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        started_tx.send(()).expect("announce drop");
        drop(control);
        done_tx.send(()).expect("announce cleanup");
    });
    started_rx.recv().expect("dropper started");
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "cleanup must wait for the held locks rather than abandoning registrations"
    );
    drop(cancel_hold);
    drop(inbox_hold);
    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("cleanup completes after unlock");
    dropper.join().expect("dropper thread");
    assert!(
        !fixture.state.turn_cancels.lock().contains_key(&session_id),
        "contended drop leaked the turn cancel token"
    );
    assert!(
        !fixture.state.steer_inboxes.lock().contains_key(&session_id),
        "contended drop leaked the steer inbox"
    );
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[tokio::test]
async fn a_steer_after_the_last_request_is_returned_not_dropped() {
    let fixture = turn_fixture().await;
    let session_id = fixture.session.as_str().to_owned();
    let mut control = TurnControl::register(&fixture.state, &session_id);
    // What `Steer` does: push under the map lock while the turn is live.
    fixture
        .state
        .steer_inboxes
        .lock()
        .get(&session_id)
        .expect("published inbox")
        .push("one more thing");
    assert_eq!(control.retire_steers(), vec!["one more thing".to_owned()]);
    assert!(
        !fixture.state.steer_inboxes.lock().contains_key(&session_id),
        "a retired turn must not accept steers it can no longer send"
    );
    assert!(control.retire_steers().is_empty());
    let _ = std::fs::remove_dir_all(&fixture.root);
}

/// Answers the first request with a `read` tool call, the second with text.
struct CallTransport {
    bodies: Mutex<Vec<String>>,
}

fn tool_call_sse() -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_a",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"a.txt\"}"}
            }]},
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

impl SseTransport for CallTransport {
    fn post<'a>(
        &'a self,
        call: TransportCall,
        _cancel: tokio_util::sync::CancellationToken,
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
            let count = {
                let mut bodies = self.bodies.lock().expect("bodies");
                bodies.push(body);
                bodies.len()
            };
            let sse = if count == 1 {
                tool_call_sse()
            } else {
                reply_sse("REPLY done")
            };
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            sender
                .send(Ok(bytes::Bytes::from(sse)))
                .await
                .expect("queue reply body");
            Ok(stream)
        })
    }
}

/// Live transcript rows and replayed rows pair on the ledger call identity:
/// a mid-turn reopen swaps live rows for replayed ones, so a result that
/// lands after the reopen must still match its call.
#[tokio::test]
async fn live_tool_rows_key_on_the_ledger_call_identity() {
    let fixture = turn_fixture().await;
    let transport = Arc::new(CallTransport {
        bodies: Mutex::new(Vec::new()),
    });
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &HeadStamp::Empty,
        PROMPT_A,
    )
    .await
    .expect("prompt");
    let (outcome, events) = run_scripted_turn(&fixture, transport).await;
    outcome.expect("turn");
    let (call_id, provider_call_id) = events
        .iter()
        .find_map(|event| match event {
            crate::BridgeEvent::ToolStarted {
                call_id,
                provider_call_id,
                name,
                ..
            } if name == "read" => Some((call_id.clone(), provider_call_id.clone())),
            _ => None,
        })
        .expect("ToolStarted for read");
    assert_eq!(provider_call_id, "call_a");
    assert!(call_id.starts_with("call1-"), "{call_id}");
    let result_id = events
        .iter()
        .find_map(|event| match event {
            crate::BridgeEvent::ToolCompleted { entry, .. } => entry.call_id.clone(),
            _ => None,
        })
        .expect("ToolCompleted");
    assert_eq!(result_id, call_id);
    // The replayed projection keys the same rows the same way.
    let conversation = crate::ledger::open_conversation(&fixture.state.service, &fixture.session)
        .await
        .expect("reopen");
    let replayed_call = conversation
        .entries
        .iter()
        .find(|entry| entry.kind == crate::EntryKind::ToolCall)
        .expect("replayed call row");
    let replayed_result = conversation
        .entries
        .iter()
        .find(|entry| entry.kind == crate::EntryKind::ToolResult)
        .expect("replayed result row");
    assert_eq!(replayed_call.call_id.as_deref(), Some(call_id.as_str()));
    assert_eq!(replayed_result.call_id.as_deref(), Some(call_id.as_str()));
    let _ = std::fs::remove_dir_all(&fixture.root);
}
