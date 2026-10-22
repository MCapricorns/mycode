//! Tests for ledger pairing: unanswered calls, missing branches, cancel and

use super::*;

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
    let step = mycode_agent::AssistantMessage {
        blocks: vec![mycode_agent::ContentBlock::ToolCall(
            mycode_agent::ToolCall::new("call_lost", "read", serde_json::json!({"path": "a.txt"})),
        )],
        usage: None,
        stop_reason: mycode_agent::StopReason::ToolUse,
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
    let mut control = TurnControl::register(&fixture.state, fixture.session.as_str());
    let outcome = run_chat_turn_on(
        &fixture.state,
        &tx,
        fixture.session.as_str(),
        fixture.session.clone(),
        wrong_branch.clone(),
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
        &wrong_branch,
        &mut control,
        outcome,
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
    assert_eq!(control.retire(), vec!["one more thing".to_owned()]);
    assert!(
        !fixture.state.steer_inboxes.lock().contains_key(&session_id),
        "a retired turn must not accept steers it can no longer send"
    );
    assert!(
        !fixture.state.turn_cancels.lock().contains_key(&session_id),
        "a retired turn must not read as running"
    );
    assert!(control.retire().is_empty());
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
                    Output = Result<mycode_agent::transport::ByteStream, ProviderError>,
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
