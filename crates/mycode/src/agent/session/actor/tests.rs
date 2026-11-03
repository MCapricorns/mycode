use super::*;

#[test]
fn pending_operation_pins_its_session_until_the_terminal_is_dropped() {
    let root = std::env::temp_dir().join(format!(
        "mycode-resident-operation-{}-{}",
        std::process::id(),
        crate::core::unix_now().as_nanos()
    ));
    let home = HomeLayout::from_root(&root).unwrap();
    let fence = Arc::new(GenerationFence::new());
    fence.mark_current();
    let mut core = SessionCore::new(home, fence);
    let create = |core: &mut SessionCore| {
        let operation = core
            .invoke_sync(SessionRequest::Create { project: None })
            .unwrap();
        let result = core
            .pull_sync(&mut operation.inner.lock().unwrap())
            .unwrap();
        let SessionPull::Complete(super::super::dto::SessionResult::Created(created)) = result
        else {
            panic!("session creation failed");
        };
        created.session_id
    };
    let first = create(&mut core);
    for _ in 0..MAX_RESIDENT_SESSIONS {
        create(&mut core);
    }
    assert!(!core.sessions.contains_key(&first));
    // Open reloads the evicted session and must pin the newly loaded ledger.
    let pending = core
        .invoke_sync(SessionRequest::Open {
            session: first.clone(),
        })
        .unwrap();
    // These operations can be queued before the first client's terminal pull.
    for _ in 0..MAX_RESIDENT_SESSIONS + 1 {
        create(&mut core);
    }
    assert!(core.sessions.contains_key(&first));
    let result = core.pull_sync(&mut pending.inner.lock().unwrap()).unwrap();
    assert!(matches!(
        result,
        SessionPull::Complete(super::super::dto::SessionResult::Opened(_))
    ));
    drop(pending);
    create(&mut core);
    assert!(
        !core.sessions.contains_key(&first),
        "a closed operation must release the residency pin"
    );
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
