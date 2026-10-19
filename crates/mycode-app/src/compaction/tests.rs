use std::sync::Arc;

use mycode_core::{
    AssistantMessage, ContentBlock, Message, StopReason, TextBlock, ThinkingBlock, ToolCall,
    ToolResultMessage, UserMessage,
};

use super::{
    CompactStatus, Compacted, compaction_transcript, display_summary_text, is_display_only_summary,
    transcript_head,
};

/// Long enough for the short-summary gate. The opening phrase stays so
/// older assertions can still find it.
const PLAUSIBLE_SUMMARY: &str = concat!(
    "handoff notes. User goals and constraints: continue the coding task from the kept tail. ",
    "Decisions made, and why: the covered prefix is replaced by this checkpoint. ",
    "Files and paths touched: the files named in the conversation were read. ",
    "Commands run and their outcomes: none failed in the covered prefix. ",
    "Current work, open tasks, and unresolved errors: the tail is still unanswered. ",
    "Clear next steps: reply to the latest user message."
);
const FULL_HANDOFF: &str = concat!(
    "full handoff. User goals and constraints: continue the coding task from the kept tail. ",
    "Decisions made, and why: the covered prefix is replaced by this checkpoint. ",
    "Files and paths touched: the files named in the conversation were read. ",
    "Commands run and their outcomes: none failed in the covered prefix. ",
    "Current work, open tasks, and unresolved errors: the tail is still unanswered. ",
    "Clear next steps: reply to the latest user message."
);

fn user(text: &str) -> Arc<Message> {
    Arc::new(Message::User(UserMessage::text(text)))
}

#[test]
fn a_later_compact_summarizes_only_the_uncovered_head() {
    let history = vec![
        user("old-a"),
        user("old-b"),
        user("old-c"),
        user("new-d"),
        user("new-e"),
        user("kept-tail"),
    ];
    let head = transcript_head(&history, 5, Some(3));
    let transcript = compaction_transcript(Some("prior summary"), head);
    assert!(transcript.contains("prior summary"), "{transcript}");
    assert!(transcript.contains("new-d"), "{transcript}");
    assert!(transcript.contains("new-e"), "{transcript}");
    assert!(!transcript.contains("old-a"), "{transcript}");
    assert!(!transcript.contains("old-b"), "{transcript}");
    assert!(!transcript.contains("old-c"), "{transcript}");
    assert!(!transcript.contains("kept-tail"), "{transcript}");
}

#[test]
fn an_oversized_head_keeps_the_prior_checkpoint() {
    let filler = "x".repeat(3_900);
    let mut history: Vec<_> = (0..120)
        .map(|index| user(&format!("m{index} {filler}")))
        .collect();
    history.push(user("newest"));
    let transcript = compaction_transcript(Some("prior summary"), &history);
    assert!(
        transcript.starts_with("Previous checkpoint:\nprior summary"),
        "{}",
        &transcript[..200]
    );
    assert!(transcript.contains("earlier content elided"));
    assert!(transcript.contains("newest"));
    assert!(!transcript.contains("m0 "));
}

#[test]
fn the_first_compact_still_includes_the_whole_head() {
    let history = vec![user("one"), user("two"), user("tail")];
    let head = transcript_head(&history, 2, None);
    assert_eq!(head.len(), 2);
    let transcript = compaction_transcript(None, head);
    assert!(transcript.contains("one"), "{transcript}");
    assert!(transcript.contains("two"), "{transcript}");
    assert!(!transcript.contains("tail"), "{transcript}");
}

#[test]
fn manual_and_auto_compaction_post_the_same_summary_message() {
    let body = "files touched: src/main.rs\nnext: run the tests";
    let manual = Compacted {
        messages: Vec::new(),
        status: CompactStatus::Wrote,
        summary: Some(body.to_owned()),
    };
    let auto = Compacted {
        messages: Vec::new(),
        status: CompactStatus::Wrote,
        summary: Some(body.to_owned()),
    };
    let manual_text = display_summary_text(manual.summary.as_deref().expect("manual"));
    let auto_text = display_summary_text(auto.summary.as_deref().expect("auto"));
    assert_eq!(manual_text, auto_text);
    assert!(is_display_only_summary(&manual_text));
    assert!(manual_text.contains(body));
    assert!(super::summary_body(&manual_text).contains("src/main.rs"));
}

