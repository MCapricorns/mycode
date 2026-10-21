//! Compaction around a turn: the request hook's deferred card, and
//! `/compact` against a missing session or a moved head.

use std::sync::{Arc, Mutex};

use mycode_agent::session::{BranchId, EventKind, HeadStamp, SessionId};
use mycode_config::{AppSettings, AuthorityRevision, replace_app_settings};
use mycode_core::ProviderError;
use mycode_providers::{ResolvedProvider, SseTransport, TransportCall};

use super::scratch;
use crate::protocol::EntryKind;
use crate::state::CoreState;
use crate::turn::compact::{DeferredSummaries, watch_prompt_cache};
use crate::turn::{TurnControl, end_turn, manual_compact, run_chat_turn_on};

struct TurnTransport {
    kinds: Mutex<Vec<&'static str>>,
    /// How many times `COMPACTION SUMMARY` appears in the final model request.
    final_summaries: Mutex<Option<usize>>,
}

impl SseTransport for TurnTransport {
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
            let body = String::from_utf8_lossy(&call.body);
            let (kind, sse) = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                let summary = format!(
                    "handoff notes. {}",
                    "The covered prefix is replaced by this checkpoint. ".repeat(6)
                );
                ("summary", text_sse(&summary, 80))
            } else if body.contains("\"role\":\"tool\"") {
                ("final", {
                    *self.final_summaries.lock().expect("summaries") =
                        Some(body.matches("COMPACTION SUMMARY").count());
                    text_sse("ok", 3)
                })
            } else {
                ("tools", tool_sse())
            };
            self.kinds.lock().expect("kinds").push(kind);
            // One queued chunk, then the dropped sender ends the stream.
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            sender
                .send(Ok(bytes::Bytes::from(sse)))
                .await
                .expect("queue scripted body");
            Ok(stream)
        })
    }
}

