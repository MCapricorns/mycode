use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mycode_agent::{HomeLayout, UiState, replace_ui_state};

use crate::{BranchId, BridgeCommand, BridgeReply, CoreBridge, HeadStamp, SessionId};

const WAIT: Duration = Duration::from_secs(5);
const SESSION: &str = "sess-20261019-120000-0123abcd";

struct TestHome(HomeLayout);

impl TestHome {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "mycode-bridge-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("test home directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private test home");
        }
        let home = HomeLayout::from_root(&root).expect("test home");
        replace_ui_state(
            &home,
            &UiState {
                auto_update: false,
                ..UiState::default()
            },
        )
        .expect("disable automatic update checks");
        Self(home)
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.root());
    }
}

fn gated_bridge(home: HomeLayout) -> (CoreBridge, mpsc::Sender<()>) {
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (start_tx, start_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        start_rx.recv_timeout(WAIT).expect("worker start signal");
        crate::dispatch::run_core(home, command_rx, event_tx);
    });
    (
        CoreBridge {
            command_tx: Some(command_tx),
            worker: Some(worker),
        },
        start_tx,
    )
}

fn shutdown_in_background(mut bridge: CoreBridge) -> (mpsc::Receiver<()>, JoinHandle<()>) {
    let (done_tx, done_rx) = mpsc::channel();
    let shutdown = thread::spawn(move || {
        bridge.shutdown();
        let _ = done_tx.send(());
    });
    (done_rx, shutdown)
}

#[test]
fn retained_unpolled_reply_does_not_block_shutdown() {
    let home = TestHome::new("unpolled");
    let (bridge, start) = gated_bridge(home.0.clone());
    let reply = bridge.request(BridgeCommand::CreateSession { project: None });
    let (done, shutdown) = shutdown_in_background(bridge);
    start.send(()).expect("start worker");

    if let Err(error) = done.recv_timeout(WAIT) {
        // Release the old implementation's retained sender before failing,
        // so the regression cannot leave a permanently blocked test thread.
        drop(reply);
        if done.recv_timeout(WAIT).is_ok() {
            shutdown.join().expect("shutdown thread");
        }
        panic!("shutdown waited for an unpolled reply future: {error}");
    }
    shutdown.join().expect("shutdown thread");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let reply = runtime
        .block_on(async move { tokio::time::timeout(WAIT, reply).await })
        .expect("reply after shutdown");
    let BridgeReply::Created(Ok(created)) = reply else {
        panic!("accepted session creation did not complete: {reply:?}");
    };
    let listing = mycode_agent::session::inspect_sessions(&home.0).expect("durable sessions");
    assert!(listing.corrupt.is_empty());
    assert_eq!(listing.sessions.len(), 1);
    assert_eq!(listing.sessions[0].session_id.as_str(), created.session_id);
}

#[test]
fn shutdown_drains_persistent_commands_when_reply_waits_are_dropped() {
    let home = TestHome::new("dropped");
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = CoreBridge {
        command_tx: Some(command_tx),
        worker: None,
    };
    let expected_ui = UiState {
        auto_update: false,
        selected_model: Some("durable-marker".to_owned()),
        ..UiState::default()
    };
    drop(bridge.request(BridgeCommand::CreateSession { project: None }));
    drop(bridge.request(BridgeCommand::SaveUiState {
        state: expected_ui.clone(),
    }));
    // Close the command channel before starting the real worker, so every
    // accepted operation is still queued when the drain begins.
    bridge.shutdown();
    let worker_home = home.0.clone();
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (done_tx, done) = mpsc::channel();
    let worker = thread::spawn(move || {
        crate::dispatch::run_core(worker_home, command_rx, event_tx);
        let _ = done_tx.send(());
    });
    done.recv_timeout(WAIT).expect("bounded worker drain");
    worker.join().expect("core worker thread");

    let listing = mycode_agent::session::inspect_sessions(&home.0).expect("durable sessions");
    assert!(listing.corrupt.is_empty());
    assert_eq!(listing.sessions.len(), 1, "accepted creation was discarded");
    assert_eq!(mycode_agent::read_ui_state(&home.0).unwrap(), expected_ui);
}

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