#[test]
fn the_wire_checkpoint_owns_the_work_it_describes() {
    // A resumed model that reads the checkpoint as another agent's notes
    // re-verifies finished work from scratch. The wire frame must claim the
    // summary as the model's own earlier work.
    let wire_text = super::request_summary_text("files: src/main.rs");
    assert!(super::is_summary_message(&user(&wire_text)));
    assert!(wire_text.contains("COMPACTION SUMMARY"));
    assert!(wire_text.contains("your own earlier work"));
    assert!(wire_text.contains("src/main.rs"));
    // The ledger card keeps the bare display text: it is user-facing.
    assert!(!display_summary_text("files: src/main.rs").contains("your own"));
}

fn user_text(message: &Message) -> String {
    match message {
        Message::User(user) => user
            .content
            .iter()
            .filter_map(|block| match block {
                mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

fn padded(marker: &str, chars: usize) -> String {
    let mut text = marker.to_owned();
    let extra = chars.saturating_sub(marker.chars().count());
    text.push_str(&"x".repeat(extra));
    text
}

struct SummaryTransport {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Request bodies, kept when a test needs to read the transcript.
    bodies: Option<std::sync::Arc<parking_lot::Mutex<Vec<String>>>>,
}

impl mycode_providers::SseTransport for SummaryTransport {
    fn post<'a>(
        &'a self,
        call: mycode_providers::TransportCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        mycode_providers::transport::ByteStream,
                        mycode_core::ProviderError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = String::from_utf8_lossy(&call.body);
            if let Some(bodies) = &self.bodies {
                bodies.lock().push(body.clone().into_owned());
            }
            let text = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                PLAUSIBLE_SUMMARY
            } else {
                "unexpected summary request"
            };
            let chunk = serde_json::json!({
                "choices": [{
                    "delta": {"content": text},
                    "finish_reason": "stop"
                }]
            });
            let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
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

fn scratch_home() -> (std::path::PathBuf, mycode_config::HomeLayout) {
    // Parallel tests start within one clock tick (100ns on Windows), so the
    // timestamp alone handed two tests the same home.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "mycode-compact-{}-{}-{}",
        std::process::id(),
        crate::unix_now().as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("scratch");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    // A checkpoint lives in its session's directory, which session creation
    // makes; these tests name sessions without creating them.
    for session in [
        "parent-session",
        "session-a",
        "session-b",
        "session-c",
        "session-cut",
        "session-empty",
        "session-glm",
        "session-short",
    ] {
        let dir = mycode_config::new_session_dir(None, session).expect("session dir");
        std::fs::create_dir_all(root.join(dir)).expect("session dir");
    }
    let home = mycode_config::HomeLayout::from_root(&root).expect("home");
    (root, home)
}

fn summary_wire(
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> mycode_providers::WireProvider {
    summary_wire_with(calls, None)
}

fn summary_wire_with(
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bodies: Option<std::sync::Arc<parking_lot::Mutex<Vec<String>>>>,
) -> mycode_providers::WireProvider {
    let settings = mycode_config::ProviderSettings {
        id: "local".to_owned(),
        kind: "openai-completions".to_owned(),
        base_url: "https://example.com/v1".to_owned(),
        models: vec!["model-a".to_owned()],
        enabled: true,
        context_limit: None,
        max_output: None,
        hidden_models: Vec::new(),
    };
    let resolved =
        mycode_providers::ResolvedProvider::resolve(&settings, "model-a", "test-key", "test-agent")
            .expect("provider");
    mycode_providers::WireProvider::new(
        resolved,
        std::sync::Arc::new(SummaryTransport { calls, bodies }),
    )
}

/// A turn that already compacted once sends the hook a history that opens
/// with a summary. The disk checkpoint's `covered_messages` indexes the
/// ledger, not this stitched vector, and the leading summary may exist only
/// in memory. Neither may drop work from the next summary.
#[tokio::test]
async fn a_second_compact_in_one_turn_keeps_every_uncovered_message() {
    let (root, home) = scratch_home();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bodies = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let wire = summary_wire_with(calls.clone(), Some(bodies.clone()));
    mycode_config::write_compaction(
        &home,
        "session-c",
        &mycode_config::CompactionCheckpoint {
            format_version: mycode_config::COMPACTION_FORMAT_VERSION,
            kind: mycode_config::COMPACTION_KIND.to_owned(),
            session_id: "session-c".to_owned(),
            branch_id: "branch-c".to_owned(),
            covered_head: "head-1".to_owned(),
            covered_messages: 3,
            summary: "DISK-SUMMARY of the first three ledger messages".to_owned(),
            model: "model-a".to_owned(),
            created_at_unix: 1,
        },
    )
    .expect("seed checkpoint");
    let history = vec![
        user(&super::request_summary_text(
            "MEM-SUMMARY of the disk checkpoint and the first hook pass",
        )),
        user(&padded("UNCOVERED-ONE ", 39_000)),
        user(&padded("UNCOVERED-TWO ", 39_000)),
        user(&padded("TAIL-A ", 39_000)),
        user(&padded("TAIL-B ", 39_000)),
    ];
    let scope = super::CompactScope {
        wire: &wire,
        model: "model-a",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-c",
            branch_id: "branch-c",
            head: "head-1",
        }),
        context_window: 30_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, history, false).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let sent = bodies.lock().concat();
    for marker in ["MEM-SUMMARY", "UNCOVERED-ONE", "UNCOVERED-TWO"] {
        assert!(
            sent.contains(marker),
            "{marker} missing from the summary request"
        );
    }
    assert!(!sent.contains("TAIL-A"), "the kept tail must stay verbatim");
    assert!(super::is_summary_message(&compacted.messages[0]));
    assert!(user_text(&compacted.messages[0]).contains("handoff notes"));
    let rest: Vec<String> = compacted.messages[1..]
        .iter()
        .map(|message| user_text(message))
        .collect();
    assert_eq!(rest.len(), 2, "{rest:?}");
    assert!(rest[0].starts_with("TAIL-A") && rest[1].starts_with("TAIL-B"));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn manual_compact_is_the_next_request_while_under_the_auto_threshold() {
    let (root, home) = scratch_home();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = summary_wire(calls.clone());
    let head = padded("HEAD-MARKER-", 100_000);
    let history = vec![user(&head), user("TAIL-ONE"), user("TAIL-TWO")];
    let scope = super::CompactScope {
        wire: &wire,
        model: "model-a",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-a",
            branch_id: "branch-a",
            head: "head-1",
        }),
        // Large window: this history is over the ~20k tail, under the
        // auto threshold, which is the case that used to skip the checkpoint.
        context_window: 200_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let manual = super::compact_history(&scope, history.clone(), true).await;
    assert_eq!(manual.status, CompactStatus::Wrote);
    assert_eq!(manual.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(user_text(&manual.messages[0]).contains("handoff notes"));
    assert!(user_text(&manual.messages[0]).contains("COMPACTION SUMMARY"));
    assert_eq!(user_text(&manual.messages[1]), "TAIL-ONE");
    assert_eq!(user_text(&manual.messages[2]), "TAIL-TWO");
    assert!(
        manual
            .messages
            .iter()
            .all(|message| !user_text(message).contains("HEAD-MARKER-"))
    );

    let mut later = history;
    later.push(user("NEW-TURN"));
    let next_scope = super::CompactScope {
        checkpoint: scope.checkpoint.map(|slot| super::CheckpointSlot {
            head: "head-2",
            ..slot
        }),
        ..scope
    };
    let next = super::compact_history(&next_scope, later, false).await;
    assert_eq!(next.status, CompactStatus::Unchanged);
    assert!(next.summary.is_none());
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an under-threshold replay must reuse the checkpoint"
    );
    assert_eq!(
        next.messages
            .iter()
            .filter(|message| super::is_summary_message(message))
            .count(),
        1
    );
    assert!(user_text(&next.messages[0]).contains("handoff notes"));
    assert_eq!(user_text(&next.messages[1]), "TAIL-ONE");
    assert_eq!(user_text(&next.messages[2]), "TAIL-TWO");
    assert_eq!(user_text(&next.messages[3]), "NEW-TURN");
    assert!(
        next.messages
            .iter()
            .all(|message| !user_text(message).contains("HEAD-MARKER-"))
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn manual_compact_summarizes_a_session_that_still_fits() {
    let (root, home) = scratch_home();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wire = summary_wire(calls.clone());
    let history = vec![user("hi"), user("there")];
    let scope = super::CompactScope {
        wire: &wire,
        model: "model-a",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-b",
            branch_id: "branch-b",
            head: "head-1",
        }),
        context_window: 200_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    // `/compact` no longer answers "nothing to compact" for a short
    // session: the whole ledger is summarized and the tail is empty.
    let manual = super::compact_history(&scope, history, true).await;
    assert_eq!(manual.status, CompactStatus::Wrote);
    assert_eq!(manual.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(manual.messages.len(), 1);
    assert!(user_text(&manual.messages[0]).contains("COMPACTION SUMMARY"));
    let checkpoint = mycode_config::read_compaction(&home, "session-b")
        .expect("read")
        .expect("written");
    assert_eq!(checkpoint.covered_messages, 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_model_that_cannot_disable_thinking_gets_a_larger_summary_cap() {
    assert_eq!(super::summary_max_output("glm-5.3"), 8_192);
    assert_eq!(super::summary_max_output("kimi-k2.7-code"), 32_768);
    assert_eq!(
        super::summary_max_output("k3-256k"),
        32_768,
        "K3 has no thinking off switch"
    );
}

#[test]
fn a_long_message_keeps_its_head_and_tail() {
    let mut text = "HEAD-".to_owned();
    text.push_str(&"m".repeat(5_000));
    text.push_str("-TAIL");
    let kept = super::excerpt(&text);
    assert!(kept.starts_with("HEAD-"), "{kept}");
    assert!(kept.ends_with("-TAIL"), "{kept}");
    let omitted = text.chars().count() - super::EXCERPT_CHARS;
    assert!(
        kept.contains(&format!("{omitted} characters omitted")),
        "{kept}"
    );
    let marker = format!("\n...{omitted} characters omitted...\n");
    let (head, tail) = kept.split_once(&marker).expect("marker");
    assert_eq!(
        head.chars().count() + tail.chars().count(),
        super::EXCERPT_CHARS
    );
}

struct ScriptedSummary {
    bodies: std::sync::Mutex<Vec<String>>,
    /// `(finish_reason, content)` for each summary call, in order.
    script: Vec<(&'static str, &'static str)>,
    index: std::sync::atomic::AtomicUsize,
    /// `openai` or `anthropic`. The request body is what the test checks;
    /// the scripted stream only has to complete in that protocol.
    protocol: &'static str,
}

impl mycode_providers::SseTransport for ScriptedSummary {
    fn post<'a>(
        &'a self,
        call: mycode_providers::TransportCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        mycode_providers::transport::ByteStream,
                        mycode_core::ProviderError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.bodies
                .lock()
                .expect("bodies")
                .push(String::from_utf8_lossy(&call.body).into_owned());
            let index = self.index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (finish, content) = self
                .script
                .get(index)
                .copied()
                .unwrap_or(("stop", PLAUSIBLE_SUMMARY));
            let sse = if self.protocol == "anthropic" {
                let stop = if finish == "length" {
                    "max_tokens"
                } else {
                    "end_turn"
                };
                let text = serde_json::to_string(content).unwrap_or_else(|_| "\"\"".to_owned());
                format!(
                    "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
                     data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{text}}}}}\n\n\
                     data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
                     data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop}\"}}}}\n\n\
                     data: {{\"type\":\"message_stop\"}}\n\n"
                )
            } else {
                let chunk = serde_json::json!({
                    "choices": [{
                        "delta": {"content": content},
                        "finish_reason": finish
                    }]
                });
                format!("data: {chunk}\n\ndata: [DONE]\n\n")
            };
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

fn scripted_wire(
    kind: &str,
    base_url: &str,
    model: &str,
    script: Vec<(&'static str, &'static str)>,
) -> (
    mycode_providers::WireProvider,
    std::sync::Arc<ScriptedSummary>,
) {
    let protocol = if kind == "anthropic-messages" {
        "anthropic"
    } else {
        "openai"
    };
    let transport = std::sync::Arc::new(ScriptedSummary {
        bodies: std::sync::Mutex::new(Vec::new()),
        script,
        index: std::sync::atomic::AtomicUsize::new(0),
        protocol,
    });
    let settings = mycode_config::ProviderSettings {
        id: "local".to_owned(),
        kind: kind.to_owned(),
        base_url: base_url.to_owned(),
        models: vec![model.to_owned()],
        enabled: true,
        context_limit: None,
        max_output: None,
        hidden_models: Vec::new(),
    };
    let resolved =
        mycode_providers::ResolvedProvider::resolve(&settings, model, "test-key", "test-agent")
            .expect("provider");
    let wire = mycode_providers::WireProvider::new(resolved, transport.clone());
    (wire, transport)
}

fn large_history() -> Vec<Arc<Message>> {
    vec![
        user(&padded("HEAD-MARKER-", 100_000)),
        user("TAIL-ONE"),
        user("TAIL-TWO"),
    ]
}

#[tokio::test]
async fn summary_request_disables_glm_thinking_and_retries_a_truncated_summary() {
    let (root, home) = scratch_home();
    let (wire, transport) = scripted_wire(
        "openai-completions",
        "https://api.z.ai/api/paas/v4",
        "glm-5.3",
        vec![("length", "partial"), ("stop", FULL_HANDOFF)],
    );
    let scope = super::CompactScope {
        wire: &wire,
        model: "glm-5.3",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-glm",
            branch_id: "branch-glm",
            head: "head-1",
        }),
        context_window: 200_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, large_history(), true).await;
    assert_eq!(compacted.status, CompactStatus::Wrote);
    assert_eq!(compacted.summary.as_deref(), Some(FULL_HANDOFF));
    let bodies = transport.bodies.lock().expect("bodies");
    assert_eq!(bodies.len(), 2, "a length finish must retry once");
    let first: serde_json::Value = serde_json::from_str(&bodies[0]).expect("json");
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).expect("json");
    assert_eq!(first["thinking"]["type"], "disabled");
    assert!(first.get("reasoning_effort").is_none());
    assert_eq!(first["max_tokens"], 8_192);
    assert_eq!(second["max_tokens"], 65_536);
    assert_eq!(second["thinking"]["type"], "disabled");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_still_truncated_summary_is_not_stored() {
    let (root, home) = scratch_home();
    let (wire, transport) = scripted_wire(
        "openai-completions",
        "https://api.z.ai/api/paas/v4",
        "glm-5.3",
        vec![("length", "partial"), ("length", "still partial")],
    );
    let scope = super::CompactScope {
        wire: &wire,
        model: "glm-5.3",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-cut",
            branch_id: "branch-cut",
            head: "head-1",
        }),
        context_window: 200_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, large_history(), true).await;
    match compacted.status {
        CompactStatus::Failed(message) => assert!(message.contains("truncated"), "{message}"),
        other => panic!("truncated summary was accepted: {other:?}"),
    }
    assert!(compacted.summary.is_none());
    assert_eq!(transport.index.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        mycode_config::read_compaction(&home, "session-cut")
            .expect("read")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_empty_summary_is_an_error() {
    let (root, home) = scratch_home();
    let (wire, transport) = scripted_wire(
        "anthropic-messages",
        "https://api.z.ai",
        "glm-5.3",
        vec![("end_turn", "")],
    );
    let scope = super::CompactScope {
        wire: &wire,
        model: "glm-5.3",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-empty",
            branch_id: "branch-empty",
            head: "head-1",
        }),
        context_window: 200_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, large_history(), true).await;
    match compacted.status {
        CompactStatus::Failed(message) => assert!(message.contains("empty"), "{message}"),
        other => panic!("empty summary was accepted: {other:?}"),
    }
    let bodies = transport.bodies.lock().expect("bodies");
    let body: serde_json::Value = serde_json::from_str(&bodies[0]).expect("json");
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(body["max_tokens"], 8_192);
    assert!(
        mycode_config::read_compaction(&home, "session-empty")
            .expect("read")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(root);
}

