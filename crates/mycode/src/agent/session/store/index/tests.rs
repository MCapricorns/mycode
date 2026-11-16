use std::path::PathBuf;

use super::*;

struct TemporaryHome(PathBuf);

impl Drop for TemporaryHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn failed_session_directory_creation_leaves_no_index_rows() {
    let root = std::env::temp_dir().join(format!("mycode-session-create-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let root = TemporaryHome(root);
    let home = HomeLayout::from_root(&root.0).unwrap();
    let mut store = SessionStore::open(&home).unwrap();
    let session = SessionId::generate().unwrap();
    let branch = BranchId::generate().unwrap();
    let dir = crate::config::new_session_dir(Some("project"), session.as_str()).unwrap();
    let blocked = root.0.join(crate::config::SESSIONS_DIR);
    std::fs::write(&blocked, b"not a directory").unwrap();

    assert!(matches!(
        store.create_session(&session, &branch, &dir),
        Err(StoreError::Storage)
    ));
    assert!(list_sessions(&home).unwrap().is_empty());
    assert!(matches!(
        store.load_session(&session),
        Err(StoreError::NotFound)
    ));

    std::fs::remove_file(&blocked).unwrap();
    store.create_session(&session, &branch, &dir).unwrap();
    assert!(home.owned_join(&dir).unwrap().is_dir());
    assert_eq!(list_sessions(&home).unwrap().len(), 1);
    assert_eq!(store.load_session(&session).unwrap().current, Some(branch));
}
