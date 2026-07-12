//! F12 fail-closed matrix (post-audit hardening): prove the two double-fault
//! seams the implementation audit found in the create path, end to end, with a
//! REAL `gx create --commit` against a bare remote.
//!
//! The F12 guarantee is: a pushed branch is ALWAYS recorded in state OR
//! recovery, never neither. Two double faults threatened it:
//!
//! - (a) `StateManager::new()` failing in commit mode -- a run with no durable
//!   state store cannot honor F12, so it must ABORT before mutating/pushing any
//!   repo (not downgrade to a best-effort `None`).
//! - (b) the pushed safe-point state save failing -- the recovery file must be
//!   RETAINED (not deleted by finalize), so the pushed branch is still recorded
//!   in recovery. Invariant: recovery file deleted => state contains this repo.

use std::path::Path;
use std::process::Command;
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

/// A dirty repo at `workspace/app` with a bare remote at `remotes/app.git`,
/// containing a tracked `data.md` ("old value") and an UNTRACKED `wip.txt` so
/// the run stashes WIP (proving the stash is restored on the retain path).
#[cfg(unix)]
fn make_fixture(workspace: &Path, remotes: &Path) -> std::path::PathBuf {
    let bare = remotes.join("app.git");
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );

    let repo = workspace.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &repo);
    git(&["config", "user.email", "t@e.com"], &repo);
    git(&["config", "user.name", "T"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);

    std::fs::write(repo.join("data.md"), "old value\n").unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);

    // Uncommitted WIP so the create pipeline stashes it.
    std::fs::write(repo.join("wip.txt"), "WIP\n").unwrap();
    repo
}

/// True if `branch` exists on the bare remote at `remotes/app.git`.
fn branch_on_bare(remotes: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            remotes.join("app.git").to_str().unwrap(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}

/// True if `branch` exists locally in `repo`.
fn branch_local(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

/// The number of recovery JSON files under `data_home/gx/recovery`.
fn recovery_file_count(data_home: &Path) -> usize {
    let dir = data_home.join("gx").join("recovery");
    match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .count(),
        Err(_) => 0,
    }
}

fn create_args<'a>(workspace: &'a str, change_id: &'a str) -> Vec<&'a str> {
    vec![
        "--cwd",
        workspace,
        "--log-level",
        "off",
        "create",
        "--files",
        "**/*.md",
        "--change-id",
        change_id,
        "--commit",
        "f12: old to new",
        "--yes",
        "sub",
        "old",
        "new",
    ]
}

/// (a) A committing run whose state store is unavailable must ABORT before it
/// mutates or pushes any repo.
///
/// StateManager::new() is failed in ISOLATION: `gx/` is a real directory (so the
/// change-lock dir `gx/locks` and the log dir `gx/logs` still create fine), but
/// `gx/changes` is a regular FILE, so `StateManager::new()`'s
/// `create_dir_all(gx/changes)` fails -- and nothing else early does.
///
/// Break-the-code proof: reverting the guard to the pre-fix `warn!` + `None`
/// lets the run proceed and push, so `branch_on_bare` becomes true and the
/// "must NOT push" assertion fails.
#[test]
#[cfg(unix)]
fn test_commit_aborts_before_push_when_state_store_unavailable() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    let change_id = "GX-f12-abort";

    let gx = data_home.path().join("gx");
    std::fs::create_dir_all(&gx).unwrap();
    std::fs::write(gx.join("changes"), b"not a directory").unwrap();

    let out = Command::new(gx_binary())
        .args(create_args(workspace.path().to_str().unwrap(), change_id))
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx create failed to spawn");

    assert!(
        !out.status.success(),
        "committing run must abort when the state store is unavailable"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("state store"),
        "error must name the unavailable state store; got:\n{combined}"
    );

    // Fail-closed proof: nothing was pushed, no GX branch created, data untouched.
    assert!(
        !branch_on_bare(remotes.path(), change_id),
        "must NOT push before aborting"
    );
    assert!(
        !branch_local(&repo, change_id),
        "must NOT create the local GX branch before aborting"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("data.md")).unwrap(),
        "old value\n",
        "working tree must be untouched"
    );
}

/// (b) When the pushed safe-point save fails, the recovery file is RETAINED and
/// the repo is reported Committed with an error naming the retained file.
///
/// `GX_TEST_FAIL_STATE_SAVE` fails every `StateManager::save` AFTER the real
/// push, so the pushed safe-point save fails and the create path takes the
/// retain-recovery branch instead of finalize (which deletes the file).
///
/// Break-the-code proof: reverting the retain branch to an unconditional
/// `finalize()` deletes the recovery file, so `recovery_file_count == 1` fails
/// (it becomes 0) -- proving the retain is load-bearing for F12.
#[test]
#[cfg(unix)]
fn test_pushed_save_failure_retains_recovery_and_reports_committed() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    let change_id = "GX-f12-retain";

    let out = Command::new(gx_binary())
        .args(create_args(workspace.path().to_str().unwrap(), change_id))
        .env("XDG_DATA_HOME", data_home.path())
        .env("GX_TEST_FAIL_STATE_SAVE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx create failed to spawn");

    // The run completes (the repo is a per-repo Committed-with-error), exit 0.
    assert!(
        out.status.success(),
        "create should complete and display results: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("RETAINED"),
        "must report the retained recovery file; got:\n{combined}"
    );
    assert!(
        combined.contains(change_id),
        "retained-recovery report must name the change; got:\n{combined}"
    );

    // Recovery file RETAINED: exactly one recovery json survives.
    assert_eq!(
        recovery_file_count(data_home.path()),
        1,
        "the recovery file must be retained when the safe-point save fails"
    );

    // The invariant's contrapositive: the state store does NOT contain this
    // change (its save failed), which is exactly WHY recovery is retained.
    let state_file = data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    assert!(
        !state_file.exists(),
        "the state save failed, so no state file should exist for {change_id}"
    );

    // The pushed work is KEPT (never reversed by the create path): the remote
    // and local GX branch both survive; `gx undo` owns reversing shared work.
    assert!(
        branch_on_bare(remotes.path(), change_id),
        "the pushed branch must be retained on the remote"
    );
    assert!(
        branch_local(&repo, change_id),
        "the local GX branch must be retained"
    );

    // Environment restored: back on the base branch with WIP re-applied and the
    // base branch's content unchanged (the sub landed only on the GX branch).
    assert_eq!(
        std::fs::read_to_string(repo.join("data.md")).unwrap(),
        "old value\n",
        "the base branch's working tree must be restored"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("wip.txt")).unwrap(),
        "WIP\n",
        "the stashed WIP must be re-applied on the retain path"
    );
}