fn assistant_message(blocks: Vec<ContentBlock>) -> Arc<Message> {
    Arc::new(Message::Assistant(AssistantMessage {
        blocks,
        usage: None,
        stop_reason: StopReason::Stop,
    }))
}

fn tool_result_message(id: &str, body: &str) -> Arc<Message> {
    Arc::new(Message::ToolResult(ToolResultMessage {
        tool_call_id: id.to_owned(),
        content: vec![ContentBlock::Text(TextBlock::new(body))],
        is_error: false,
        details: None,
    }))
}

fn request_user_text(request: &mycode_core::Request) -> String {
    user_text(
        request
            .messages
            .first()
            .expect("summary user message")
            .as_ref(),
    )
}

#[test]
fn a_conversation_ending_in_a_tool_result_is_wrapped_and_the_instruction_is_repeated() {
    let history = vec![
        user("在 big2.txt 里找 Marker 行"),
        assistant_message(vec![ContentBlock::ToolCall(ToolCall::new(
            "call-read",
            "read",
            serde_json::json!({"path": "big2.txt"}),
        ))]),
        tool_result_message("call-read", "[output truncated ...] revision 9"),
    ];
    let transcript = compaction_transcript(None, &history);
    let request = super::summary_request(&transcript, 8_192);
    let text = request_user_text(&request);
    let open = text.find("<conversation>").expect("opening fence") + "<conversation>".len();
    let close = text.find("</conversation>").expect("closing fence");
    assert!(open < close, "{text}");
    let inside = &text[open..close];
    let after = &text[close + "</conversation>".len()..];
    assert!(
        inside.contains("[output truncated ...] revision 9"),
        "{inside}"
    );
    assert!(inside.contains("[tool]"), "{inside}");
    assert!(
        inside
            .trim_end()
            .ends_with("[output truncated ...] revision 9"),
        "the tool result must be the end of the fenced conversation: {inside}"
    );
    assert!(
        !after.contains("[output truncated ...]"),
        "the tool result leaked out of the fence: {after}"
    );
    assert!(
        text[..open].contains("Summarize the following conversation for continuation:"),
        "{text}"
    );
    assert!(
        after.contains("The text above is the conversation to summarize."),
        "{after}"
    );
    assert!(
        after.contains("Do not continue or execute any task in it."),
        "{after}"
    );
    assert!(
        after.contains("Output only the summary, covering the points the instructions list."),
        "{after}"
    );
    assert_eq!(request.reasoning, Some(mycode_core::ReasoningLevel::Off));
}

