use super::*;
use crate::state::RepoChangeStatus;
use crate::test_utils::run_git_command;
use std::fs;
use tempfile::TempDir;

/// Initialize a git repo and commit all current files (fail-loud).
fn init_git_repo(repo_path: &Path) {
    let init = run_git_command(&["init", "--quiet"], repo_path);
    assert!(init.status.success(), "git init failed");
    run_git_command(&["config", "user.email", "test@example.com"], repo_path);
    run_git_command(&["config", "user.name", "Test User"], repo_path);
    run_git_command(&["config", "commit.gpgsign", "false"], repo_path);
    let add = run_git_command(&["add", "-A"], repo_path);
    assert!(add.status.success(), "git add failed");
    let commit = run_git_command(&["commit", "--quiet", "-m", "init"], repo_path);
    assert!(commit.status.success(), "git commit failed");
}

#[test]
fn test_generate_change_id() {
    let change_id = generate_change_id();
    assert!(change_id.starts_with("GX-"));
    assert!(change_id.len() > 10); // Should have timestamp
}

#[test]
fn test_process_single_repo_hard_errors_on_head_branch_failure() {
    // F10: a `get_head_branch` failure must surface as a hard per-repo
    // error, not be silently swallowed (which would leave the repo on
    // whatever branch the user happened to be on).
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path().to_path_buf();
    fs::write(repo_path.join("README.md"), "# repo").unwrap();
    init_git_repo(&repo_path);
    // No `origin` remote: get_head_branch() can neither read
    // origin/HEAD nor confirm main/master exist remotely, so it errors.
    let repo = Repo::new(repo_path).unwrap();

    let result = process_single_repo(
        &repo,
        "GX-test",
        &["**/*.md".to_string()],
        &Change::Delete,
        None,
        false,
        false,
        &Config::default(),
        None,
        None,
    );

    assert!(
        result.error.is_some(),
        "a get_head_branch failure must be a hard error, not swallowed"
    );
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("determine head branch"),
        "error should name the head-branch failure, got: {:?}",
        result.error
    );
}

#[test]
fn test_apply_add_change() {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();
    let mut transaction = Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
    let mut files_affected = Vec::new();
    let mut diff_parts = Vec::new();

    let result = apply_add_change(
        repo_path,
        "new_file.txt",
        "Hello, world!",
        &mut transaction,
        &mut files_affected,
        &mut diff_parts,
    );

    assert!(result.is_ok());
    assert_eq!(files_affected.len(), 1);
    assert_eq!(files_affected[0], "new_file.txt");
    assert_eq!(diff_parts.len(), 1);
    assert!(repo_path.join("new_file.txt").exists());

    // Test rollback
    transaction.rollback();
    assert!(!repo_path.join("new_file.txt").exists());
}

#[test]
fn test_apply_add_change_file_exists() {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path();
    let file_path = repo_path.join("existing.txt");
    fs::write(&file_path, "existing content").unwrap();

    let mut transaction = Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
    let mut files_affected = Vec::new();
    let mut diff_parts = Vec::new();

    let result = apply_add_change(
        repo_path,
        "existing.txt",
        "new content",
        &mut transaction,
        &mut files_affected,
        &mut diff_parts,
    );

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("File already exists"));
}

