use super::*;
use crate::test_utils::{run_git_command, ENV_LOCK};
use tempfile::TempDir;

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

/// Init `repo` with a bare `origin` remote at `bare`, push the initial branch,
/// and set `origin/HEAD`. Returns the default branch name (git's default varies
/// between `main`/`master`, so callers read it back rather than assume).
fn init_repo_with_bare_remote(repo: &Path, bare: &Path) -> String {
    let parent = bare.parent().unwrap();
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        parent,
    );
    init_repo(repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], repo);
    let branch = crate::git::get_current_branch_name(repo).unwrap();
    git(&["push", "--quiet", "-u", "origin", &branch], repo);
    git(&["remote", "set-head", "origin", &branch], repo);
    branch
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
            version: 1,
            transaction_id: "tx-test".to_string(),
            change_id: "GX-kill".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-06-11T00:00:00Z".to_string(),
            phase: Phase::Mutating,
            branch: Some("GX-kill".to_string()),
            steps: vec![
                StepEntry::pending(RollbackStep::RestoreBackup {
                    backup,
                    original: repo.path().join("README.md"),
                }),
                StepEntry::pending(RollbackStep::DeleteLocalBranch {
                    repo: repo.path().to_path_buf(),
                    branch: "GX-kill".to_string(),
                    branch_existed: false,
                }),
                StepEntry::pending(RollbackStep::ResetCommit {
                    repo: repo.path().to_path_buf(),
                    expected_sha: sha_before.clone(),
                }),
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

#[test]
fn test_step_entry_roundtrip_and_legacy_bare_steps() {
    // Journaled shape round-trips with status + error.
    let entry = StepEntry {
        step: RollbackStep::SwitchBranch {
            repo: PathBuf::from("/r"),
            branch: "main".to_string(),
        },
        status: StepStatus::Failed,
        error: Some("boom".to_string()),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let back: StepEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(entry, back);

    // A pre-journal recovery file stored bare `RollbackStep`s; those still load,
    // defaulting to Pending, so an upgrade never strands an in-flight recovery.
    let legacy = r#"[
        { "SwitchBranch": { "repo": "/r", "branch": "main" } },
        { "RemoveCreatedFile": { "path": "/f" } }
    ]"#;
    let steps: Vec<StepEntry> = serde_json::from_str(legacy).unwrap();
    assert_eq!(steps.len(), 2);
    assert!(steps.iter().all(|s| s.status == StepStatus::Pending));
    assert!(steps.iter().all(|s| s.error.is_none()));
}

/// Hand-write a recovery file for `tx_id` under the active data dir.
fn write_recovery_fixture(data: &Path, state: &RecoveryState) {
    let dir = data.join("gx").join("recovery");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{}.json", state.transaction_id)),
        serde_json::to_string_pretty(state).unwrap(),
    )
    .unwrap();
}