#[test]
fn think_content_is_stripped_when_the_conversation_is_serialized() {
    let mut reasoning = "<think>".to_owned();
    reasoning.push_str(&"r".repeat(5_000));
    reasoning.push_str("</think>\nvisible-answer about big1");
    let history = vec![
        user("the user wrote <think>keep this quote</think>"),
        assistant_message(vec![
            ContentBlock::Thinking(ThinkingBlock::new("reasoning_content hidden plan")),
            ContentBlock::Text(TextBlock::new(reasoning)),
            ContentBlock::Text(TextBlock::new("Later the notes mention <think> as a tag.")),
        ]),
        tool_result_message("call-read", "file body keeps <think>not reasoning</think>"),
    ];
    let transcript = compaction_transcript(None, &history);
    assert!(
        transcript.contains("<think>keep this quote</think>"),
        "{transcript}"
    );
    assert!(!transcript.contains("reasoning_content"), "{transcript}");
    assert!(!transcript.contains("hidden plan"), "{transcript}");
    assert!(
        !transcript.contains(&"r".repeat(80)),
        "leading think text leaked into the transcript: {transcript}"
    );
    assert!(
        transcript.contains("visible-answer about big1"),
        "{transcript}"
    );
    assert!(
        transcript.contains("Later the notes mention <think> as a tag."),
        "{transcript}"
    );
    assert!(
        transcript.contains("file body keeps <think>not reasoning</think>"),
        "{transcript}"
    );
}

