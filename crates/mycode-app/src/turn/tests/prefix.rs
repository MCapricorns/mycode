//! Prompt-cache prefix across turns: the next turn's request must open with
//! the exact messages the previous turn last sent, so the provider's prefix
//! cache keeps hitting after a ledger replay.

use std::sync::{Arc, Mutex};

use mycode_agent::session::HeadStamp;
use mycode_core::ProviderError;
use mycode_providers::{SseTransport, TransportCall};

use super::{parse_head, run_scripted_turn, turn_fixture};

/// Request 1: thinking, text, and two parallel reads. Request 2: the
/// closing reply. Every later request: a plain reply.
struct ReplayTransport {
    bodies: Mutex<Vec<serde_json::Value>>,
}

fn two_reads_sse() -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "reasoning_content": "plan both reads",
                "content": "reading both files",
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_slow",
                        "type": "function",
                        "function": {"name": "read", "arguments": "{\"path\":\"slow.txt\"}"}
                    },
                    {
                        "index": 1,
                        "id": "call_fast",
                        "type": "function",
                        "function": {"name": "read", "arguments": "{\"path\":\"fast.txt\"}"}
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 3}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

impl SseTransport for ReplayTransport {
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
            let body: serde_json::Value = serde_json::from_slice(&call.body).expect("json body");
            let count = {
                let mut bodies = self.bodies.lock().expect("bodies");
                bodies.push(body);
                bodies.len()
            };
            let sse = match count {
                1 => two_reads_sse(),
                2 => super::reply_sse(super::REPLY_A),
                _ => super::reply_sse("REPLY_B ok"),
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

#[tokio::test]
async fn the_next_turn_resends_the_previous_request_byte_for_byte() {
    let fixture = turn_fixture().await;
    // The first read is larger, so the two parallel calls tend to finish
    // out of call order; the ledger commits them in completion order.
    std::fs::write(fixture.cwd.join("slow.txt"), "slow\n".repeat(20_000)).expect("slow");
    std::fs::write(fixture.cwd.join("fast.txt"), "fast\n").expect("fast");
    let transport = Arc::new(ReplayTransport {
        bodies: Mutex::new(Vec::new()),
    });
    crate::ledger::send_message(
        &fixture.state.service,
        &fixture.session,
        &fixture.branch,
        &HeadStamp::Empty,
        super::PROMPT_A,
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
        super::PROMPT_B,
    )
    .await
    .expect("prompt B");
    let (outcome, _) = run_scripted_turn(&fixture, transport.clone()).await;
    outcome.expect("turn B");

    let bodies = transport.bodies.lock().expect("bodies").clone();
    assert_eq!(bodies.len(), 3, "two requests for A, one for B");
    let (last_a, first_b) = (&bodies[1], &bodies[2]);
    assert_eq!(last_a["tools"], first_b["tools"], "tool specs changed");
    let before = last_a["messages"].as_array().expect("A messages");
    let after = first_b["messages"].as_array().expect("B messages");
    assert!(after.len() > before.len(), "B must extend A's history");
    for (index, (old, new)) in before.iter().zip(after).enumerate() {
        assert_eq!(
            old, new,
            "message {index} differs between A's last request and B's replay"
        );
    }
    let reply = &after[before.len()];
    assert_eq!(reply["role"], "assistant");
    assert!(
        reply.to_string().contains(super::REPLY_A),
        "A's reply must follow A's request: {reply}"
    );
    let _ = std::fs::remove_dir_all(&fixture.root);
}
