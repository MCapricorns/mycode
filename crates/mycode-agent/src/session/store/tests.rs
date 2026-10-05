//! SQLite index and JSONL log behavior through the session service.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mycode_config::HomeLayout;
use mycode_core::{AssistantMessage, StopReason};

use crate::session::{
    BranchMutationKind, EventKind, HeadStamp, SessionError, SessionService, inspect_sessions,
};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
    home: HomeLayout,
}

impl Scratch {
    fn new() -> Self {
        let n = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mycode-session-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("scratch dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("scratch mode");
        }
        let home = HomeLayout::from_root(&root).expect("home");
        Self { root, home }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn jsonl_path(home: &HomeLayout, session: &str, branch: &str) -> PathBuf {
    home.root()
        .join("sessions")
        .join(session)
        .join(format!("{branch}.jsonl"))
}

async fn append_text(
    service: &SessionService,
    session: &crate::session::SessionId,
    branch: &crate::session::BranchId,
    head: &HeadStamp,
    text: &str,
) -> HeadStamp {
    let reservation = service
        .reserve_event(session, branch, EventKind::Message, None, text.as_bytes())
        .await
        .expect("reserve");
    service
        .append(session, branch, head, &reservation)
        .await
        .expect("append")
        .head
}

#[test]
fn append_lists_title_without_reading_payloads() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create().await.expect("create");
        let assistant = serde_json::to_vec(&AssistantMessage {
            blocks: Vec::new(),
            usage: None,
            stop_reason: StopReason::Stop,
        })
        .expect("assistant json");
        let reservation = service
            .reserve_event(
                &created.session_id,
                &created.branch_id,
                EventKind::Message,
                None,
                &assistant,
            )
            .await
            .expect("reserve assistant");
        let head = service
            .append(
                &created.session_id,
                &created.branch_id,
                &HeadStamp::Empty,
                &reservation,
            )
            .await
            .expect("append assistant")
            .head;
        append_text(
            &service,
            &created.session_id,
            &created.branch_id,
            &head,
            "hello   title from user",
        )
        .await;
        let listing = inspect_sessions(&scratch.home).expect("list");
        assert!(listing.corrupt.is_empty());
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].title, "hello title from user");
        assert_eq!(listing.sessions[0].event_count, 2);
        service.shutdown().await;
    });
}

#[test]
fn stale_head_append_conflicts_and_reservation_is_spent() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create().await.expect("create");
        let first = service
            .reserve_event(
                &created.session_id,
                &created.branch_id,
                EventKind::Message,
                None,
                b"first",
            )
            .await
            .expect("reserve first");
        let second = service
            .reserve_event(
                &created.session_id,
                &created.branch_id,
                EventKind::Message,
                None,
                b"second",
            )
            .await
            .expect("reserve second");
        let appended = service
            .append(
                &created.session_id,
                &created.branch_id,
                &HeadStamp::Empty,
                &second,
            )
            .await
            .expect("append second");
        let conflict = service
            .append(
                &created.session_id,
                &created.branch_id,
                &HeadStamp::Empty,
                &first,
            )
            .await;
        match conflict {
            Err(SessionError::Conflict(conflict)) => assert_eq!(conflict.actual, appended.head),
            other => panic!("expected conflict, got {other:?}"),
        }
        service.shutdown().await;
    });
}