#[test]
fn test_rollback_retains_artifacts_on_failed_step() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        let tx_id = "tx-fail";
        let backups_base = data.path().join("gx").join("backups").join(tx_id);

        // Step A: a valid backup that restores cleanly.
        let a_original = repo.path().join("a.txt");
        std::fs::write(&a_original, "A-modified").unwrap();
        let a_backup = backups_base.join("a.txt");
        std::fs::create_dir_all(&backups_base).unwrap();
        std::fs::write(&a_backup, "A-orig").unwrap();

        // Step B: a backup path that does NOT exist yet -> restore fails.
        let b_original = repo.path().join("b.txt");
        let b_backup = backups_base.join("b.txt");

        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-fail".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Mutating,
            branch: None,
            steps: vec![
                StepEntry::pending(RollbackStep::RestoreBackup {
                    backup: a_backup.clone(),
                    original: a_original.clone(),
                }),
                StepEntry::pending(RollbackStep::RestoreBackup {
                    backup: b_backup.clone(),
                    original: b_original.clone(),
                }),
            ],
        };
        write_recovery_fixture(data.path(), &state);

        // First run: reverse order runs B (fails) then A (succeeds).
        let err = Transaction::execute_recovery(tx_id);
        assert!(err.is_err(), "a failed step must surface an error");

        // Evidence is retained: recovery file + backup dir survive.
        assert!(
            recovery_file(tx_id).unwrap().exists(),
            "recovery file must survive a failed step"
        );
        assert!(
            backups_base.exists(),
            "backup dir must survive a failed step"
        );

        // The journal recorded the per-step outcome.
        let loaded = Transaction::load_recovery_state(tx_id).unwrap();
        assert_eq!(loaded.steps[0].status, StepStatus::Done, "step A restored");
        assert_eq!(loaded.steps[1].status, StepStatus::Failed, "step B failed");
        assert!(loaded.steps[1].error.is_some());
        assert!(loaded.has_failed_steps());
        // Step A actually ran.
        assert_eq!(std::fs::read_to_string(&a_original).unwrap(), "A-orig");

        // Remove the failure: create B's backup so the retry converges.
        std::fs::write(&b_backup, "B-orig").unwrap();
        Transaction::execute_recovery(tx_id).unwrap();

        // Now everything is Done -> artifacts are cleaned up.
        assert!(
            !recovery_file(tx_id).unwrap().exists(),
            "recovery file removed after convergence"
        );
        assert!(
            !backups_base.exists(),
            "backup dir removed after convergence"
        );
        assert_eq!(std::fs::read_to_string(&b_original).unwrap(), "B-orig");
    });
}

#[test]
fn test_popstash_applied_state_skips_reapply() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        // Create WIP, stash it (-u), then apply it back to simulate the first
        // beat of a two-beat PopStash having completed (journal at `Applied`).
        std::fs::write(repo.path().join("wip.txt"), "v1").unwrap();
        let sha = crate::git::stash_save_with_untracked(repo.path(), "wip").unwrap();
        crate::git::stash_apply_sha(repo.path(), &sha).unwrap();
        // Mutate the applied file: a re-apply of the -u stash would now fail
        // (untracked file already exists), so an Ok result proves apply was
        // skipped.
        std::fs::write(repo.path().join("wip.txt"), "v2").unwrap();

        let tx_id = "tx-applied";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-applied".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Mutating,
            branch: None,
            steps: vec![StepEntry {
                step: RollbackStep::PopStash {
                    repo: repo.path().to_path_buf(),
                    stash_sha: sha.clone(),
                },
                status: StepStatus::Applied,
                error: None,
            }],
        };
        write_recovery_fixture(data.path(), &state);

        // Applied -> only the drop runs; the apply is skipped and does not error.
        Transaction::execute_recovery(tx_id).unwrap();

        // Recovery converged and cleaned up.
        assert!(!recovery_file(tx_id).unwrap().exists());
        // The stash was dropped.
        let list = run_git_command(&["stash", "list"], repo.path());
        assert!(
            String::from_utf8_lossy(&list.stdout).trim().is_empty(),
            "stash should be dropped"
        );
        // The applied file was left untouched (no re-apply clobbered "v2").
        assert_eq!(
            std::fs::read_to_string(repo.path().join("wip.txt")).unwrap(),
            "v2"
        );
    });
}

// ---- Phase 2: phase-stamped recovery, remote-safe execute ----

#[test]
fn test_recovery_state_defaults_for_versionless_file() {
    // A recovery file written before the version/phase/branch fields existed
    // must still load, with those fields defaulting.
    let json = r#"{
        "transaction_id": "tx-old",
        "change_id": "GX-old",
        "repo_path": "/tmp/r",
        "created_at": "2026-07-11T00:00:00Z",
        "steps": []
    }"#;
    let state: RecoveryState = serde_json::from_str(json).unwrap();
    assert_eq!(state.version, 1);
    assert_eq!(state.phase, Phase::Mutating);
    assert_eq!(state.branch, None);
}

