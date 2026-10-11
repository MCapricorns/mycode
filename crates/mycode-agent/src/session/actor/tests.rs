//! Residency-cap and reservation-cap governance, driven through the actor
//! core's own action path (the same methods the pull loop dispatches to).
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use mycode_config::HomeLayout;

use super::super::dto::{EventKind, HeadStamp, SessionError, SessionResult};
use super::super::generation::GenerationFence;
use super::super::ids::{BranchId, SessionId};
use super::super::ledger::MAX_EVENT_RESERVATIONS;
use super::{MAX_RESIDENT_SESSIONS, OpFail, SessionCore, SessionPull};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
    home: HomeLayout,
}

impl Scratch {
    fn new() -> Self {
        let n = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mycode-actor-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("scratch dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
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

fn core(home: &HomeLayout) -> SessionCore {
    let fence = Arc::new(GenerationFence::new(Arc::new(AtomicU64::new(0))));
    fence.mark_current();
    SessionCore::new(home.clone(), fence)
}

fn mint() -> Option<(SessionId, BranchId)> {
    Some((SessionId::generate()?, BranchId::generate()?))
}

fn unwrap_complete(pull: Result<SessionPull, OpFail>) -> SessionResult {
    match pull {
        Ok(SessionPull::Complete(result)) => result,
        Ok(SessionPull::Failed(error)) => panic!("domain failure: {error:?}"),
        Err(failure) => panic!("infrastructure failure: {failure:?}"),
    }
}

/// Creates one session and returns its ids.
fn create(core: &mut SessionCore) -> (SessionId, BranchId) {
    let (session, branch) = mint().expect("ids");
    match unwrap_complete(core.action_create(&session, &branch)) {
        SessionResult::Created(_) => (session, branch),
        other => panic!("create returned {other:?}"),
    }
}

/// Reserves and commits one message event, returning the new head.
fn append_message(
    core: &mut SessionCore,
    session: &SessionId,
    branch: &BranchId,
    head: &HeadStamp,
    text: &[u8],
) -> HeadStamp {
    let view = match unwrap_complete(core.action_reserve_event(
        session,
        branch,
        EventKind::Message,
        None,
        text,
    )) {
        SessionResult::ReservedEvent(view) => view,
        other => panic!("reserve returned {other:?}"),
    };
    match unwrap_complete(core.action_append(session, branch, head, &view)) {
        SessionResult::Appended(appended) => appended.head,
        other => panic!("append returned {other:?}"),
    }
}

#[test]
fn residency_cap_evicts_least_recently_loaded_and_reloads_on_demand() {
    let scratch = Scratch::new();
    let mut core = core(&scratch.home);
    let mut created = Vec::new();
    let mut heads = Vec::new();
    for _ in 0..=MAX_RESIDENT_SESSIONS {
        let (session, branch) = create(&mut core);
        let head = append_message(&mut core, &session, &branch, &HeadStamp::Empty, b"first");
        created.push((session, branch));
        heads.push(head);
    }
    assert_eq!(core.sessions.len(), MAX_RESIDENT_SESSIONS);
    // Each create loads its own session last, so the first-created session is
    // the least recently loaded and must be the one evicted.
    assert!(!core.sessions.contains_key(&created[0].0));
    assert!(
        core.sessions
            .contains_key(&created[MAX_RESIDENT_SESSIONS].0)
    );
    // The store is the source of truth: a later open reloads the evicted
    // ledger with its committed head and event row intact.
    core.ensure_loaded(&created[0].0).expect("reload");
    let ledger = core
        .sessions
        .get(&created[0].0)
        .expect("reloaded session is resident");
    let branch_state = ledger.branches.get(&created[0].1).expect("reloaded branch");
    assert_eq!(branch_state.events.len(), 1);
    assert_eq!(branch_state.head, heads[0]);
    // The reload evicted the next-oldest session instead.
    assert!(!core.sessions.contains_key(&created[1].0));
    assert_eq!(core.sessions.len(), MAX_RESIDENT_SESSIONS);
}

#[test]
fn forget_drops_the_residency_bookkeeping_too() {
    let scratch = Scratch::new();
    let mut core = core(&scratch.home);
    let (session, _) = create(&mut core);
    assert!(core.resident_touch.contains_key(&session));
    unwrap_complete(core.action_evict(&session));
    assert!(!core.sessions.contains_key(&session));
    assert!(!core.resident_touch.contains_key(&session));
    let (other, _) = create(&mut core);
    assert!(core.resident_touch.contains_key(&other));
}

#[test]
fn a_live_reservation_pins_its_session_against_eviction() {
    let scratch = Scratch::new();
    let mut core = core(&scratch.home);
    let (session, branch) = create(&mut core);
    // Reserve without committing: the payload bytes exist nowhere durably yet.
    let pinned = match unwrap_complete(core.action_reserve_event(
        &session,
        &branch,
        EventKind::Message,
        None,
        b"pinned payload",
    )) {
        SessionResult::ReservedEvent(view) => view,
        other => panic!("reserve returned {other:?}"),
    };
    for _ in 0..(MAX_RESIDENT_SESSIONS + 2) {
        create(&mut core);
    }
    // The pinned session stayed resident even though the map overflows the
    // cap; every victim was quiescent.
    assert!(core.sessions.contains_key(&session));
    let appended =
        match unwrap_complete(core.action_append(&session, &branch, &HeadStamp::Empty, &pinned)) {
            SessionResult::Appended(appended) => appended,
            other => panic!("pinned append returned {other:?}"),
        };
    assert_eq!(
        appended.head,
        HeadStamp::Event(pinned.event_id.clone()),
        "the pinned reservation must survive the eviction pressure"
    );
}

#[test]
fn abandoned_event_reservations_stop_at_the_cap() {
    let scratch = Scratch::new();
    let mut core = core(&scratch.home);
    let (session, branch) = create(&mut core);
    let mut views = Vec::new();
    for index in 0..(MAX_EVENT_RESERVATIONS + 2) {
        let view = match unwrap_complete(core.action_reserve_event(
            &session,
            &branch,
            EventKind::Message,
            None,
            format!("residue {index}").as_bytes(),
        )) {
            SessionResult::ReservedEvent(view) => view,
            other => panic!("reserve returned {other:?}"),
        };
        views.push(view);
    }
    let ledger = core
        .sessions
        .get(&session)
        .expect("single resident session");
    assert_eq!(
        ledger.event_reservations.len(),
        MAX_EVENT_RESERVATIONS,
        "abandoned reservations must not accumulate past the cap"
    );
    // Oldest-first dropping: the first two reservations are gone, so their
    // appends fail NotFound exactly as an evicted reservation would.
    for stale in &views[..2] {
        match core.action_append(&session, &branch, &HeadStamp::Empty, stale) {
            Err(OpFail::Domain(error)) => {
                assert_eq!(error, SessionError::NotFound);
            }
            other => panic!("stale append unexpectedly returned {other:?}"),
        }
    }
    // The newest reservation is still live and commits at the empty head.
    let newest = views.pop().expect("newest reservation");
    let appended =
        match unwrap_complete(core.action_append(&session, &branch, &HeadStamp::Empty, &newest)) {
            SessionResult::Appended(appended) => appended,
            other => panic!("live append returned {other:?}"),
        };
    assert_eq!(appended.head, HeadStamp::Event(newest.event_id.clone()));
}

#[test]
fn append_cas_still_rejects_after_an_eviction_reload() {
    let scratch = Scratch::new();
    let mut core = core(&scratch.home);
    let (session, branch) = create(&mut core);
    let head = append_message(&mut core, &session, &branch, &HeadStamp::Empty, b"m0");
    // Force eviction, then reload.
    for _ in 0..MAX_RESIDENT_SESSIONS {
        create(&mut core);
    }
    assert!(!core.sessions.contains_key(&session));
    core.ensure_loaded(&session).expect("reload");
    let stale = match unwrap_complete(core.action_reserve_event(
        &session,
        &branch,
        EventKind::Message,
        None,
        b"stale",
    )) {
        SessionResult::ReservedEvent(view) => view,
        other => panic!("reserve returned {other:?}"),
    };
    match core.action_append(&session, &branch, &HeadStamp::Empty, &stale) {
        Err(OpFail::Domain(SessionError::Conflict(_))) => {}
        other => panic!("reloaded ledger must still reject a stale head: {other:?}"),
    }
    let reloaded = match unwrap_complete(core.action_reserve_event(
        &session,
        &branch,
        EventKind::Message,
        None,
        b"fresh",
    )) {
        SessionResult::ReservedEvent(view) => view,
        other => panic!("reserve returned {other:?}"),
    };
    match unwrap_complete(core.action_append(&session, &branch, &head, &reloaded)) {
        SessionResult::Appended(appended) => {
            assert_eq!(appended.head, HeadStamp::Event(reloaded.event_id.clone()));
        }
        other => panic!("append at the durable head returned {other:?}"),
    }
}
