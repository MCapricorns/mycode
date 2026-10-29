use super::*;
use session::BranchMutationKind;

struct Fixture {
    root: std::path::PathBuf,
    home: HomeLayout,
    service: SessionService,
    session: SessionId,
    branch: BranchId,
}

impl Fixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "mycode-recall-{label}-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = HomeLayout::from_root(&root).unwrap();
        let service = SessionService::new(&home);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let created = service.create(workspace.to_str()).await.unwrap();
        Self {
            root,
            home,
            service,
            session: created.session_id,
            branch: created.branch_id,
        }
    }

    async fn append(&self, head: &HeadStamp, kind: EventKind, payload: &[u8]) -> SessionEventId {
        let reservation = self
            .service
            .reserve_event(&self.session, &self.branch, kind, None, payload)
            .await
            .unwrap();
        match self
            .service
            .append(&self.session, &self.branch, head, &reservation)
            .await
            .unwrap()
            .head
        {
            HeadStamp::Event(event) => event,
            HeadStamp::Empty => panic!("append returned an empty head"),
        }
    }

    async fn recall(&self, head: &HeadStamp, event: &SessionEventId) -> ActiveConversation {
        recall_message(
            &self.home,
            &self.service,
            &self.session,
            &self.branch,
            head,
            event.as_str(),
        )
        .await
        .unwrap()
    }

    async fn finish(self) {
        self.service.shutdown().await;
        // The worker acknowledges drain just before dropping its SQLite handle.
        for _ in 0..100 {
            if std::fs::remove_dir_all(&self.root).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("temporary session store remained open");
    }
}

#[tokio::test]
async fn recall_first_message_keeps_original_history_and_workspace_and_can_send_again() {
    let f = Fixture::new("first").await;
    let workspace_file = f.root.join("workspace").join("existing.txt");
    std::fs::write(&workspace_file, "keep current workspace contents").unwrap();
    let first = f
        .append(
            &HeadStamp::Empty,
            EventKind::Message,
            "original
多行 prompt"
                .as_bytes(),
        )
        .await;
    let head = HeadStamp::Event(
        f.append(
            &HeadStamp::Event(first.clone()),
            EventKind::Message,
            b"later",
        )
        .await,
    );
    let original = f
        .service
        .read_payloads(&f.session, &f.branch, &head, None, 256)
        .await
        .unwrap();
    let recalled = f.recall(&head, &first).await;
    assert_eq!(recalled.head, "empty");
    assert!(recalled.entries.is_empty());
    assert!(recalled.older_before.is_none());
    assert_ne!(recalled.branch_id, f.branch.as_str());
    assert_eq!(
        std::fs::read_to_string(&workspace_file).unwrap(),
        "keep current workspace contents"
    );
    let still_original = f
        .service
        .read_payloads(&f.session, &f.branch, &head, None, 256)
        .await
        .unwrap();
    assert_eq!(original.items, still_original.items);
    let new_branch = BranchId::parse(&recalled.branch_id).unwrap();
    // Evict and reload to verify that the empty branch and current pointer are durable.
    f.service.forget(&f.session).await.unwrap();
    let opened = open_conversation(&f.service, &f.session).await.unwrap();
    assert_eq!(opened.branch_id, recalled.branch_id);
    assert_eq!(opened.head, "empty");
    let (_, entry) = send_message_with(
        &f.service,
        &f.session,
        &new_branch,
        &HeadStamp::Empty,
        "edited",
        &Default::default(),
    )
    .await
    .unwrap();
    let reopened = open_conversation(&f.service, &f.session).await.unwrap();
    assert_eq!(reopened.entries, [entry]);
    assert_eq!(
        f.service
            .load_event(&f.session, &f.branch, &first)
            .await
            .unwrap()
            .payload,
        "original
多行 prompt"
            .as_bytes()
    );
    f.finish().await;
}

#[tokio::test]
async fn recall_subsequent_message_keeps_every_preceding_event_and_attachment_payload() {
    let f = Fixture::new("subsequent").await;
    let attachments = mycode_agent::Attachments {
        files: vec![mycode_agent::TextAttachment {
            name: "note.txt".into(),
            // Exceeds the 64 KiB threshold for external payload storage.
            text: "attached text".repeat(8192),
            truncated: false,
        }],
        ..Default::default()
    };
    let (first_id, first_entry) = send_message_with(
        &f.service,
        &f.session,
        &f.branch,
        &HeadStamp::Empty,
        "first",
        &attachments,
    )
    .await
    .unwrap();
    let first = SessionEventId::parse(&first_id).unwrap();
    let metadata = f
        .append(&HeadStamp::Event(first.clone()), EventKind::Usage, b"{}")
        .await;
    let second = f
        .append(
            &HeadStamp::Event(metadata.clone()),
            EventKind::Message,
            b"second",
        )
        .await;
    let head = HeadStamp::Event(second.clone());
    let recalled = f.recall(&head, &second).await;
    assert_eq!(recalled.head, metadata.as_str());
    assert_eq!(recalled.entries, [first_entry]);
    let new_branch = BranchId::parse(&recalled.branch_id).unwrap();
    let replay = f
        .service
        .read_payloads(
            &f.session,
            &new_branch,
            &HeadStamp::Event(metadata),
            None,
            256,
        )
        .await
        .unwrap();
    assert_eq!(
        replay.items.len(),
        2,
        "invisible events belong to the prefix too"
    );
    let stored = mycode_agent::StoredUserMessage::from_payload(&replay.items[0].payload).unwrap();
    assert_eq!(stored.prompt, "first");
    assert_eq!(stored.attachments, attachments);
    assert!(
        f.service
            .load_event(&f.session, &f.branch, &second)
            .await
            .is_ok()
    );
    // Existing inclusive rewind still copies the target itself.
    let reservation = f
        .service
        .reserve_branch(&f.session, BranchMutationKind::Rewind, &f.branch, &second)
        .await
        .unwrap();
    let inclusive = f
        .service
        .rewind(&f.session, &f.branch, &second, &reservation)
        .await
        .unwrap();
    assert_eq!(inclusive.head, head);
    assert_eq!(
        f.service
            .read_payloads(&f.session, &inclusive.branch_id, &head, None, 256)
            .await
            .unwrap()
            .items
            .len(),
        3
    );
    // Fork shares the same durable prefix copy and must remain inclusive too.
    let reservation = f
        .service
        .reserve_branch(&f.session, BranchMutationKind::Fork, &f.branch, &first)
        .await
        .unwrap();
    let forked = f
        .service
        .fork(&f.session, &f.branch, &first, &reservation)
        .await
        .unwrap();
    assert_eq!(forked.head, HeadStamp::Event(first.clone()));
    assert_eq!(
        f.service
            .load_event(&f.session, &forked.branch_id, &first)
            .await
            .unwrap()
            .payload,
        replay.items[0].payload
    );
    f.finish().await;
}