#[test]
fn rewind_keeps_the_prefix_and_window_pages_backward() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create().await.expect("create");
        let mut head = HeadStamp::Empty;
        for text in ["one", "two", "three", "four", "five"] {
            head = append_text(
                &service,
                &created.session_id,
                &created.branch_id,
                &head,
                text,
            )
            .await;
        }
        let tail = service
            .read_payload_window(&created.session_id, &created.branch_id, &head, None, 2)
            .await
            .expect("tail");
        assert_eq!(tail.items.len(), 2);
        assert_eq!(tail.items[0].payload, b"four");
        assert_eq!(tail.items[1].payload, b"five");
        let older = tail.older.expect("older cursor");
        let middle = service
            .read_payload_window(
                &created.session_id,
                &created.branch_id,
                &head,
                Some(&older),
                2,
            )
            .await
            .expect("middle");
        assert_eq!(middle.items[0].payload, b"two");
        assert_eq!(middle.items[1].payload, b"three");

        let page = service
            .read(&created.session_id, &created.branch_id, &head, None, 8)
            .await
            .expect("read");
        let first = page.items[0].event_id.clone();
        let reservation = service
            .reserve_branch(
                &created.session_id,
                BranchMutationKind::Rewind,
                &created.branch_id,
                &first,
            )
            .await
            .expect("reserve rewind");
        let branched = service
            .rewind(
                &created.session_id,
                &created.branch_id,
                &first,
                &reservation,
            )
            .await
            .expect("rewind");
        let prefix = service
            .read_payloads(
                &created.session_id,
                &branched.branch_id,
                &branched.head,
                None,
                8,
            )
            .await
            .expect("prefix");
        assert_eq!(prefix.items.len(), 1);
        assert_eq!(prefix.items[0].payload, b"one");
        service.shutdown().await;
    });
}

#[test]
fn external_payload_bytes_stay_out_of_sqlite() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create().await.expect("create");
        let marker = b"UNIQUE_MARKER_XY";
        let mut payload = vec![0xAB; 64 * 1024];
        payload[..marker.len()].copy_from_slice(marker);
        let reservation = service
            .reserve_event(
                &created.session_id,
                &created.branch_id,
                EventKind::Usage,
                None,
                &payload,
            )
            .await
            .expect("reserve");
        let head = service
            .append(
                &created.session_id,
                &created.branch_id,
                &HeadStamp::Empty,
                &reservation,
            )
            .await
            .expect("append")
            .head;
        let HeadStamp::Event(event) = &head else {
            panic!("head");
        };
        let loaded = service
            .load_event(&created.session_id, &created.branch_id, event)
            .await
            .expect("load");
        assert_eq!(loaded.payload, payload);
        service.shutdown().await;
        let db = std::fs::read(scratch.home.root().join("sessions.db")).expect("db");
        assert!(
            !contains_bytes(&db, marker),
            "payload bytes landed in the sqlite file"
        );
        let wal = scratch.home.root().join("sessions.db-wal");
        if wal.exists() {
            let wal_bytes = std::fs::read(&wal).expect("wal");
            assert!(
                !contains_bytes(&wal_bytes, marker),
                "payload bytes landed in the wal"
            );
        }
    });
}

#[test]
fn torn_tail_is_truncated_and_unindexed_directories_are_ignored() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create().await.expect("create");
        append_text(
            &service,
            &created.session_id,
            &created.branch_id,
            &HeadStamp::Empty,
            "kept",
        )
        .await;
        let log = jsonl_path(
            &scratch.home,
            created.session_id.as_str(),
            created.branch_id.as_str(),
        );
        let committed = std::fs::metadata(&log).expect("log").len();
        service.forget(&created.session_id).await.expect("forget");
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .expect("open log");
        file.write_all(b"torn-tail-not-a-record\n").expect("tear");
        drop(file);
        service.open(&created.session_id).await.expect("reopen");
        assert_eq!(std::fs::metadata(&log).expect("log").len(), committed);
        let junk = scratch.home.root().join("sessions").join("junk");
        std::fs::create_dir_all(junk.join("pending")).expect("junk");
        std::fs::write(junk.join("manifest.json"), b"{}").expect("manifest");
        std::fs::create_dir_all(junk.join("branches")).expect("branches");
        std::fs::write(junk.join("branches").join("main.events"), b"old").expect("events");
        let listing = inspect_sessions(&scratch.home).expect("list");
        assert!(listing.corrupt.is_empty());
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].title, "kept");
        service.shutdown().await;
    });
}
