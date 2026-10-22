use std::sync::Arc;

use mycode_core::{
    AssistantMessage, ContentBlock, Message, StopReason, TextBlock, ThinkingBlock, ToolCall,
    ToolResultMessage, UserMessage,
};

use super::{
    CompactStatus, Compacted, MAX_TRANSCRIPT_TOKENS, TokenScale, compaction_transcript,
    display_summary_text, is_display_only_summary, transcript_head,
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

mod checkpoints;
mod echo;

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
    let transcript = compaction_transcript(
        Some("prior summary"),
        head,
        MAX_TRANSCRIPT_TOKENS,
        TokenScale::ONE,
    );
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
    let transcript = compaction_transcript(
        Some("prior summary"),
        &history,
        MAX_TRANSCRIPT_TOKENS,
        TokenScale::ONE,
    );
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
    let transcript = compaction_transcript(None, head, MAX_TRANSCRIPT_TOKENS, TokenScale::ONE);
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
        tokens: 0,
    };
    let auto = Compacted {
        messages: Vec::new(),
        status: CompactStatus::Wrote,
        summary: Some(body.to_owned()),
        tokens: 0,
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
        measure: super::Measure::default(),
        on_summarize: None,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let compacted = super::compact_history(&scope, history, super::CompactMode::Auto).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let sent = bodies.lock().concat();
    // A 30k window keeps a ~10k tail, one of these messages: a 20k tail
    // left the request over the trigger right after compacting.
    for marker in ["MEM-SUMMARY", "UNCOVERED-ONE", "UNCOVERED-TWO", "TAIL-A"] {
        assert!(
            sent.contains(marker),
            "{marker} missing from the summary request"
        );
    }
    assert!(!sent.contains("TAIL-B"), "the kept tail must stay verbatim");
    assert!(super::is_summary_message(&compacted.messages[0]));
    assert!(user_text(&compacted.messages[0]).contains("handoff notes"));
    let rest: Vec<String> = compacted.messages[1..]
        .iter()
        .map(|message| user_text(message))
        .collect();
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert!(rest[0].starts_with("TAIL-B"));
    assert!(
        compacted.tokens < super::sizing::compaction_threshold(30_000),
        "the compacted request is back under the trigger: {}",
        compacted.tokens
    );
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
        // Large window: this history is over the ~20k tail but under the
        // auto threshold, the case that must still write a checkpoint.
        context_window: 200_000,
        overhead_tokens: 0,
        measure: super::Measure::default(),
        on_summarize: None,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    let manual = super::compact_history(&scope, history.clone(), super::CompactMode::Manual).await;
    assert_eq!(manual.status, CompactStatus::Wrote);
    assert_eq!(manual.summary.as_deref(), Some(PLAUSIBLE_SUMMARY));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(user_text(&manual.messages[0]).contains("handoff notes"));
    assert!(user_text(&manual.messages[0]).contains("COMPACTION SUMMARY"));
    assert_eq!(user_text(&manual.messages[1]), "TAIL-ONE");
    assert_eq!(user_text(&manual.messages[2]), "TAIL-TWO");
    // The user's long head message stays with the summary only as its
    // start and end.
    let kept = user_text(&manual.messages[0]);
    assert!(kept.contains("HEAD-MARKER-") && kept.contains("characters omitted"));
    assert!(kept.len() < head.len() / 4, "{} chars kept", kept.len());

    let mut later = history;
    later.push(user("NEW-TURN"));
    let next_scope = super::CompactScope {
        checkpoint: scope.checkpoint.map(|slot| super::CheckpointSlot {
            head: "head-2",
            ..slot
        }),
        ..scope
    };
    let next = super::compact_history(&next_scope, later, super::CompactMode::Auto).await;
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
    assert_eq!(
        user_text(&next.messages[0]),
        kept,
        "the checkpoint keeps them too"
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
        measure: super::Measure::default(),
        on_summarize: None,
        cancel: &tokio_util::sync::CancellationToken::new(),
    };
    // `/compact` no longer answers "nothing to compact" for a short
    // session: the whole ledger is summarized and the tail is empty.
    let manual = super::compact_history(&scope, history, super::CompactMode::Manual).await;
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

mod scripted;