#[tokio::test]
async fn recall_paginated_first_visible_message_preserves_invisible_root_metadata() {
    let f = Fixture::new("paginated-root").await;
    let mut head = HeadStamp::Empty;
    // Two pages of invisible root metadata: no visible predecessor exists.
    for _ in 0..(usize::from(VISIBLE_EVENTS) * 2 + 1) {
        head = HeadStamp::Event(f.append(&head, EventKind::Usage, b"{}").await);
    }
    let root_head = head.clone();
    let first = f.append(&head, EventKind::Message, b"first visible").await;
    head = HeadStamp::Event(first.clone());
    let tail = read_branch(&f.service, &f.session, &f.branch, &head)
        .await
        .unwrap();
    assert_eq!(tail.entries.len(), 1);
    let older = load_older(
        &f.service,
        &f.session,
        &f.branch,
        &head,
        tail.older_before.as_deref().unwrap(),
    )
    .await
    .unwrap();
    assert!(older.entries.is_empty());
    assert!(older.older.is_some());
    let recalled = f.recall(&head, &first).await;
    assert_eq!(recalled.head, head_spelling(&root_head));
    assert!(recalled.entries.is_empty());
    let new_branch = BranchId::parse(&recalled.branch_id).unwrap();
    let prefix = f
        .service
        .read_payloads(&f.session, &new_branch, &root_head, None, 256)
        .await
        .unwrap();
    assert_eq!(prefix.items.len(), usize::from(VISIBLE_EVENTS) * 2 + 1);
    assert!(
        prefix
            .items
            .iter()
            .all(|item| item.event.kind == EventKind::Usage)
    );
    f.service.forget(&f.session).await.unwrap();
    assert_eq!(
        open_conversation(&f.service, &f.session)
            .await
            .unwrap()
            .head,
        recalled.head
    );
    f.finish().await;
}

#[tokio::test]
async fn recall_rejects_stale_ui_heads_and_reservation_cas_without_changing_history() {
    let f = Fixture::new("stale").await;
    let first = f
        .append(&HeadStamp::Empty, EventKind::Message, b"first")
        .await;
    let stale = HeadStamp::Event(first.clone());
    let reservation = f
        .service
        .reserve_branch(
            &f.session,
            BranchMutationKind::RewindBefore,
            &f.branch,
            &first,
        )
        .await
        .unwrap();
    let latest = HeadStamp::Event(f.append(&stale, EventKind::Message, b"newer").await);
    assert!(
        matches!(f.service.rewind_before(&f.session, &f.branch, &first, &reservation).await, Err(SessionError::Conflict(conflict)) if conflict.actual == latest)
    );
    let error = recall_message(
        &f.home,
        &f.service,
        &f.session,
        &f.branch,
        &stale,
        first.as_str(),
    )
    .await
    .unwrap_err();
    assert_eq!(error, HEAD_MOVED_ON);
    let opened = f.service.open(&f.session).await.unwrap();
    assert_eq!(opened.heads.len(), 1);
    assert_eq!(opened.current.branch_id, f.branch);
    assert_eq!(opened.current.head, latest);
    f.finish().await;
}

#[tokio::test]
async fn recall_rejects_non_user_targets_and_crossed_reservations() {
    let f = Fixture::new("invalid-target").await;
    let metadata = f.append(&HeadStamp::Empty, EventKind::Usage, b"{}").await;
    let first = f
        .append(
            &HeadStamp::Event(metadata.clone()),
            EventKind::Message,
            b"first",
        )
        .await;
    let head = HeadStamp::Event(first.clone());
    assert!(
        recall_message(
            &f.home,
            &f.service,
            &f.session,
            &f.branch,
            &head,
            metadata.as_str()
        )
        .await
        .is_err()
    );
    let inclusive = f
        .service
        .reserve_branch(&f.session, BranchMutationKind::Rewind, &f.branch, &first)
        .await
        .unwrap();
    assert!(matches!(
        f.service
            .rewind_before(&f.session, &f.branch, &first, &inclusive)
            .await,
        Err(SessionError::InvalidArgument)
    ));
    assert_eq!(f.service.open(&f.session).await.unwrap().heads.len(), 1);
    f.finish().await;
}
