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
            crate::unix_now().as_nanos()
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
        .join(mycode_config::NO_PROJECT_DIR)
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
        let created = service.create(None).await.expect("create");
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
        let created = service.create(None).await.expect("create");
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
        let created = service.create(None).await.expect("create");
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

/// A rewind forks onto a new branch; reopening the session must land on the
/// branch writes last went to, never back on the smallest branch id — that
/// pick used to replay the pre-rewind conversation.
#[test]
fn reopening_after_a_rewind_lands_on_the_new_branch() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create(None).await.expect("create");
        let mut head = HeadStamp::Empty;
        for text in ["one", "two", "three"] {
            head = append_text(
                &service,
                &created.session_id,
                &created.branch_id,
                &head,
                text,
            )
            .await;
        }
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
        let branch_head = append_text(
            &service,
            &created.session_id,
            &branched.branch_id,
            &branched.head,
            "post-rewind",
        )
        .await;

        // The in-memory ledger already tracks the new branch; drop it so the
        // reopen is answered by the index, the path a relaunch takes.
        service.forget(&created.session_id).await.expect("forget");
        let opened = service.open(&created.session_id).await.expect("reopen");
        assert_eq!(opened.current.branch_id, branched.branch_id);
        assert_eq!(opened.current.head, branch_head);
        service.shutdown().await;

        // A fresh service — a real relaunch — lands on the same branch.
        let service = SessionService::new(&scratch.home);
        let reopened = service.open(&created.session_id).await.expect("relaunch");
        assert_eq!(reopened.current.branch_id, branched.branch_id);
        assert_eq!(reopened.current.head, branch_head);
        service.shutdown().await;
    });
}

#[test]
fn external_payload_bytes_stay_out_of_sqlite() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create(None).await.expect("create");
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
        let created = service.create(None).await.expect("create");
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

/// Resident ledgers are capped, so a long-running process that opens many
/// sessions reloads the least-recently-opened one from the index on demand.
/// Reading back through the whole service proves the reload is transparent.
#[test]
fn reading_an_evicted_session_reloads_from_the_store() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        // One more session than the residency cap, each with its own events.
        let mut sessions = Vec::new();
        for index in 0..(crate::session::actor::MAX_RESIDENT_SESSIONS + 1) {
            let created = service.create(None).await.expect("create");
            let head = append_text(
                &service,
                &created.session_id,
                &created.branch_id,
                &HeadStamp::Empty,
                &format!("only message of session {index}"),
            )
            .await;
            sessions.push((created, head));
        }
        // The first-created session is the least recently loaded; opening the
        // rest evicted it. A fresh read must reload it from the index.
        let (first, first_head) = &sessions[0];
        let opened = service
            .open(&first.session_id)
            .await
            .expect("reopen evicted");
        assert_eq!(opened.heads.len(), 1);
        assert_eq!(opened.heads[0].head, *first_head);
        let window = service
            .read_payload_window(
                &first.session_id,
                &first.branch_id,
                &opened.heads[0].head,
                None,
                16,
            )
            .await
            .expect("window");
        assert_eq!(window.items.len(), 1);
        assert_eq!(
            String::from_utf8_lossy(&window.items[0].payload),
            "only message of session 0"
        );
        // A new append after the reload still commits at the durable tip.
        append_text(
            &service,
            &first.session_id,
            &first.branch_id,
            first_head,
            "after reload",
        )
        .await;
        service.shutdown().await;
    });
}

/// A live reservation's payload bytes exist nowhere durably yet, so
/// eviction pressure from other sessions must never drop it.
#[test]
fn a_reserved_event_survives_other_sessions_churning() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create(None).await.expect("create");
        let reservation = service
            .reserve_event(
                &created.session_id,
                &created.branch_id,
                EventKind::Message,
                None,
                b"pinned across churn",
            )
            .await
            .expect("reserve");
        for _ in 0..(crate::session::actor::MAX_RESIDENT_SESSIONS + 2) {
            service.create(None).await.expect("create");
        }
        let appended = service
            .append(
                &created.session_id,
                &created.branch_id,
                &HeadStamp::Empty,
                &reservation,
            )
            .await
            .expect("reserved append must survive the churn");
        assert_eq!(
            appended.head,
            HeadStamp::Event(reservation.event_id.clone())
        );
        service.shutdown().await;
    });
}

