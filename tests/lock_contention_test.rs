//! Phase 7 [F6] success criterion: a two-process contention test using real
//! spawned `gx` binaries shows exactly ONE winner and ONE fast failure naming
//! the holder.
//!
//! `gx rollback execute` now takes the per-repo `RepoLock` for the duration of
//! its run (validate, confirm, execute). `GX_TEST_LOCK_DELAY_MS` (a Phase
//! 7-only, inert-unless-set test hook in `lock.rs`) lets this test create a
//! deterministic contention window: process A holds the lock for ~1s, process
//! B is spawned ~200ms in and must fail immediately rather than queue.

use gx::transaction::{Phase, RecoveryState, RollbackStep, StepEntry};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn gx_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("gx");
    path
}

/// Spawn `gx rollback execute <tx_id> --force --yes` against `data_home`,
/// optionally holding the lock for `delay_ms` after acquiring it.
fn spawn_rollback_execute(
    tx_id: &str,
    data_home: &Path,
    delay_ms: Option<u64>,
) -> std::process::Child {
    let mut cmd = Command::new(gx_binary());
    cmd.args(["rollback", "execute", tx_id, "--force", "--yes"])
        .env("XDG_DATA_HOME", data_home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(ms) = delay_ms {
        cmd.env("GX_TEST_LOCK_DELAY_MS", ms.to_string());
    }
    cmd.spawn().expect("failed to spawn gx rollback execute")
}

fn wait_with_output(child: std::process::Child) -> Output {
    child.wait_with_output().expect("child process failed")
}

/// Build a git repo plus a one-step `mutating` recovery whose `rollback
/// execute` reaches (and holds) the per-repo `RepoLock`. The step is a
/// `SwitchBranch` back to the already-checked-out `main` -- a harmless no-op;
/// the CONTENT doesn't matter, only that executing it takes the lock. Returns
/// the repo dir, the `$XDG_DATA_HOME` dir, and the transaction id.
fn setup_repo_and_recovery(tx_id: &str) -> (TempDir, TempDir, String) {
    let repo_dir = TempDir::new().unwrap();
    let repo = repo_dir.path();
    git(&["init", "--quiet", "-b", "main"], repo);
    git(&["config", "user.email", "t@e.com"], repo);
    git(&["config", "user.name", "T"], repo);
    git(&["config", "commit.gpgsign", "false"], repo);
    std::fs::write(repo.join("README.md"), "# r\n").unwrap();
    git(&["add", "-A"], repo);
    git(&["commit", "--quiet", "-m", "init"], repo);

    let data_home = TempDir::new().unwrap();
    let recovery = RecoveryState {
        version: 1,
        transaction_id: tx_id.to_string(),
        change_id: "GX-lock-contention".to_string(),
        repo_path: repo.to_path_buf(),
        created_at: "2026-07-11T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: Some("main".to_string()),
        steps: vec![StepEntry::pending(RollbackStep::SwitchBranch {
            repo: repo.to_path_buf(),
            branch: "main".to_string(),
        })],
    };
    let recovery_dir = data_home.path().join("gx").join("recovery");
    std::fs::create_dir_all(&recovery_dir).unwrap();
    std::fs::write(
        recovery_dir.join(format!("{tx_id}.json")),
        serde_json::to_string_pretty(&recovery).unwrap(),
    )
    .unwrap();

    (repo_dir, data_home, tx_id.to_string())
}

#[test]
fn two_processes_contend_for_one_repo_lock_exactly_one_wins() {
    let (_repo_dir, data_home, tx_id) = setup_repo_and_recovery("gx-tx-lock-contention-1");
    let tx_id = tx_id.as_str();

    // Process A: acquires the RepoLock, holds it ~1s, then executes.
    let a = spawn_rollback_execute(tx_id, data_home.path(), Some(1000));

    // Give A a comfortable head start well inside its 1s hold, then spawn B
    // against the SAME transaction (same repo_path -> same lock file).
    std::thread::sleep(Duration::from_millis(200));
    let b_start = Instant::now();
    let b = spawn_rollback_execute(tx_id, data_home.path(), None);
    let b_out = wait_with_output(b);
    let b_elapsed = b_start.elapsed();

    let a_out = wait_with_output(a);

    // Exactly one winner: A succeeds (it held the lock uncontested), B fails
    // fast (never even reaches validate/confirm/execute).
    assert!(
        a_out.status.success(),
        "process A (the lock holder) should succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&a_out.stdout),
        String::from_utf8_lossy(&a_out.stderr)
    );
    assert!(
        !b_out.status.success(),
        "process B must fail while A holds the lock"
    );

    let b_stderr = String::from_utf8_lossy(&b_out.stderr).to_lowercase();
    assert!(
        b_stderr.contains("locked") && b_stderr.contains("pid"),
        "B's failure must name the holder (pid): {b_stderr}"
    );

    // Fast failure, not a queued/blocked wait: well under A's 1s hold.
    assert!(
        b_elapsed < Duration::from_millis(800),
        "B's failure should be immediate, not blocked waiting for A: {b_elapsed:?}"
    );
}

#[test]
fn kill_9_holder_releases_lock_immediately_for_next_process() {
    // Success criterion: kill -9 the holder -> immediate reacquire by a new
    // process. The old scheme relied on pid-liveness reclaim to recover from a
    // dead holder; the OS flock releases automatically when the holding process
    // dies (even SIGKILL, which runs no Drop / cleanup), so a fresh process
    // acquires cleanly with no reclaim machinery.
    let (_repo_dir, data_home, tx_id) = setup_repo_and_recovery("gx-tx-lock-kill9-1");
    let tx_id = tx_id.as_str();

    // Process A holds the lock for a long time (5s), so if the lock were NOT
    // released on death, B would have to fail or block. We SIGKILL A mid-hold.
    let mut a = spawn_rollback_execute(tx_id, data_home.path(), Some(5000));

    // Let A get well past acquiring the lock (the hold delay runs right after
    // acquisition), then SIGKILL it -- no Drop, no explicit unlock runs.
    std::thread::sleep(Duration::from_millis(400));
    a.kill().expect("failed to SIGKILL process A"); // std Child::kill = SIGKILL on Unix
    a.wait().expect("failed to reap process A");

    // B, spawned after A is dead, must acquire the (kernel-released) lock and
    // run to success -- immediately, not after any 5s hold or reclaim wait.
    let b_start = Instant::now();
    let b = spawn_rollback_execute(tx_id, data_home.path(), None);
    let b_out = wait_with_output(b);
    let b_elapsed = b_start.elapsed();

    assert!(
        b_out.status.success(),
        "B must reacquire the dead holder's lock and succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&b_out.stdout),
        String::from_utf8_lossy(&b_out.stderr)
    );
    assert!(
        b_elapsed < Duration::from_millis(4000),
        "B should reacquire immediately after the SIGKILL, not wait out A's hold: {b_elapsed:?}"
    );
}