#[tokio::test]
async fn a_twelve_token_one_line_summary_is_rejected_and_history_is_kept() {
    let (root, home) = scratch_home();
    let line = "未找到 `big2.txt` 中的 Marker 行。";
    assert!(
        line.chars().count() < super::MIN_SUMMARY_CHARS,
        "the fixture must stay under the short-summary gate"
    );
    let (wire, transport) = scripted_wire(
        "openai-completions",
        "https://api.minimax.chat/v1",
        "MiniMax-M3",
        vec![("stop", line), ("stop", line)],
    );
    let history = large_history();
    let scope = super::CompactScope {
        wire: &wire,
        model: "MiniMax-M3",
        checkpoint: Some(super::CheckpointSlot {
            home: &home,
            session_id: "session-short",
            branch_id: "branch-short",
            head: "head-1",
        }),
        context_window: 200_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, history.clone(), true).await;
    match compacted.status {
        CompactStatus::Failed(message) => {
            assert!(message.contains("too short"), "{message}");
        }
        other => panic!("short summary was accepted: {other:?}"),
    }
    assert!(compacted.summary.is_none());
    assert_eq!(compacted.messages.len(), history.len());
    for (left, right) in compacted.messages.iter().zip(&history) {
        assert!(Arc::ptr_eq(left, right));
    }
    assert!(
        compacted
            .messages
            .iter()
            .any(|message| user_text(message).contains("HEAD-MARKER-"))
    );
    assert!(
        compacted
            .messages
            .iter()
            .all(|message| !user_text(message).starts_with("COMPACTION SUMMARY"))
    );
    assert_eq!(transport.index.load(std::sync::atomic::Ordering::SeqCst), 2);
    let bodies = transport.bodies.lock().expect("bodies");
    assert_eq!(bodies.len(), 2, "a short summary must retry once");
    for body in bodies.iter() {
        let parsed: serde_json::Value = serde_json::from_str(body).expect("json");
        let content = parsed["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .and_then(|message| message["content"].as_str())
            .expect("user content");
        assert!(content.contains("<conversation>"), "{content}");
        assert!(content.contains("</conversation>"), "{content}");
        assert!(
            content.contains("Do not continue or execute any task in it."),
            "{content}"
        );
    }
    assert!(
        mycode_config::read_compaction(&home, "session-short")
            .expect("read")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn system_prompt_and_tools_count_toward_the_auto_trigger() {
    let history = vec![
        user(&"a".repeat(8_000)),
        user(&"b".repeat(8_000)),
        user(&"c".repeat(8_000)),
        user(&"d".repeat(8_000)),
    ];
    let threshold = 9_000;
    assert!(
        super::compaction_split(&history, threshold, 4_000, 0).is_none(),
        "history alone is under the trigger"
    );
    assert!(
        super::compaction_split(&history, threshold, 4_000, 2_000).is_some(),
        "the same history plus the prompt and tools is over the trigger"
    );
    let tools = [mycode_core::ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        params_schema: serde_json::json!({"type": "object"}),
    }];
    let tokens = super::estimate_prefix_tokens(&["You are MYCode".to_owned()], &tools);
    assert!(tokens > 0, "{tokens}");
}

/// A subagent runs this same compaction with no checkpoint slot: its own
/// summary chain only, never its parent session's checkpoint.
#[tokio::test]
async fn a_history_without_a_checkpoint_slot_compacts_in_memory_only() {
    let (root, home) = scratch_home();
    mycode_config::write_compaction(
        &home,
        "parent-session",
        &mycode_config::CompactionCheckpoint {
            format_version: mycode_config::COMPACTION_FORMAT_VERSION,
            kind: mycode_config::COMPACTION_KIND.to_owned(),
            session_id: "parent-session".to_owned(),
            branch_id: "branch-p".to_owned(),
            covered_head: "head-p".to_owned(),
            covered_messages: 1,
            summary: "PARENT-SUMMARY must stay out of the child".to_owned(),
            model: "model-a".to_owned(),
            created_at_unix: 1,
        },
    )
    .expect("parent checkpoint");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bodies = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let wire = summary_wire_with(calls.clone(), Some(bodies.clone()));
    let history = vec![
        user(&padded("CHILD-HEAD ", 39_000)),
        user(&padded("CHILD-MID ", 39_000)),
        user(&padded("CHILD-TAIL-A ", 39_000)),
        user(&padded("CHILD-TAIL-B ", 39_000)),
    ];
    let scope = super::CompactScope {
        wire: &wire,
        model: "model-a",
        checkpoint: None,
        context_window: 30_000,
        overhead_tokens: 0,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, history, false).await;
    assert_eq!(compacted.status, CompactStatus::Replaced);
    assert_eq!(compacted.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
    assert!(super::is_summary_message(&compacted.messages[0]));
    let sent = bodies.lock().concat();
    assert!(sent.contains("CHILD-HEAD") && sent.contains("CHILD-MID"));
    assert!(
        !sent.contains("PARENT-SUMMARY"),
        "the parent checkpoint leaked in"
    );
    let parent = mycode_config::read_compaction(&home, "parent-session")
        .expect("read")
        .expect("still there");
    assert!(
        parent.summary.starts_with("PARENT-SUMMARY"),
        "the parent file changed"
    );
    let _ = std::fs::remove_dir_all(root);
}