#[test]
fn test_apply_delete_change() {
    // XDG-isolated (Phase 5 flock-fix): `apply_delete_change` writes an
    // out-of-tree backup via `Transaction::backup_path_for`, which resolves
    // `$XDG_DATA_HOME` UNCONDITIONALLY (regardless of `persist`). Left
    // unpinned, this test's backup write can land inside some OTHER
    // concurrently-running test's transient `XDG_DATA_HOME` TempDir; when that
    // other test finishes and its TempDir drops, our backup file is deleted
    // out from under us and the rollback restore silently finds nothing --
    // exactly the `create::core::tests::test_apply_delete_change` flake this
    // phase reproduced under parallel `cargo test`.
    let data_home = TempDir::new().unwrap();
    with_data_home(data_home.path(), || {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create test files
        fs::write(repo_path.join("file1.txt"), "content1").unwrap();
        fs::write(repo_path.join("file2.txt"), "content2").unwrap();
        fs::write(repo_path.join("file3.md"), "markdown").unwrap();
        init_git_repo(repo_path);

        let mut transaction =
            Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
        let mut files_affected = Vec::new();
        let mut diff_parts = Vec::new();
        let patterns = vec!["*.txt".to_string()];

        let result = apply_delete_change(
            repo_path,
            &patterns,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        assert!(result.is_ok());
        assert_eq!(files_affected.len(), 2);
        assert!(!repo_path.join("file1.txt").exists());
        assert!(!repo_path.join("file2.txt").exists());
        assert!(repo_path.join("file3.md").exists()); // Should not be deleted

        // Test rollback
        transaction.rollback();
        assert!(repo_path.join("file1.txt").exists());
        assert!(repo_path.join("file2.txt").exists());
    });
}

#[test]
fn test_apply_substitution_change() {
    // XDG-isolated (Phase 5 flock-fix): see `test_apply_delete_change` above --
    // `apply_substitution_change` takes the same unconditional out-of-tree
    // backup path.
    let data_home = TempDir::new().unwrap();
    with_data_home(data_home.path(), || {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create test file
        fs::write(repo_path.join("test.txt"), "Hello world\nHello again").unwrap();
        init_git_repo(repo_path);

        let mut transaction =
            Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
        let mut files_affected = Vec::new();
        let mut diff_parts = Vec::new();
        let patterns = vec!["*.txt".to_string()];

        let result = apply_substitution_change(
            repo_path,
            &patterns,
            "Hello",
            "Hi",
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        assert!(result.is_ok());
        assert_eq!(files_affected.len(), 1);

        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "Hi world\nHi again");

        // Test rollback
        transaction.rollback();
        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "Hello world\nHello again");
    });
}

// ---- Phase 4: pushed-state safe point (F12) ----

/// Init `repo` with a bare `origin` remote at `bare`, push the initial
/// branch, and set `origin/HEAD`. Returns the default branch name.
fn init_repo_with_bare_remote(repo: &Path, bare: &Path) -> String {
    let parent = bare.parent().unwrap();
    run_git_command(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        parent,
    );
    fs::create_dir_all(repo).unwrap();
    fs::write(repo.join("README.md"), "# repo\n").unwrap();
    init_git_repo(repo);
    run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], repo);
    let branch = crate::git::get_current_branch_name(repo).unwrap();
    run_git_command(&["push", "--quiet", "-u", "origin", &branch], repo);
    run_git_command(&["remote", "set-head", "origin", &branch], repo);
    branch
}

/// Point `XDG_DATA_HOME` at `dir` for the duration of `f`, serialized
/// behind the shared `ENV_LOCK` (env vars are process-global).
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

#[test]
fn test_pushed_state_recorded_before_finalize_deletes_recovery() {
    // F12, "state-saved-first" order: the pushed safe-point save happens
    // BEFORE finalize() runs (finalize deletes the recovery file). A crash
    // any time after the save - even after finalize already cleaned up the
    // recovery file - still leaves the pushed branch recorded, because
    // state landed first.
    let data_home = TempDir::new().unwrap();
    with_data_home(data_home.path(), || {
        let ws = TempDir::new().unwrap();
        let repo_path = ws.path().join("repo");
        let bare = ws.path().join("repo.git");
        let branch = init_repo_with_bare_remote(&repo_path, &bare);
        fs::write(repo_path.join("README.md"), "# repo\nupdated\n").unwrap();

        let change_id = "GX-safepoint";
        let mut transaction = Transaction::new(repo_path.clone(), change_id.to_string(), true);
        let base_sha = commit_changes_with_rollback(
            &repo_path,
            change_id,
            "test commit",
            &["README.md".to_string()],
            &mut transaction,
        )
        .expect("commit+push should succeed");

        let repo = Repo::new(repo_path.clone()).unwrap();
        let change_state = Mutex::new(ChangeState::new(change_id.to_string(), None));
        let state_manager = StateManager::new().unwrap();

        let saved = record_pushed_state(
            Some(&change_state),
            Some(&state_manager),
            &repo,
            change_id,
            &branch,
            &["README.md".to_string()],
            &base_sha,
        );
        assert!(saved, "a successful save must report durably saved (true)");

        // Simulate the run continuing to finalize (which deletes the
        // recovery file) - the state save already happened, so it survives
        // regardless of what happens to the recovery file next.
        transaction.finalize().expect("finalize should succeed");

        let recoveries = Transaction::list_recovery_states().unwrap();
        assert!(
            recoveries.iter().all(|r| r.repo_path != repo_path),
            "finalize should have removed the recovery file"
        );

        let loaded = state_manager
            .load(change_id)
            .unwrap()
            .expect("change state must have been saved");
        let repo_state = loaded
            .repositories
            .get(&repo.slug)
            .expect("repo must be recorded");
        assert_eq!(repo_state.branch_name, change_id);
        assert_eq!(repo_state.base_sha.as_deref(), Some(base_sha.as_str()));
    });
}