fn text_sse(text: &str, output_tokens: u64) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {"content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": output_tokens}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn tool_sse() -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_read",
                        "function": {
                            "name": "read",
                            "arguments": serde_json::json!({
                                "path": "a.txt"
                            }).to_string()
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

#[tokio::test]
async fn hook_compaction_waits_until_the_turn_commits() {
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
    std::fs::write(
        cwd.join("a.txt"),
        format!("alpha-body\n{}", "a".repeat(20_000)),
    )
    .expect("a");

    let state = CoreState::new(home, Vec::new());
    *state.catalog.write().expect("catalog") =
        Arc::new(super::test_catalog(&[("test-model", 26_000)]));
    let created = state.service.create(None).await.expect("session");
    let mut head = HeadStamp::Empty;
    for text in ["prior work", &"P".repeat(78_000)] {
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

    let scripted = Arc::new(TurnTransport {
        kinds: Mutex::new(Vec::new()),
        final_summaries: Mutex::new(None),
    });
    let transport: Arc<dyn SseTransport> = scripted.clone();
    let provider =
        crate::magpie::provider_row(&settings.magpie, &state.models()).expect("gateway row");
    let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
        .expect("resolve");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let collected = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let done = matches!(
                event,
                crate::BridgeEvent::ChatDone { .. } | crate::BridgeEvent::ChatFailed { .. }
            );
            events.push(event);
            if done {
                break;
            }
        }
        events
    });
    let session_id = created.session_id.as_str().to_owned();
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
    end_turn(
        &state,
        &tx,
        &session_id,
        &created.session_id,
        &created.branch_id,
        &mut control,
        outcome,
    )
    .await
    .expect("turn");
    let kinds = scripted.kinds.lock().expect("kinds").clone();
    assert_eq!(
        kinds,
        ["tools", "summary", "final"],
        "compaction must fire in the hook, between the tool round and the final reply"
    );
    assert_eq!(
        *scripted.final_summaries.lock().expect("summaries"),
        Some(1),
        "the compacted request must carry the summary once"
    );
    drop(tx);
    let events = tokio::time::timeout(std::time::Duration::from_secs(20), collected)
        .await
        .expect("turn events timed out")
        .expect("collector");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, crate::BridgeEvent::ChatDone { .. })),
        "final reply was not committed: {events:?}"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, crate::BridgeEvent::ChatFailed { .. })),
        "turn failed: {events:?}"
    );
    // The view hears the summary start and end, in order, so the turn does
    // not look stuck on the model for the minutes a summary can take.
    let compacting: Vec<bool> = events
        .iter()
        .filter_map(|event| match event {
            crate::BridgeEvent::ContextCompacting { active, .. } => Some(*active),
            _ => None,
        })
        .collect();
    assert_eq!(compacting, [true, false], "{events:?}");

    // Reopen reads the ledger, the same path a restarted session uses.
    let opened = crate::ledger::open_conversation(
        &state.service,
        &SessionId::parse(&session_id).expect("session id"),
    )
    .await
    .expect("reopen");
    let entries = &opened.entries;
    let summary_at = entries
        .iter()
        .position(|entry| crate::compaction::is_display_only_summary(&entry.text))
        .expect("summary card");
    let reply_at = entries
        .iter()
        .position(|entry| entry.kind == EntryKind::AssistantMessage && entry.text.contains("ok"))
        .expect("final reply");
    assert!(
        summary_at > reply_at,
        "summary card landed before the final reply"
    );
    let tool_calls: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.kind == EntryKind::ToolCall)
        .collect();
    assert_eq!(
        tool_calls.len(),
        1,
        "the read card should be on the ledger: {tool_calls:?}"
    );
    assert!(
        tool_calls
            .iter()
            .any(|(_, entry)| entry.text.contains("a.txt")),
        "read card missing: {tool_calls:?}"
    );
    for (index, call) in tool_calls {
        let call_id = call.call_id.as_deref().expect("call id");
        let result_at = entries[index + 1..]
            .iter()
            .position(|entry| {
                entry.kind == EntryKind::ToolResult && entry.call_id.as_deref() == Some(call_id)
            })
            .map(|offset| index + 1 + offset)
            .expect("tool result");
        assert!(
            result_at < summary_at,
            "summary card split the tool call from its result"
        );
        assert!(
            entries[index + 1..result_at]
                .iter()
                .all(|entry| !crate::compaction::is_display_only_summary(&entry.text)),
            "a summary row was written between a tool call and its result"
        );
        let result = &entries[result_at];
        assert!(
            !result.text.starts_with("failed:"),
            "read did not finish: {}",
            result.text
        );
    }
    assert!(
        entries
            .iter()
            .any(|entry| entry.text.contains("alpha-body")),
        "read result missing"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn mid_turn_stash_defers_the_card_and_the_pump_watch_is_compaction() {
    let deferred = DeferredSummaries::default();
    assert!(!deferred.stash(None).await);
    assert!(deferred.stash(Some("goals and files".to_owned())).await);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    watch_prompt_cache(&tx, "ses-1", crate::CacheDisruption::Compaction);
    match rx.try_recv().expect("watch") {
        crate::BridgeEvent::PromptCacheWatch { session_id, cause } => {
            assert_eq!(session_id, "ses-1");
            assert_eq!(cause, crate::CacheDisruption::Compaction);
        }
        other => panic!("expected a cache watch, got {other:?}"),
    }
}

#[tokio::test]
async fn manual_compact_fails_when_the_session_cannot_open() {
    let (root, home) = scratch();
    let state = Arc::new(CoreState::new(home, Vec::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    manual_compact(
        state,
        tx,
        SessionId::generate().expect("session id"),
        BranchId::generate().expect("branch id"),
        HeadStamp::Empty,
        "missing".to_owned(),
        "missing-model".to_owned(),
    )
    .await;
    let event = rx.recv().await.expect("compact event");
    match event {
        crate::BridgeEvent::CompactFinished { message, ok, .. } => {
            assert!(!ok);
            assert!(message.contains("could not open the session"), "{message}");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn manual_compact_rejects_a_moved_branch_head() {
    let (root, home) = scratch();
    let state = Arc::new(CoreState::new(home, Vec::new()));
    let created = state.service.create(None).await.expect("session");
    let (head, _) = crate::ledger::send_message(
        &state.service,
        &created.session_id,
        &created.branch_id,
        &HeadStamp::Empty,
        "first message",
    )
    .await
    .expect("send");
    assert!(!head.is_empty());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // The UI still reports Empty while the ledger moved on to `head`.
    manual_compact(
        state.clone(),
        tx,
        created.session_id.clone(),
        created.branch_id.clone(),
        HeadStamp::Empty,
        "local".to_owned(),
        "test-model".to_owned(),
    )
    .await;
    let event = rx.recv().await.expect("compact event");
    match event {
        crate::BridgeEvent::CompactFinished { message, ok, .. } => {
            assert!(!ok);
            assert_eq!(message, crate::ledger::HEAD_MOVED_ON);
        }
        other => panic!("unexpected event: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Scripted model for a long turn: `rounds` tool rounds, each reading its
/// own large file, then a plain reply. Every summary request gets a summary
/// with its own number, so the test can follow the chain.
struct ChainTransport {
    rounds: usize,
    tool_rounds: Mutex<usize>,
    summaries: Mutex<Vec<String>>,
    finals: Mutex<Vec<String>>,
}

impl ChainTransport {
    fn new(rounds: usize) -> Arc<Self> {
        Arc::new(Self {
            rounds,
            tool_rounds: Mutex::new(0),
            summaries: Mutex::new(Vec::new()),
            finals: Mutex::new(Vec::new()),
        })
    }
}

impl SseTransport for ChainTransport {
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
            let sse = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                let mut summaries = self.summaries.lock().expect("summaries");
                summaries.push(body);
                text_sse(
                    &format!(
                        "chain-mark-{}; {}",
                        summaries.len(),
                        "The covered prefix is replaced by this checkpoint. ".repeat(6)
                    ),
                    80,
                )
            } else {
                let mut done = self.tool_rounds.lock().expect("rounds");
                if *done < self.rounds {
                    *done += 1;
                    read_sse(*done)
                } else {
                    self.finals.lock().expect("finals").push(body);
                    text_sse("all rounds done", 3)
                }
            };
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            sender
                .send(Ok(bytes::Bytes::from(sse)))
                .await
                .expect("queue scripted body");
            Ok(stream)
        })
    }
}

fn read_sse(round: usize) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call_read_{round}"),
                    "function": {
                        "name": "read",
                        "arguments": serde_json::json!({"path": format!("f{round}.txt")}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Answers the first request with a `read` call whose usage reports
/// `reported` prompt tokens, a summary request with a summary, and the
/// request carrying the tool result with a reply. Records each request's
/// kind and the final request's output cap.
struct MeasuredTransport {
    reported: u64,
    kinds: Mutex<Vec<&'static str>>,
    final_cap: Mutex<Option<u64>>,
}

impl SseTransport for MeasuredTransport {
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
            let body = String::from_utf8_lossy(&call.body);
            let (kind, sse) = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                let summary = format!(
                    "handoff notes. {}",
                    "The covered prefix is replaced by this checkpoint. ".repeat(6)
                );
                ("summary", text_sse(&summary, 80))
            } else if body.contains("\"role\":\"tool\"") {
                let value: serde_json::Value = serde_json::from_str(&body).expect("json body");
                *self.final_cap.lock().expect("cap") = value["max_tokens"].as_u64();
                ("final", text_sse("ok", 3))
            } else {
                let chunk = serde_json::json!({
                    "choices": [{
                        "delta": {"tool_calls": [{
                            "index": 0,
                            "id": "call_read",
                            "function": {
                                "name": "read",
                                "arguments": serde_json::json!({"path": "a.txt"}).to_string()
                            }
                        }]},
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": self.reported, "completion_tokens": 20}
                });
                ("tools", format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            };
            self.kinds.lock().expect("kinds").push(kind);
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            sender
                .send(Ok(bytes::Bytes::from(sse)))
                .await
                .expect("queue scripted body");
            Ok(stream)
        })
    }
}

/// One turn in a 100k window whose history estimates near 20k tokens,
/// where the provider counted the first request at `reported`.
async fn measured_turn(reported: u64) -> Arc<MeasuredTransport> {
    let transport = Arc::new(MeasuredTransport {
        reported,
        kinds: Mutex::new(Vec::new()),
        final_cap: Mutex::new(None),
    });
    turn_in_a_100k_window(transport.clone()).await;
    transport
}

/// Runs one turn in a 100k window on a history that estimates near 20k
/// tokens and ends with a prompt to read `a.txt`.
async fn turn_in_a_100k_window(transport: Arc<dyn SseTransport>) {
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
    std::fs::write(cwd.join("a.txt"), "alpha-body").expect("a");
    let state = CoreState::new(home, Vec::new());
    *state.catalog.write().expect("catalog") =
        Arc::new(super::test_catalog(&[("test-model", 100_000)]));
    let created = state.service.create(None).await.expect("session");
    let mut head = HeadStamp::Empty;
    for text in ["prior work", &"P".repeat(40_000), "read a.txt"] {
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
    run_scripted_turn(
        &state,
        &created.session_id,
        &created.branch_id,
        &cwd,
        &settings,
        transport,
    )
    .await
    .expect("turn");
    let _ = std::fs::remove_dir_all(root);
}

/// Refuses the first turn request as over the context window, the way
/// OpenAI-compatible servers do, and answers the rest. Records each
/// request's kind and whether the resent request carried the summary.
#[derive(Default)]
struct OverflowTransport {
    kinds: Mutex<Vec<&'static str>>,
    resent_with_summary: Mutex<Option<bool>>,
}

impl SseTransport for OverflowTransport {
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
            let body = String::from_utf8_lossy(&call.body);
            let refused = !self.kinds.lock().expect("kinds").contains(&"refused");
            let (kind, sse) = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                let summary = format!(
                    "handoff notes. {}",
                    "The covered prefix is replaced by this checkpoint. ".repeat(6)
                );
                ("summary", text_sse(&summary, 80))
            } else if refused {
                self.kinds.lock().expect("kinds").push("refused");
                return Err(ProviderError::with_message(
                    mycode_core::ProviderErrorKind::Rejected,
                    "HTTP 400 Bad Request: {\"error\":{\"message\":\"This model's maximum \
                     context length is 100000 tokens.\",\"code\":\"context_length_exceeded\"}}",
                )
                .with_status(400));
            } else {
                *self.resent_with_summary.lock().expect("resent") =
                    Some(body.contains("handoff notes"));
                ("resent", text_sse("done", 3))
            };
            self.kinds.lock().expect("kinds").push(kind);
            let (sender, stream) = tokio::sync::mpsc::channel(1);
            sender
                .send(Ok(bytes::Bytes::from(sse)))
                .await
                .expect("queue scripted body");
            Ok(stream)
        })
    }
}

/// A request the provider refuses as over its window is compacted and sent
/// once more. The turn used to end on the refusal, and every later turn
/// sent the same oversized history and was refused again.
#[tokio::test]
async fn a_request_refused_as_too_long_is_compacted_and_sent_again() {
    let transport = Arc::new(OverflowTransport::default());
    turn_in_a_100k_window(transport.clone()).await;
    assert_eq!(
        *transport.kinds.lock().expect("kinds"),
        ["refused", "summary", "resent"]
    );
    assert_eq!(
        *transport.resent_with_summary.lock().expect("resent"),
        Some(true)
    );
}

/// The provider's count of the request it answered drives compaction. The
/// estimate alone, about a fifth of it here, never reached the trigger,
/// so a session the provider saw as full went out until it overflowed.
#[tokio::test]
async fn the_provider_count_of_the_last_request_triggers_compaction() {
    let full = measured_turn(90_000).await;
    assert_eq!(
        *full.kinds.lock().expect("kinds"),
        ["tools", "summary", "final"],
        "a 90k count in a 100k window must compact before the next request"
    );
    let near = measured_turn(80_000).await;
    assert_eq!(*near.kinds.lock().expect("kinds"), ["tools", "final"]);
    // Under the trigger, the request goes out whole, but its output cap
    // shrinks to what the window leaves: 80k in plus 32k out passes 100k.
    let cap = near
        .final_cap
        .lock()
        .expect("cap")
        .expect("max_tokens sent");
    assert!(cap < 20_000, "output cap {cap} overflows the window");
}

/// Runs one turn on `transport`, draining its bridge events.
async fn run_scripted_turn(
    state: &CoreState,
    session: &SessionId,
    branch: &BranchId,
    cwd: &std::path::Path,
    settings: &AppSettings,
    transport: Arc<dyn SseTransport>,
) -> Result<(), String> {
    let provider =
        crate::magpie::provider_row(&settings.magpie, &state.models()).expect("gateway row");
    let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
        .expect("resolve");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let session_id = session.as_str().to_owned();
    let mut control = TurnControl::register(state, &session_id);
    let outcome = run_chat_turn_on(
        state,
        &tx,
        &session_id,
        session.clone(),
        branch.clone(),
        "test-model",
        None,
        cwd.to_path_buf(),
        resolved,
        settings.clone(),
        provider,
        transport,
        &control,
    )
    .await;
    let result = end_turn(
        state,
        &tx,
        &session_id,
        session,
        branch,
        &mut control,
        outcome,
    )
    .await;
    drop(tx);
    let _ = drain.await;
    result
}

/// One turn that compacts again and again must carry every earlier summary
/// forward, keep every tool result somewhere the model sees, send only the
/// newest summary, and leave a checkpoint the next turn resumes from.
#[tokio::test]
async fn a_turn_that_compacts_four_times_loses_nothing_and_the_next_turn_resumes() {
    const ROUNDS: usize = 6;
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
    let line = format!("{}\n", "b".repeat(79));
    for round in 1..=ROUNDS {
        let body = format!("BODY-{round}\n{}", line.repeat(600));
        std::fs::write(cwd.join(format!("f{round}.txt")), body).expect("fixture");
    }

    let state = CoreState::new(home.clone(), Vec::new());
    *state.catalog.write().expect("catalog") =
        Arc::new(super::test_catalog(&[("test-model", 30_000)]));
    let created = state.service.create(None).await.expect("session");
    let (session, branch) = (created.session_id.clone(), created.branch_id.clone());
    crate::ledger::send_message(
        &state.service,
        &session,
        &branch,
        &HeadStamp::Empty,
        "read every file in turn",
    )
    .await
    .expect("prompt");

    let chain = ChainTransport::new(ROUNDS);
    run_scripted_turn(&state, &session, &branch, &cwd, &settings, chain.clone())
        .await
        .expect("long turn");
    let summaries = chain.summaries.lock().expect("summaries").clone();
    let finals = chain.finals.lock().expect("finals").clone();
    assert!(
        summaries.len() >= 4,
        "expected four or more compactions in one turn, got {}",
        summaries.len()
    );
    for (index, body) in summaries.iter().enumerate().skip(1) {
        assert!(
            body.contains(&format!("chain-mark-{index};")),
            "summary request {} lost summary {index}",
            index + 1
        );
    }
    let final_body = finals.last().expect("final request");
    assert_eq!(final_body.matches("COMPACTION SUMMARY").count(), 1);
    let newest = format!("chain-mark-{};", summaries.len());
    assert!(
        final_body.contains(&newest),
        "final request lacks the newest summary"
    );
    for round in 1..=ROUNDS {
        let marker = format!("BODY-{round}");
        assert!(
            summaries.iter().any(|body| body.contains(&marker)) || final_body.contains(&marker),
            "{marker} never reached the model after it was read"
        );
    }

    // The newest summary is on disk, aligned with the ledger replay.
    let session_id = session.as_str().to_owned();
    let checkpoint = mycode_config::read_compaction(&home, &session_id)
        .expect("read checkpoint")
        .expect("checkpoint written");
    assert!(
        checkpoint.summary.contains(&newest),
        "the checkpoint kept an older summary"
    );

    // The next turn starts from that summary and does not summarize again.
    let head = crate::ledger::branch_head(&state.service, &session, &branch)
        .await
        .expect("head");
    crate::ledger::send_message(
        &state.service,
        &session,
        &branch,
        &head,
        "NEXT-TURN what now",
    )
    .await
    .expect("second prompt");
    let next = ChainTransport::new(0);
    run_scripted_turn(&state, &session, &branch, &cwd, &settings, next.clone())
        .await
        .expect("next turn");
    assert!(
        next.summaries.lock().expect("summaries").is_empty(),
        "the next turn re-summarized instead of resuming from the checkpoint"
    );
    let next_body = next
        .finals
        .lock()
        .expect("finals")
        .last()
        .cloned()
        .expect("request");
    assert_eq!(next_body.matches("COMPACTION SUMMARY").count(), 1);
    assert!(
        next_body.contains(&newest),
        "the next turn did not start from the newest summary"
    );
    assert!(next_body.find(&newest) < next_body.find("NEXT-TURN what now"));
    // The resumed prefix is the one the long turn last sent, so the
    // provider's prompt cache still covers it.
    let messages = |body: &str| -> Vec<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_str(body).expect("json body");
        value["messages"].as_array().expect("messages").clone()
    };
    let (before, after) = (messages(final_body), messages(&next_body));
    assert!(after.len() > before.len());
    for (index, (old, new)) in before.iter().zip(&after).enumerate() {
        assert_eq!(old, new, "message {index} changed across the turn boundary");
    }
    let _ = std::fs::remove_dir_all(root);
}
