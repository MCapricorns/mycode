use super::*;

fn git(repo: &Path, args: &[&str]) {
    let output = git_command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Repository {
    root: PathBuf,
    repo: PathBuf,
    home: HomeLayout,
}

impl Repository {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mycode-worktree-test-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let home = HomeLayout::from_root(root.join("home")).unwrap();
        git(&repo, &["init", "--quiet"]);
        std::fs::write(repo.join("tracked.txt"), "original\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        Self { root, repo, home }
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn worktree_handoff_and_recovery_preserve_modified_untracked_and_ignored_files() {
    let fixture = Repository::new();
    git(&fixture.repo, &["config", "core.autocrlf", "true"]);
    for fail in [false, true] {
        let lease = WorktreeLease::acquire(&fixture.home, &fixture.repo).unwrap();
        let path = lease.path.clone();
        let manifest = lease.manifest.clone();
        std::fs::write(path.join("tracked.txt"), "changed\n").unwrap();
        std::fs::write(path.join("untracked.bin"), [0, 159, 255]).unwrap();
        std::fs::write(path.join("ignored.txt"), "keep ignored\n").unwrap();
        let result = if fail {
            Err(crate::subagent::cancelled_subagent("artisan"))
        } else {
            Ok("finished".to_owned())
        };
        let handoff = handoff_worktree(lease, result);
        assert_eq!(handoff.is_err(), fail);
        let text = match handoff {
            Ok(text) => text,
            Err(error) => error.to_string(),
        };
        assert!(
            text.contains(&format!("Worktree retained at {}", path.display())),
            "{text}"
        );
        if fail {
            assert!(text.contains(mycode_agent::tool::INTERRUPTED_NOTE));
        }
        assert!(manifest.exists());
        recover_agent_worktrees(&fixture.home);
        assert_eq!(
            std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            std::fs::read(path.join("untracked.bin")).unwrap(),
            [0, 159, 255]
        );
        assert_eq!(
            std::fs::read_to_string(path.join("ignored.txt")).unwrap(),
            "keep ignored\n"
        );
        assert!(manifest.exists());
        assert_eq!(
            std::fs::read_to_string(fixture.repo.join("tracked.txt")).unwrap(),
            "original\n"
        );
        // Restore Git's checkout bytes, including configured line-ending conversion.
        git(
            &path,
            &[
                "restore",
                "--source=HEAD",
                "--worktree",
                "--",
                "tracked.txt",
            ],
        );
        std::fs::remove_file(path.join("untracked.bin")).unwrap();
        std::fs::remove_file(path.join("ignored.txt")).unwrap();
        assert!(
            WorktreeLease {
                path: path.clone(),
                manifest: manifest.clone()
            }
            .release()
        );
        assert!(!path.exists());
        assert!(!manifest.exists());
    }
}

#[test]
fn ignored_files_and_unrecognized_lease_directories_are_never_erased() {
    let fixture = Repository::new();
    let lease = WorktreeLease::acquire(&fixture.home, &fixture.repo).unwrap();
    let path = lease.path.clone();
    let manifest = lease.manifest.clone();
    std::fs::write(path.join("ignored.txt"), "user content").unwrap();
    assert!(!lease.release());
    assert!(manifest.exists());
    let unknown = fixture
        .home
        .root()
        .join(WORKTREE_LEASE_DIR)
        .join("unknown-checkout");
    std::fs::create_dir_all(&unknown).unwrap();
    std::fs::write(unknown.join("draft.txt"), "keep draft").unwrap();
    recover_agent_worktrees(&fixture.home);
    assert_eq!(
        std::fs::read_to_string(path.join("ignored.txt")).unwrap(),
        "user content"
    );
    assert!(unknown.join("draft.txt").exists());
}

#[tokio::test]
async fn dropping_worktree_acquisition_still_reclaims_the_eventual_clean_lease() {
    let fixture = Repository::new();
    let home = fixture.home.clone();
    let repo = fixture.repo.clone();
    let (path_tx, path_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let acquire = tokio::task::spawn_blocking(move || {
        let lease = WorktreeLease::acquire(&home, &repo).unwrap();
        let path = lease.path.clone();
        let guard = LeaseGuard(Some(lease));
        path_tx.send(path).unwrap();
        finish_rx.recv().unwrap();
        guard
    });
    let path = path_rx.await.unwrap();
    drop(acquire);
    finish_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the dropped acquisition's guard must reclaim its checkout");
}

#[test]
fn reopening_original_worktree_preserves_edits_untracked_and_ignored_files_without_git_changes() {
    let fixture = Repository::new();
    let lease = WorktreeLease::acquire(&fixture.home, &fixture.repo).unwrap();
    let path = lease.path.clone();
    let manifest = lease.manifest.clone();
    std::fs::write(path.join("tracked.txt"), "original edit\n").unwrap();
    std::fs::write(path.join("untracked.bin"), [0, 159, 255]).unwrap();
    std::fs::write(path.join("ignored.txt"), "keep ignored").unwrap();
    handoff_worktree(lease, Ok("first result".into())).unwrap();
    let reopened = WorktreeLease::reopen(&fixture.home, &fixture.repo, &path).unwrap();
    assert_eq!(reopened.path, path.canonicalize().unwrap());
    assert_eq!(
        std::fs::read_to_string(reopened.path.join("tracked.txt")).unwrap(),
        "original edit\n"
    );
    std::fs::write(
        reopened.path.join("tracked.txt"),
        "original edit\nrework edit\n",
    )
    .unwrap();
    assert!(
        handoff_worktree(
            reopened,
            Err(mycode_agent::ToolError::Execution(
                "review still failed".into()
            ))
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "original edit\nrework edit\n"
    );
    assert_eq!(
        std::fs::read(path.join("untracked.bin")).unwrap(),
        [0, 159, 255]
    );
    assert_eq!(
        std::fs::read_to_string(path.join("ignored.txt")).unwrap(),
        "keep ignored"
    );
    assert!(manifest.exists());
    let foreign = Repository::new();
    assert!(WorktreeLease::reopen(&fixture.home, &foreign.repo, &path).is_err());
    assert!(WorktreeLease::reopen(&fixture.home, &fixture.repo, &fixture.repo).is_err());
    assert!(
        WorktreeLease::reopen(
            &fixture.home,
            &fixture.repo,
            &fixture.home.root().join("missing-checkout")
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("tracked.txt")).unwrap(),
        "original\n"
    );
}

#[test]
fn a_clean_worktree_is_removed_after_handoff() {
    let fixture = Repository::new();
    let lease = WorktreeLease::acquire(&fixture.home, &fixture.repo).unwrap();
    let path = lease.path.clone();
    let manifest = lease.manifest.clone();
    assert_eq!(
        handoff_worktree(lease, Ok("done".to_owned())).unwrap(),
        "done"
    );
    assert!(!path.exists());
    assert!(!manifest.exists());
}
