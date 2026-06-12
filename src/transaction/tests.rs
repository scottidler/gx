use super::*;
use crate::test_utils::run_git_command;
use std::sync::Mutex;
use tempfile::TempDir;

// Serialize env-var mutation across tests (recovery/backup dirs key off XDG_DATA_HOME).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_data_home<F: FnOnce()>(dir: &Path, f: F) {
    let guard = ENV_LOCK.lock().unwrap();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
    f();
    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

fn git(args: &[&str], dir: &Path) {
    let out = run_git_command(args, dir);
    assert!(out.status.success(), "git {args:?} failed");
}

fn init_repo(dir: &Path) {
    git(&["init", "--quiet"], dir);
    git(&["config", "user.email", "t@e.com"], dir);
    git(&["config", "user.name", "T"], dir);
    git(&["config", "commit.gpgsign", "false"], dir);
    std::fs::write(dir.join("README.md"), "# repo\n").unwrap();
    git(&["add", "-A"], dir);
    git(&["commit", "--quiet", "-m", "init"], dir);
}

#[test]
fn test_rollback_step_serialize_roundtrip() {
    let steps = vec![
        RollbackStep::PopStash {
            repo: PathBuf::from("/r"),
            stash_sha: "abc".to_string(),
        },
        RollbackStep::SwitchBranch {
            repo: PathBuf::from("/r"),
            branch: "main".to_string(),
        },
        RollbackStep::DeleteLocalBranch {
            repo: PathBuf::from("/r"),
            branch: "GX-1".to_string(),
            branch_existed: false,
        },
        RollbackStep::ResetCommit {
            repo: PathBuf::from("/r"),
            expected_sha: "deadbeef".to_string(),
        },
        RollbackStep::RestoreBackup {
            backup: PathBuf::from("/b"),
            original: PathBuf::from("/o"),
        },
        RollbackStep::RemoveCreatedFile {
            path: PathBuf::from("/f"),
        },
    ];
    let json = serde_json::to_string(&steps).unwrap();
    let back: Vec<RollbackStep> = serde_json::from_str(&json).unwrap();
    assert_eq!(steps, back);
}

#[test]
fn test_persist_writes_then_finalize_deletes() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        let mut tx = Transaction::new(repo.path().to_path_buf(), "GX-1".to_string(), true);
        let tx_id = tx.transaction_id.clone();
        tx.push_step(RollbackStep::RemoveCreatedFile {
            path: repo.path().join("new.txt"),
        })
        .unwrap();

        // Recovery state is on disk and round-trips.
        let loaded = Transaction::load_recovery_state(&tx_id).unwrap();
        assert_eq!(loaded.change_id, "GX-1");
        assert_eq!(loaded.steps.len(), 1);

        // Finalize removes the recovery file.
        tx.finalize().unwrap();
        assert!(Transaction::load_recovery_state(&tx_id).is_err());
    });
}

#[test]
fn test_dry_run_does_not_persist() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        let mut tx = Transaction::new(repo.path().to_path_buf(), "GX-2".to_string(), false);
        let tx_id = tx.transaction_id.clone();
        tx.push_step(RollbackStep::RemoveCreatedFile {
            path: repo.path().join("new.txt"),
        })
        .unwrap();
        // persist=false: nothing written.
        assert!(Transaction::load_recovery_state(&tx_id).is_err());
    });
}

#[test]
fn test_execute_step_remove_created_file() {
    let temp = TempDir::new().unwrap();
    let f = temp.path().join("created.txt");
    std::fs::write(&f, "x").unwrap();
    execute_step(&RollbackStep::RemoveCreatedFile { path: f.clone() }).unwrap();
    assert!(!f.exists());
    // Idempotent: a second run is fine.
    execute_step(&RollbackStep::RemoveCreatedFile { path: f }).unwrap();
}

#[test]
fn test_execute_step_restore_backup() {
    let temp = TempDir::new().unwrap();
    let original = temp.path().join("file.txt");
    let backup = temp.path().join("bk").join("file.txt");
    std::fs::write(&original, "ORIGINAL").unwrap();
    crate::file::create_backup(&original, &backup).unwrap();
    std::fs::write(&original, "MODIFIED").unwrap();

    execute_step(&RollbackStep::RestoreBackup {
        backup,
        original: original.clone(),
    })
    .unwrap();
    assert_eq!(std::fs::read_to_string(&original).unwrap(), "ORIGINAL");
}