#[test]
fn test_pushed_branch_recorded_via_recovery_when_state_save_not_reached() {
    // F12, "recovery-only" order: if the process dies between the pushed
    // phase stamp and the pushed safe-point save (never reached), the
    // recovery file - stamped write-ahead BEFORE the push ran - still
    // records the branch on its own.
    let data_home = TempDir::new().unwrap();
    with_data_home(data_home.path(), || {
        let ws = TempDir::new().unwrap();
        let repo_path = ws.path().join("repo");
        let bare = ws.path().join("repo.git");
        init_repo_with_bare_remote(&repo_path, &bare);
        fs::write(repo_path.join("README.md"), "# repo\nupdated\n").unwrap();

        let change_id = "GX-recoveryonly";
        let mut transaction = Transaction::new(repo_path.clone(), change_id.to_string(), true);
        commit_changes_with_rollback(
            &repo_path,
            change_id,
            "test commit",
            &["README.md".to_string()],
            &mut transaction,
        )
        .expect("commit+push should succeed");

        // Simulate a crash right here: record_pushed_state is never
        // called, and finalize() never runs.
        let recoveries = Transaction::list_recovery_states().unwrap();
        let recorded = recoveries
            .iter()
            .find(|r| r.repo_path == repo_path)
            .expect("recovery file must exist for the pushed branch");
        assert_eq!(recorded.phase, crate::transaction::Phase::Pushed);
        assert_eq!(recorded.branch.as_deref(), Some(change_id));

        // No change state was ever saved for this change id.
        let state_manager = StateManager::new().unwrap();
        assert!(state_manager.load(change_id).unwrap().is_none());
    });
}

#[test]
fn test_process_single_repo_records_state_with_base_sha() {
    // End-to-end (Phase 4 control-flow refactor): process_single_repo
    // itself - not just the lower-level helpers above - saves state with
    // base_sha via the Mutex<ChangeState>/StateManager now threaded in.
    let data_home = TempDir::new().unwrap();
    with_data_home(data_home.path(), || {
        let ws = TempDir::new().unwrap();
        let repo_path = ws.path().join("repo");
        let bare = ws.path().join("repo.git");
        init_repo_with_bare_remote(&repo_path, &bare);
        fs::write(repo_path.join("file1.txt"), "content1").unwrap();
        run_git_command(&["add", "-A"], &repo_path);
        run_git_command(&["commit", "--quiet", "-m", "add file1"], &repo_path);
        run_git_command(&["push", "--quiet"], &repo_path);

        let repo = Repo::new(repo_path.clone()).unwrap();
        let change_id = "GX-e2e-state";
        let change_state = Mutex::new(ChangeState::new(
            change_id.to_string(),
            Some("test".to_string()),
        ));
        let state_manager = StateManager::new().unwrap();

        let result = process_single_repo(
            &repo,
            change_id,
            &["file1.txt".to_string()],
            &Change::Delete,
            Some("delete file1"),
            false,
            false,
            &Config::default(),
            Some(&change_state),
            Some(&state_manager),
        );

        assert!(
            result.error.is_none(),
            "expected success, got: {:?}",
            result.error
        );
        assert!(result.base_sha.is_some());

        let loaded = state_manager
            .load(change_id)
            .unwrap()
            .expect("change state must have been saved");
        let repo_state = loaded
            .repositories
            .get(&repo.slug)
            .expect("repo must be recorded");
        assert_eq!(repo_state.base_sha, result.base_sha);
        assert_eq!(repo_state.status, RepoChangeStatus::BranchCreated);
    });
}