#[test]
fn test_legacy_delete_remote_branch_alias_loads() {
    // A pre-rename recovery file serialized the step as `DeleteRemoteBranch`; the
    // serde alias must load it as the retired `LegacyDeleteRemoteBranch` variant.
    let json = r#"{
        "transaction_id": "tx-legacy",
        "change_id": "GX-legacy",
        "repo_path": "/tmp/r",
        "created_at": "2026-07-11T00:00:00Z",
        "steps": [
            { "step": { "DeleteRemoteBranch": { "repo": "/tmp/r", "branch": "GX-legacy" } }, "status": "pending" }
        ]
    }"#;
    let state: RecoveryState = serde_json::from_str(json).unwrap();
    assert!(matches!(
        state.steps[0].step,
        RollbackStep::LegacyDeleteRemoteBranch { .. }
    ));
}

#[test]
fn test_legacy_step_skipped_on_execute() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        let tx_id = "tx-legacy-exec";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-legacy".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Mutating,
            branch: Some("GX-legacy".to_string()),
            steps: vec![StepEntry::pending(RollbackStep::LegacyDeleteRemoteBranch {
                repo: repo.path().to_path_buf(),
                branch: "GX-legacy".to_string(),
            })],
        };
        write_recovery_fixture(data.path(), &state);

        // The retired step is a no-op marked skipped-legacy -> counts complete.
        let outcome = Transaction::execute_recovery(tx_id).unwrap();
        assert_eq!(outcome, RecoveryOutcome::FullReverse);
        assert!(
            !recovery_file(tx_id).unwrap().exists(),
            "skipped-legacy step converges and cleans up"
        );
    });
}

#[test]
fn test_popstash_by_message_restores_stash() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        // Simulate the F5 write-ahead window: a stash exists under a known
        // message, but the recovery step is still keyed by message (SHA not yet
        // swapped in). Recovery must resolve the message and restore the WIP.
        std::fs::write(repo.path().join("wip.txt"), "work").unwrap();
        let message = "GX auto-stash for GX-msg";
        crate::git::stash_save_with_untracked(repo.path(), message).unwrap();
        assert!(!repo.path().join("wip.txt").exists(), "stash hid the WIP");

        let tx_id = "tx-msg";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-msg".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Mutating,
            branch: None,
            steps: vec![StepEntry::pending(RollbackStep::PopStashByMessage {
                repo: repo.path().to_path_buf(),
                message: message.to_string(),
            })],
        };
        write_recovery_fixture(data.path(), &state);

        let outcome = Transaction::execute_recovery(tx_id).unwrap();
        assert_eq!(outcome, RecoveryOutcome::FullReverse);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("wip.txt")).unwrap(),
            "work"
        );
        let list = run_git_command(&["stash", "list"], repo.path());
        assert!(
            String::from_utf8_lossy(&list.stdout).trim().is_empty(),
            "stash should be dropped after restore"
        );
        assert!(!recovery_file(tx_id).unwrap().exists());
    });
}

#[test]
fn test_popstash_by_message_no_matching_stash_is_noop() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    with_data_home(data.path(), || {
        // Crash BEFORE the stash existed: the placeholder resolves to no stash
        // and converges as a harmless no-op.
        let tx_id = "tx-msg-none";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-msg".to_string(),
            repo_path: repo.path().to_path_buf(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Mutating,
            branch: None,
            steps: vec![StepEntry::pending(RollbackStep::PopStashByMessage {
                repo: repo.path().to_path_buf(),
                message: "GX auto-stash for GX-never".to_string(),
            })],
        };
        write_recovery_fixture(data.path(), &state);

        let outcome = Transaction::execute_recovery(tx_id).unwrap();
        assert_eq!(outcome, RecoveryOutcome::FullReverse);
        assert!(!recovery_file(tx_id).unwrap().exists());
    });
}