#[test]
fn missing_or_lengthened_external_payload_and_bad_index_rows_report_corrupt() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service.create(None).await.expect("create");
        let payload = vec![b'x'; 64 * 1024];
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
        let HeadStamp::Event(event_id) = &head else {
            panic!("head");
        };
        let payload_path = scratch
            .home
            .root()
            .join("sessions")
            .join(mycode_config::NO_PROJECT_DIR)
            .join(created.session_id.as_str())
            .join("payloads")
            .join(format!("{}.bin", event_id.as_str()));

        // Trailing bytes on an external payload file must fail closed as Corrupt.
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&payload_path)
                .expect("open payload");
            file.write_all(b"extra").expect("append extra");
        }
        assert_eq!(
            service
                .load_event(&created.session_id, &created.branch_id, event_id)
                .await,
            Err(SessionError::Corrupt)
        );

        // A missing external payload file must also fail closed as Corrupt.
        std::fs::remove_file(&payload_path).expect("remove payload");
        assert_eq!(
            service
                .load_event(&created.session_id, &created.branch_id, event_id)
                .await,
            Err(SessionError::Corrupt)
        );

        // A missing JSONL log when reading or branching a committed event must report Corrupt.
        let log_path = jsonl_path(
            &scratch.home,
            created.session_id.as_str(),
            created.branch_id.as_str(),
        );
        std::fs::remove_file(&log_path).expect("remove log");
        assert_eq!(
            service
                .load_event(&created.session_id, &created.branch_id, event_id)
                .await,
            Err(SessionError::Corrupt)
        );
        let branch_res = service
            .reserve_branch(
                &created.session_id,
                BranchMutationKind::Fork,
                &created.branch_id,
                event_id,
            )
            .await
            .expect("reserve branch");
        assert_eq!(
            service
                .fork(
                    &created.session_id,
                    &created.branch_id,
                    event_id,
                    &branch_res,
                )
                .await,
            Err(SessionError::Corrupt)
        );

        // Compaction summary payloads must not become the session title.
        let summary_session = service.create(None).await.expect("create summary session");
        let summary_head = append_text(
            &service,
            &summary_session.session_id,
            &summary_session.branch_id,
            &HeadStamp::Empty,
            "COMPACTION SUMMARY\n\nsummary text",
        )
        .await;
        append_text(
            &service,
            &summary_session.session_id,
            &summary_session.branch_id,
            &summary_head,
            "real user prompt",
        )
        .await;
        let listing = inspect_sessions(&scratch.home).expect("list");
        let summary_listed = listing
            .sessions
            .iter()
            .find(|row| row.session_id == summary_session.session_id)
            .expect("summary session listed");
        assert_eq!(summary_listed.title, "real user prompt");
        service.shutdown().await;
    });

    // Corrupt SQLite index invariants (negative event_count or mismatched head/count)
    // must surface in `listing.corrupt` instead of aborting the whole listing.
    let db_path = scratch.home.root().join("sessions.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let bad_session = crate::session::SessionId::generate().expect("bad session id");
    let bad_branch = crate::session::BranchId::generate().expect("bad branch id");
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, root_branch, event_count, corrupt)
         VALUES (?1, 'bad', 1, ?2, -1, 0)",
        rusqlite::params![bad_session.as_str(), bad_branch.as_str()],
    )
    .expect("insert bad session");
    conn.execute(
        "INSERT INTO branches (
            session_id, branch_id, head, event_count, committed_bytes,
            parent_kind, source_branch_id, at_event_id, to_event_id
         ) VALUES (?1, ?2, 'empty', 0, 0, 'root', NULL, NULL, NULL)",
        rusqlite::params![bad_session.as_str(), bad_branch.as_str()],
    )
    .expect("insert bad branch");
    drop(conn);

    let listing = inspect_sessions(&scratch.home).expect("list with corrupt row");
    assert!(
        listing.corrupt.contains(&bad_session.as_str().to_owned()),
        "corrupt row should be isolated in listing.corrupt: {listing:?}"
    );
}

#[test]
fn a_session_lives_in_its_project_folder_under_a_timestamped_name() {
    let scratch = Scratch::new();
    runtime().block_on(async {
        let service = SessionService::new(&scratch.home);
        let created = service
            .create(Some(r"D:\work\my app\"))
            .await
            .expect("create");
        let id = created.session_id.as_str();
        assert!(
            crate::session::SessionId::parse(id).is_some_and(|parsed| parsed == created.session_id)
        );
        let stamp = id.strip_prefix("sess-").expect("sess- prefix");
        assert_eq!(stamp.len(), "20261019-153012-0123abcd".len(), "{id}");
        let dir = scratch
            .home
            .root()
            .join("sessions")
            .join("D--work-my-app")
            .join(id);
        assert!(
            dir.is_dir(),
            "created under the project key: {}",
            dir.display()
        );
        assert_eq!(
            mycode_config::locate_session_dir(&scratch.home, id).expect("locate"),
            Some(format!("sessions/D--work-my-app/{id}"))
        );
        let head = append_text(
            &service,
            &created.session_id,
            &created.branch_id,
            &HeadStamp::Empty,
            "first",
        )
        .await;
        // A cold store finds the directory again without being told.
        service.forget(&created.session_id).await.expect("forget");
        service.open(&created.session_id).await.expect("reopen");
        append_text(
            &service,
            &created.session_id,
            &created.branch_id,
            &head,
            "second",
        )
        .await;
        assert!(
            dir.join(format!("{}.jsonl", created.branch_id.as_str()))
                .is_file()
        );
        assert!(!dir.join("title.txt").exists(), "no title marker");
        assert_eq!(
            inspect_sessions(&scratch.home).expect("list").sessions[0].event_count,
            2
        );
        service.shutdown().await;
    });
}
