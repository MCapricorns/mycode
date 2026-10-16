//! Compaction around a turn: the request hook's deferred card, and
//! `/compact` against a missing session or a moved head.

use std::sync::{Arc, Mutex};

use mycode_agent::session::{BranchId, EventKind, HeadStamp, SessionId};
use mycode_config::{
    AppSettings, AuthorityRevision, ProviderSecrets, ProviderSettings, replace_app_settings,
    replace_provider_secrets,
};
use mycode_core::ProviderError;
use mycode_providers::{ResolvedProvider, SseTransport, TransportCall};

use super::scratch;
use crate::protocol::EntryKind;
use crate::state::CoreState;
use crate::turn::compact::{DeferredSummaries, watch_prompt_cache};
use crate::turn::{TurnControl, manual_compact, run_chat_turn_on};

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
    let mut settings = AppSettings::default();
    settings.providers.push(ProviderSettings {
        id: "local".to_owned(),
        kind: "openai-completions".to_owned(),
        base_url: "https://example.com/v1".to_owned(),
        models: vec!["test-model".to_owned()],
        enabled: true,
        // ~22k token auto threshold. The seeded prompt stays under it;
        // the `read` result (the whole 20k-char fixture) pushes the next hook over it.
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
    std::fs::write(
        cwd.join("a.txt"),
        format!("alpha-body\n{}", "a".repeat(20_000)),
    )
    .expect("a");

    let catalog = mycode_providers::catalog::current(&home);
    let state = CoreState::new(home, catalog, Vec::new());
    let created = state.service.create().await.expect("session");
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
    let provider = settings.providers[0].clone();
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
    let control = TurnControl::register(&state, &session_id);
    run_chat_turn_on(
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
    let catalog = mycode_providers::catalog::current(&home);
    let state = Arc::new(CoreState::new(home, catalog, Vec::new()));
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
    let catalog = mycode_providers::catalog::current(&home);
    let state = Arc::new(CoreState::new(home, catalog, Vec::new()));
    let created = state.service.create().await.expect("session");
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