#[test]
fn test_execute_finalizing_phase_keeps_pushed_branch() {
    // Success criterion: a hand-authored `finalizing`-phase recovery file with a
    // pushed bare-remote fixture -> execute restores branch+stash and the remote
    // branch STILL EXISTS (rollback never mutates a remote).
    let data = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let repo = ws.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let bare = ws.path().join("repo.git");

    with_data_home(data.path(), || {
        let base = init_repo_with_bare_remote(&repo, &bare);
        let sha_before = crate::git::get_head_sha(&repo).unwrap();

        // WIP stashed on the base branch.
        std::fs::write(repo.join("wip.txt"), "work").unwrap();
        let stash_sha =
            crate::git::stash_save_with_untracked(&repo, "GX auto-stash for GX-fin").unwrap();
        assert!(!repo.join("wip.txt").exists());

        // Create the GX branch, commit a change, push it. The "crash" happened
        // entering finalize, so the process is still on GX-fin.
        git(&["checkout", "-q", "-b", "GX-fin"], &repo);
        std::fs::write(repo.join("README.md"), "changed\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "--quiet", "-m", "gx change"], &repo);
        git(&["push", "--quiet", "-u", "origin", "GX-fin"], &repo);
        assert!(crate::git::remote_branch_exists_probe(&repo, "GX-fin").unwrap());

        let tx_id = "tx-fin";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-fin".to_string(),
            repo_path: repo.clone(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Finalizing,
            branch: Some("GX-fin".to_string()),
            steps: vec![
                StepEntry::pending(RollbackStep::PopStash {
                    repo: repo.clone(),
                    stash_sha: stash_sha.clone(),
                }),
                StepEntry::pending(RollbackStep::SwitchBranch {
                    repo: repo.clone(),
                    branch: base.clone(),
                }),
                StepEntry::pending(RollbackStep::DeleteLocalBranch {
                    repo: repo.clone(),
                    branch: "GX-fin".to_string(),
                    branch_existed: false,
                }),
                StepEntry::pending(RollbackStep::ResetCommit {
                    repo: repo.clone(),
                    expected_sha: sha_before.clone(),
                }),
                StepEntry::pending(RollbackStep::LegacyDeleteRemoteBranch {
                    repo: repo.clone(),
                    branch: "GX-fin".to_string(),
                }),
            ],
        };
        write_recovery_fixture(data.path(), &state);

        let outcome = Transaction::execute_recovery(tx_id).unwrap();
        assert_eq!(
            outcome,
            RecoveryOutcome::KeepWork {
                branch: Some("GX-fin".to_string())
            }
        );

        // Environment restored: back on the base branch, WIP re-applied.
        assert_eq!(crate::git::get_current_branch_name(&repo).unwrap(), base);
        assert_eq!(
            std::fs::read_to_string(repo.join("wip.txt")).unwrap(),
            "work"
        );

        // The pushed branch STILL EXISTS, remote and local (keep-work retains it).
        assert!(
            crate::git::remote_branch_exists_probe(&repo, "GX-fin").unwrap(),
            "keep-work must retain the pushed remote branch"
        );
        assert!(crate::git::branch_exists_locally(&repo, "GX-fin").unwrap());

        // Keep-work mandate complete -> artifacts cleaned up.
        assert!(!recovery_file(tx_id).unwrap().exists());
    });
}

#[test]
fn test_execute_pushing_phase_no_remote_full_reverse() {
    // Success criterion: a `pushing`-phase file with NO remote branch -> the
    // probe dispatches a full reverse.
    let data = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let repo = ws.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let bare = ws.path().join("repo.git");

    with_data_home(data.path(), || {
        let base = init_repo_with_bare_remote(&repo, &bare);
        let sha_before = crate::git::get_head_sha(&repo).unwrap();

        // Create the GX branch and commit, but DO NOT push (the kill landed after
        // the `pushing` stamp but before the push reached the remote).
        git(&["checkout", "-q", "-b", "GX-push"], &repo);
        std::fs::write(repo.join("README.md"), "changed\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "--quiet", "-m", "gx change"], &repo);
        assert!(!crate::git::remote_branch_exists_probe(&repo, "GX-push").unwrap());

        let tx_id = "tx-push-absent";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-push".to_string(),
            repo_path: repo.clone(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Pushing,
            branch: Some("GX-push".to_string()),
            steps: vec![
                StepEntry::pending(RollbackStep::SwitchBranch {
                    repo: repo.clone(),
                    branch: base.clone(),
                }),
                StepEntry::pending(RollbackStep::DeleteLocalBranch {
                    repo: repo.clone(),
                    branch: "GX-push".to_string(),
                    branch_existed: false,
                }),
                StepEntry::pending(RollbackStep::ResetCommit {
                    repo: repo.clone(),
                    expected_sha: sha_before.clone(),
                }),
            ],
        };
        write_recovery_fixture(data.path(), &state);

        let outcome = Transaction::execute_recovery(tx_id).unwrap();
        assert_eq!(outcome, RecoveryOutcome::FullReverse);

        // Full reverse: the GX branch is gone locally and never appeared remotely.
        assert!(!crate::git::branch_exists_locally(&repo, "GX-push").unwrap());
        assert_eq!(crate::git::get_current_branch_name(&repo).unwrap(), base);
        assert!(!crate::git::remote_branch_exists_probe(&repo, "GX-push").unwrap());
        assert!(!recovery_file(tx_id).unwrap().exists());
    });
}

#[test]
fn test_execute_pushing_phase_with_remote_keeps_work() {
    // Success criterion: a `pushing`-phase file WITH the remote branch present ->
    // the probe dispatches keep-work.
    let data = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let repo = ws.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let bare = ws.path().join("repo.git");

    with_data_home(data.path(), || {
        let base = init_repo_with_bare_remote(&repo, &bare);
        let sha_before = crate::git::get_head_sha(&repo).unwrap();

        // Create the GX branch, commit, and push it: the push completed before
        // the crash, so the branch is present on the remote.
        git(&["checkout", "-q", "-b", "GX-push"], &repo);
        std::fs::write(repo.join("README.md"), "changed\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "--quiet", "-m", "gx change"], &repo);
        git(&["push", "--quiet", "-u", "origin", "GX-push"], &repo);
        assert!(crate::git::remote_branch_exists_probe(&repo, "GX-push").unwrap());

        let tx_id = "tx-push-present";
        let state = RecoveryState {
            version: 1,
            transaction_id: tx_id.to_string(),
            change_id: "GX-push".to_string(),
            repo_path: repo.clone(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            phase: Phase::Pushing,
            branch: Some("GX-push".to_string()),
            steps: vec![
                StepEntry::pending(RollbackStep::SwitchBranch {
                    repo: repo.clone(),
                    branch: base.clone(),
                }),
                StepEntry::pending(RollbackStep::DeleteLocalBranch {
                    repo: repo.clone(),
                    branch: "GX-push".to_string(),
                    branch_existed: false,
                }),
                StepEntry::pending(RollbackStep::ResetCommit {
                    repo: repo.clone(),
                    expected_sha: sha_before.clone(),
                }),
            ],
        };
        write_recovery_fixture(data.path(), &state);

        let outcome = Transaction::execute_recovery(tx_id).unwrap();
        assert_eq!(
            outcome,
            RecoveryOutcome::KeepWork {
                branch: Some("GX-push".to_string())
            }
        );

        // Environment restored, but the pushed work is retained (local + remote).
        assert_eq!(crate::git::get_current_branch_name(&repo).unwrap(), base);
        assert!(crate::git::branch_exists_locally(&repo, "GX-push").unwrap());
        assert!(crate::git::remote_branch_exists_probe(&repo, "GX-push").unwrap());
        assert!(!recovery_file(tx_id).unwrap().exists());
    });
}

#[test]
fn test_no_remote_mutation_reachable_from_rollback() {
    // Grep-proof: no code path from `rollback` reaches a remote-mutating git/gh
    // invocation. The rollback interpreter and CLI source must never reference
    // the remote-mutating helpers (`delete_remote_branch`, `push_branch`) or any
    // `github::` (gh) call.
    let root = env!("CARGO_MANIFEST_DIR");
    for file in ["src/transaction.rs", "src/rollback.rs"] {
        let src = std::fs::read_to_string(format!("{root}/{file}")).unwrap();
        assert!(
            !src.contains("delete_remote_branch("),
            "{file} must not call delete_remote_branch"
        );
        assert!(
            !src.contains("push_branch("),
            "{file} must not call push_branch"
        );
        assert!(
            !src.contains("github::"),
            "{file} must not invoke any github (gh) call"
        );
    }
}
