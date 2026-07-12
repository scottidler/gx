use super::*;
use crate::transaction::{Phase, StepEntry};
use std::path::Path;
use tempfile::TempDir;

/// Point `XDG_DATA_HOME` at `dir` for the duration of `f`, serialized behind
/// the shared `ENV_LOCK` (env vars are process-global).
fn with_data_home<F: FnOnce()>(dir: &Path, f: F) {
    let guard = crate::test_utils::env_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
    f();
    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

/// Hand-author a live `mutating`-phase recovery file for `tx_id` with a
/// single `RemoveCreatedFile` step, as `push_step` would have written it.
fn write_mutating_recovery(data_home: &Path, tx_id: &str, path: &Path) {
    let state = RecoveryState {
        version: 1,
        transaction_id: tx_id.to_string(),
        change_id: "GX-core-test".to_string(),
        repo_path: path.parent().unwrap().to_path_buf(),
        created_at: "2026-07-12T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: None,
        steps: vec![StepEntry::pending(
            crate::transaction::RollbackStep::RemoveCreatedFile {
                path: path.to_path_buf(),
            },
        )],
    };
    let dir = data_home.join("gx").join("recovery");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{tx_id}.json")),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
}

#[test]
fn test_execute_recovery_runs_engine_and_removes_artifacts() {
    // Happy path: execute_recovery (core) runs the real recovery engine and
    // returns its outcome, never printing or prompting itself - the
    // confirmation was already obtained by the caller.
    let data_home = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let created = repo.path().join("created-by-gx.txt");
    std::fs::write(&created, "gx wrote this").unwrap();

    with_data_home(data_home.path(), || {
        write_mutating_recovery(data_home.path(), "tx-core-happy", &created);

        let outcome = execute_recovery("tx-core-happy", Confirmation::AlreadyConfirmed)
            .expect("recovery should succeed");

        assert!(matches!(outcome, RecoveryOutcome::FullReverse));
        assert!(!created.exists(), "the created file must be removed");
        assert!(
            Transaction::load_recovery_state("tx-core-happy").is_err(),
            "the recovery file must be cleaned up on full reverse"
        );
    });
}

#[test]
fn test_execute_recovery_errors_for_unknown_transaction() {
    // Edge case: a transaction id with no recovery file is a loud error, not
    // a silent no-op.
    let data_home = TempDir::new().unwrap();
    with_data_home(data_home.path(), || {
        let result = execute_recovery("tx-does-not-exist", Confirmation::AlreadyConfirmed);
        assert!(result.is_err());
    });
}

#[test]
fn test_validate_recovery_state_flags_missing_repo_path() {
    let state = RecoveryState {
        version: 1,
        transaction_id: "tx-missing".to_string(),
        change_id: "GX-missing".to_string(),
        repo_path: std::path::PathBuf::from("/nonexistent/gx-rollback-core-test"),
        created_at: "2026-07-12T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: None,
        steps: vec![],
    };

    let (errors, warnings) = validate_recovery_state(&state);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("no longer exists"));
    assert_eq!(warnings.len(), 1, "no steps should also warn");
}

#[test]
fn test_validate_recovery_state_passes_for_real_git_repo() {
    let repo = TempDir::new().unwrap();
    crate::test_utils::run_git_command(&["init", "--quiet"], repo.path());

    let state = RecoveryState {
        version: 1,
        transaction_id: "tx-ok".to_string(),
        change_id: "GX-ok".to_string(),
        repo_path: repo.path().to_path_buf(),
        created_at: "2026-07-12T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: None,
        steps: vec![StepEntry::pending(
            crate::transaction::RollbackStep::RemoveCreatedFile {
                path: repo.path().join("whatever.txt"),
            },
        )],
    };

    let (errors, warnings) = validate_recovery_state(&state);
    assert!(errors.is_empty());
    assert!(warnings.is_empty());
}