// ---- Phase 3: diff surfaced on CreateResult (previously computed and
// discarded); execute_create orchestration + the Confirmation seam ----

#[test]
fn test_apply_add_change_surfaces_diff_on_dry_run_result() {
    // The diff computed by apply_add_change must ride the returned
    // CreateResult (design doc Phase 3), not be discarded. process_single_repo
    // needs an `origin` remote to resolve the head branch (get_head_branch),
    // so use the same bare-remote fixture as the Phase 4 tests above.
    let ws = TempDir::new().unwrap();
    let repo_path = ws.path().join("repo");
    let bare = ws.path().join("repo.git");
    init_repo_with_bare_remote(&repo_path, &bare);
    let repo = Repo::new(repo_path).unwrap();

    let result = process_single_repo(
        &repo,
        "GX-diff",
        &[],
        &Change::Add("new.txt".to_string(), "hello\n".to_string()),
        None, // dry run: no commit_message
        false,
        false,
        &Config::default(),
        None,
        None,
    );

    assert!(
        result.error.is_none(),
        "expected success: {:?}",
        result.error
    );
    let diff = result.diff.expect("dry-run add must surface its diff");
    assert!(
        diff.contains("A new.txt"),
        "diff should name the file: {diff}"
    );
    assert!(
        diff.contains("hello"),
        "diff should contain the new content: {diff}"
    );
}

#[test]
fn test_dry_run_error_reports_no_diff_before_any_mutation() {
    // An error before any file was touched (RepoLock unavailable, detached
    // HEAD, ...) must report `diff: None`, not fabricate one.
    let result = dry_run_error(
        &Repo::from_slug("org/repo".to_string()),
        "GX-none",
        "boom".to_string(),
        &[],
    );
    assert!(result.diff.is_none());
    assert_eq!(result.error.as_deref(), Some("boom"));
}

#[test]
fn test_execute_create_dry_run_returns_result_per_repo() {
    // Happy path: execute_create orchestrates discovery-independent,
    // pre-filtered repos through process_single_repo and returns one
    // CreateResult per repo, with the caller's AlreadyConfirmed threaded
    // through (never prompted for internally - this fn never touches stdin).
    // An `origin` remote is required for get_head_branch to resolve.
    let ws = TempDir::new().unwrap();
    let repo_path = ws.path().join("repo");
    let bare = ws.path().join("repo.git");
    init_repo_with_bare_remote(&repo_path, &bare);
    let repo = Repo::new(repo_path).unwrap();

    let results = execute_create(
        std::slice::from_ref(&repo),
        "GX-exec",
        &["*.md".to_string()],
        &Change::Sub("repo".to_string(), "REPO".to_string()),
        None, // dry run
        false,
        false,
        &Config::default(),
        1,
        Confirmation::AlreadyConfirmed,
    )
    .expect("execute_create should not hard-error on a dry run");

    assert_eq!(results.len(), 1);
    assert!(
        results[0].error.is_none(),
        "unexpected error: {:?}",
        results[0].error
    );
    assert!(matches!(results[0].action, CreateAction::DryRun));
}

#[test]
fn test_execute_create_accepts_token_confirmation_with_no_repos() {
    // Edge case: zero repos (the wrapper already filtered to nothing worth
    // showing a prompt for) is a trivial success, not an error - and a
    // Token confirmation (the shape a future MCP `create-apply` call uses
    // once a plan/proposal manifest exists to hash) is accepted identically
    // to AlreadyConfirmed; neither variant is interpreted differently by
    // this core in Phase 3.
    let results = execute_create(
        &[],
        "GX-empty",
        &[],
        &Change::Delete,
        None,
        false,
        false,
        &Config::default(),
        1,
        Confirmation::Token("deadbeef".to_string()),
    )
    .expect("execute_create should succeed trivially with zero repos");

    assert!(results.is_empty());
}