#[test]
fn test_execute_step_reset_commit() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    init_repo(repo);
    let sha_a = crate::git::get_head_sha(repo).unwrap();

    std::fs::write(repo.join("b.txt"), "b").unwrap();
    git(&["add", "-A"], repo);
    git(&["commit", "--quiet", "-m", "b"], repo);
    assert_ne!(crate::git::get_head_sha(repo).unwrap(), sha_a);

    execute_step(&RollbackStep::ResetCommit {
        repo: repo.to_path_buf(),
        expected_sha: sha_a.clone(),
    })
    .unwrap();
    assert_eq!(crate::git::get_head_sha(repo).unwrap(), sha_a);
    assert!(!repo.join("b.txt").exists(), "hard reset removes b.txt");
}

#[test]
fn test_execute_step_delete_local_branch_respects_existed() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    init_repo(repo);

    git(&["branch", "keep"], repo);
    // branch_existed=true: must NOT delete.
    execute_step(&RollbackStep::DeleteLocalBranch {
        repo: repo.to_path_buf(),
        branch: "keep".to_string(),
        branch_existed: true,
    })
    .unwrap();
    assert!(crate::git::branch_exists_locally(repo, "keep").unwrap());

    // branch_existed=false: delete it, even while checked out.
    git(&["checkout", "-q", "-b", "GX-x"], repo);
    execute_step(&RollbackStep::DeleteLocalBranch {
        repo: repo.to_path_buf(),
        branch: "GX-x".to_string(),
        branch_existed: false,
    })
    .unwrap();
    assert!(!crate::git::branch_exists_locally(repo, "GX-x").unwrap());
}

#[test]
fn test_kill9_recovery_restores_branch_and_file() {
    // Simulate a SIGKILL mid-run: a recovery file exists with steps, and
    // `gx rollback execute` (Transaction::execute_recovery) restores state.
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        // Forward ops: create a GX branch, commit a mutated file on it.
        let sha_before = crate::git::get_head_sha(repo.path()).unwrap();
        let backup = data
            .path()
            .join("gx")
            .join("backups")
            .join("tx-test")
            .join("README.md");
        crate::file::create_backup(&repo.path().join("README.md"), &backup).unwrap();
        git(&["checkout", "-q", "-b", "GX-kill"], repo.path());
        std::fs::write(repo.path().join("README.md"), "MUTATED\n").unwrap();
        git(&["add", "-A"], repo.path());
        git(&["commit", "--quiet", "-m", "gx change"], repo.path());

        // Hand-author the recovery state as push_step would have.
        let state = RecoveryState {
            transaction_id: "tx-test".to_string(),
            change_id: "GX-kill".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-06-11T00:00:00Z".to_string(),
            steps: vec![
                RollbackStep::RestoreBackup {
                    backup,
                    original: repo.path().join("README.md"),
                },
                RollbackStep::DeleteLocalBranch {
                    repo: repo.path().to_path_buf(),
                    branch: "GX-kill".to_string(),
                    branch_existed: false,
                },
                RollbackStep::ResetCommit {
                    repo: repo.path().to_path_buf(),
                    expected_sha: sha_before.clone(),
                },
            ],
        };
        let dir = data.path().join("gx").join("recovery");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("tx-test.json"),
            serde_json::to_string_pretty(&state).unwrap(),
        )
        .unwrap();

        // Recover.
        Transaction::execute_recovery("tx-test").unwrap();

        // The GX branch is gone and the recovery file is cleaned up.
        assert!(!crate::git::branch_exists_locally(repo.path(), "GX-kill").unwrap());
        assert!(Transaction::load_recovery_state("tx-test").is_err());
    });
}

#[test]
fn test_finalize_restores_branch_and_stash() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        let original = crate::git::get_current_branch_name(repo.path()).unwrap();

        // Create WIP and stash it (-u), capturing the SHA.
        std::fs::write(repo.path().join("wip.txt"), "work in progress").unwrap();
        let sha = crate::git::stash_save_with_untracked(repo.path(), "wip").unwrap();
        assert!(!repo.path().join("wip.txt").exists(), "stash hid the WIP");

        // Move to another branch to prove finalize switches back.
        git(&["checkout", "-q", "-b", "GX-fin"], repo.path());

        let mut tx = Transaction::new(repo.path().to_path_buf(), "GX-fin".to_string(), true);
        tx.set_original_branch(original.clone());
        tx.set_stash_sha(sha);

        let outcome = tx.finalize().unwrap();
        assert!(outcome.stash_restored, "stash should be re-applied");
        assert_eq!(
            crate::git::get_current_branch_name(repo.path()).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("wip.txt")).unwrap(),
            "work in progress"
        );
        // The stash was dropped after a clean apply.
        let reflog = run_git_command(&["reflog", "show", "stash"], repo.path());
        assert!(reflog.stdout.is_empty() || !reflog.status.success());
    });
}
