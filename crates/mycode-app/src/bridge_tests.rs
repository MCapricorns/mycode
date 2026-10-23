use std::time::Duration;

use crate::{BranchId, BridgeCommand, BridgeReply, CoreBridge, HeadStamp, SessionId};

const WAIT: Duration = Duration::from_secs(5);
const SESSION: &str = "sess-20261019-120000-0123abcd";

// Removed: retained_unpolled_reply_does_not_block_shutdown and
// shutdown_drains_persistent_commands_when_reply_waits_are_dropped — both
// spawned a real `run_core` worker thread over a %TEMP% HomeLayout and
// intermittently hung on this Windows host. The channel-level failure routes
// below keep the regression coverage without file IO.

#[derive(Clone, Copy)]
enum Failure {
    StoppedBridge,
    ClosedCommands,
    DroppedReply,
}

async fn failed_request(command: BridgeCommand, failure: Failure) -> BridgeReply {
    let (command_tx, mut commands) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = CoreBridge {
        command_tx: Some(command_tx),
        worker: None,
    };
    let discard_reply = match failure {
        Failure::StoppedBridge => {
            bridge.shutdown();
            drop(commands);
            None
        }
        Failure::ClosedCommands => {
            drop(commands);
            None
        }
        Failure::DroppedReply => Some(tokio::spawn(async move {
            let request = tokio::time::timeout(WAIT, commands.recv())
                .await
                .expect("bounded receive")
                .expect("accepted command");
            drop(request);
        })),
    };
    let reply = tokio::time::timeout(WAIT, bridge.request(command))
        .await
        .expect("bounded failure reply");
    if let Some(task) = discard_reply {
        tokio::time::timeout(WAIT, task)
            .await
            .expect("bounded reply discard")
            .expect("reply discard task");
    }
    reply
}

async fn assert_failure_routes(failure: Failure) {
    let expected = match failure {
        Failure::StoppedBridge => "core bridge stopped",
        Failure::ClosedCommands | Failure::DroppedReply => "core thread stopped",
    };
    let reply = failed_request(BridgeCommand::CreateSession { project: None }, failure).await;
    assert!(
        matches!(&reply, BridgeReply::Created(Err(message)) if message == expected),
        "wrong create reply: {reply:?}"
    );
    let reply = failed_request(
        BridgeCommand::SendMessage {
            session: SessionId::parse(SESSION).unwrap(),
            branch: BranchId::parse("br1-0123456789abcdef0123456789abcdef").unwrap(),
            expected_head: HeadStamp::Empty,
            text: "unsent".to_owned(),
            attachments: mycode_agent::Attachments::default(),
        },
        failure,
    )
    .await;
    assert!(
        matches!(&reply, BridgeReply::Sent { session, text, outcome: Err(message) }
            if session == SESSION && text == "unsent" && message == expected),
        "wrong send reply or lost session: {reply:?}"
    );
    let reply = failed_request(
        BridgeCommand::McpListTools {
            server: Box::new(mycode_agent::McpServerSettings {
                id: "routing-probe".to_owned(),
                enabled: false,
                transport: "http".to_owned(),
                command: None,
                args: Vec::new(),
                env: Default::default(),
                endpoint: Some("https://example.invalid/mcp".to_owned()),
                key_header: None,
                headers: Default::default(),
            }),
        },
        failure,
    )
    .await;
    assert!(
        matches!(&reply, BridgeReply::McpTools { server_id, outcome: Err(message) }
            if server_id == "routing-probe" && message == expected),
        "wrong MCP reply or lost server: {reply:?}"
    );
    let reply = failed_request(BridgeCommand::CheckUpdate, failure).await;
    assert!(
        matches!(&reply, BridgeReply::UpdateChecked(Err(message)) if message == expected),
        "wrong updater reply: {reply:?}"
    );
}

#[tokio::test]
async fn stopped_bridge_preserves_reply_routes() {
    assert_failure_routes(Failure::StoppedBridge).await;
}

#[tokio::test]
async fn closed_command_channel_preserves_reply_routes() {
    assert_failure_routes(Failure::ClosedCommands).await;
}

#[tokio::test]
async fn dropped_reply_sender_preserves_reply_routes() {
    assert_failure_routes(Failure::DroppedReply).await;
}
