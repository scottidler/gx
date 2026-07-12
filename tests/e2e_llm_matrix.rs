//! Phase 7 end-to-end failure-mode matrix: drives the REAL `gx` binary
//! through the two `llm` entry points (`gx create ... llm "<prompt>"
//! [--propose]` and `gx apply <change-id> [--yes]`) with a deterministic
//! fake-agent script (no live LLM), matching the fake-agent pattern Phases 4-6
//! established (`tests/e2e_llm_apply.rs`, `tests/e2e_llm_cli.rs`).
//!
//! Scenarios: happy path, garbage patch (an unusable payload-matrix
//! rejection - a symlink), agent nonzero exit, agent timeout, empty diff,
//! and drift-then-refuse. Every failure-mode scenario asserts the real
//! worktree is BYTE-IDENTICAL after the failure (design success criterion).
//!
//! NOT duplicated here (already proven and left in place, per Phases 5/6):
//! - `GX_CRASH_POINT` injection at every apply phase stamp on the happy path
//!   -> `tests/e2e_llm_apply.rs::test_apply_crash_matrix_parity_with_sub`.
//! - `gx undo` after an applied campaign (undo-after-apply) ->
//!   `tests/e2e_llm_apply.rs::test_undo_reverses_applied_llm_campaign_and_removes_proposal`.
//! - The split-vs-one-shot flow equivalence ->
//!   `tests/e2e_llm_cli.rs::test_split_propose_then_apply_equals_one_shot`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
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

fn git_stdout(args: &[&str], dir: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).to_string()
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

/// A repo at `workspace/app` on `main` with a bare remote at `remotes/app.git`,
/// tracking `data.md`. `-p app` matches exactly one repo (under the default
/// `confirm-threshold`), so the up-front blast-radius gate auto-proceeds and
/// every test here isolates its OWN scenario under confirm gate #5 (`--yes`).
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
    repo
}

/// Write an executable fake-agent script; `body` runs with CWD = the temp
/// worktree.
fn write_agent(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let agent = dir.join(name);
    std::fs::write(&agent, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    agent
}

/// A gx config pointing `create.llm.agent-command` at `agent`, with `timeout`
/// seconds.
fn write_config(dir: &Path, agent: &Path, timeout: u64) -> std::path::PathBuf {
    let cfg = dir.join("gx.yml");
    std::fs::write(
        &cfg,
        format!(
            "create:\n  llm:\n    agent-command: \"{}\"\n    timeout-seconds: {timeout}\n",
            agent.display()
        ),
    )
    .unwrap();
    cfg
}

/// Byte-identity snapshot of the REAL worktree: HEAD, sorted porcelain status,
/// tracked-file listing, and `data.md`'s bytes (mirrors the crash-matrix e2e's
/// `worktree_snapshot`).
fn worktree_snapshot(repo: &Path) -> String {
    let head = git_stdout(&["rev-parse", "HEAD"], repo);
    let mut porcelain: Vec<String> = git_stdout(&["status", "--porcelain"], repo)
        .lines()
        .map(|l| l.to_string())
        .collect();
    porcelain.sort();
    let tracked = git_stdout(&["ls-files", "-s"], repo);
    let data = std::fs::read_to_string(repo.join("data.md")).unwrap_or_default();
    format!(
        "HEAD={head}\nPORCELAIN=\n{}\nTRACKED=\n{tracked}\nDATA={data}",
        porcelain.join("\n")
    )
}

fn branch_local(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

/// `gx create -p app --change-id <id> --yes llm "<prompt>" --propose`,
/// asserting only that it SPAWNED (callers assert exit status/stdout
/// themselves, since a failure mode may still exit 0 with a per-repo
/// `failed` outcome - the design's "loud per-repo error, not a process
/// failure" contract).
fn run_propose(
    workspace: &Path,
    cfg: &Path,
    data_home: &Path,
    change_id: &str,
    prompt: &str,
) -> std::process::Output {
    Command::new(gx_binary())
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--cwd",
            workspace.to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "-p",
            "app",
            "--change-id",
            change_id,
            "--yes",
            "llm",
            prompt,
            "--propose",
        ])
        .env("XDG_DATA_HOME", data_home)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx propose failed to spawn")
}

#[test]
fn test_happy_path_one_shot_pushes_the_agent_change() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(
        scripts.path(),
        "agent.sh",
        "printf 'new value\\n' > data.md",
    );
    let cfg = write_config(scripts.path(), &agent, 60);
    let change_id = "GX-matrix-happy";

    let out = Command::new(gx_binary())
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "-p",
            "app",
            "--change-id",
            change_id,
            "--yes",
            "llm",
            "make data.md say new value",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx create llm failed to spawn");
    assert!(
        out.status.success(),
        "happy path must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(branch_local(&workspace.path().join("app"), change_id));
}

#[test]
fn test_garbage_patch_symlink_is_rejected_and_worktree_untouched() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    // An agent that produces something gx can never turn into a deterministic
    // patchset (design: "the diff is for humans, the blobs are for apply";
    // gx never reimplements `patch`) - a symlink, rejected by the
    // payload-fidelity matrix at propose.
    let agent = write_agent(scripts.path(), "agent.sh", "ln -s data.md link.txt");
    let cfg = write_config(scripts.path(), &agent, 60);
    let before = worktree_snapshot(&repo);

    let out = run_propose(
        workspace.path(),
        &cfg,
        data_home.path(),
        "GX-matrix-garbage",
        "make a symlink",
    );
    assert!(
        out.status.success(),
        "a per-repo rejection is a LOUD per-repo error, not a process failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAILED") && stdout.contains("symlink"),
        "stdout must name the rejection loudly: {stdout}"
    );
    assert_eq!(
        worktree_snapshot(&repo),
        before,
        "the real worktree must be byte-identical after a rejected proposal"
    );
    assert!(!branch_local(&repo, "GX-matrix-garbage"));
}

#[test]
fn test_agent_nonzero_exit_is_rejected_and_worktree_untouched() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(scripts.path(), "agent.sh", "echo boom 1>&2; exit 7");
    let cfg = write_config(scripts.path(), &agent, 60);
    let before = worktree_snapshot(&repo);

    let out = run_propose(
        workspace.path(),
        &cfg,
        data_home.path(),
        "GX-matrix-nonzero",
        "fail on purpose",
    );
    assert!(
        out.status.success(),
        "a per-repo agent failure is a LOUD per-repo error, not a process failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAILED") && stdout.contains("status 7"),
        "stdout must name the nonzero exit: {stdout}"
    );
    assert_eq!(
        worktree_snapshot(&repo),
        before,
        "the real worktree must be byte-identical after an agent failure"
    );
    assert!(!branch_local(&repo, "GX-matrix-nonzero"));
}

#[test]
fn test_agent_timeout_is_rejected_and_worktree_untouched() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(scripts.path(), "agent.sh", "sleep 300");
    // 1s timeout keeps the test fast.
    let cfg = write_config(scripts.path(), &agent, 1);
    let before = worktree_snapshot(&repo);

    let start = Instant::now();
    let out = run_propose(
        workspace.path(),
        &cfg,
        data_home.path(),
        "GX-matrix-timeout",
        "hang forever",
    );
    let elapsed = start.elapsed();
    assert!(
        out.status.success(),
        "a per-repo timeout is a LOUD per-repo error, not a process failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAILED") && stdout.contains("timed out"),
        "stdout must name the timeout: {stdout}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the process-group kill must land near the 1s deadline: {elapsed:?}"
    );
    assert_eq!(
        worktree_snapshot(&repo),
        before,
        "the real worktree must be byte-identical after a timeout"
    );
    assert!(!branch_local(&repo, "GX-matrix-timeout"));
}

#[test]
fn test_empty_diff_is_a_valid_outcome_not_an_error() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(scripts.path(), "agent.sh", "exit 0");
    let cfg = write_config(scripts.path(), &agent, 60);
    let before = worktree_snapshot(&repo);
    let change_id = "GX-matrix-empty";

    let out = run_propose(workspace.path(), &cfg, data_home.path(), change_id, "noop");
    assert!(
        out.status.success(),
        "empty diff must succeed (not an error): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 proposed") && stdout.contains("1 empty"),
        "stdout must record the empty outcome: {stdout}"
    );
    assert_eq!(
        worktree_snapshot(&repo),
        before,
        "the real worktree must be byte-identical after an empty proposal"
    );
    assert!(!branch_local(&repo, change_id));

    // No Proposed repo -> no change state file at all (Phase 4 design
    // decision: empty/failed outcomes live only in the manifest).
    let state_path = data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    assert!(
        !state_path.exists(),
        "an all-empty propose must not write change state"
    );
}

#[test]
fn test_apply_refuses_a_drifted_repo_and_worktree_untouched() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    let repo = make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(
        scripts.path(),
        "agent.sh",
        "printf 'new value\\n' > data.md",
    );
    let cfg = write_config(scripts.path(), &agent, 60);
    let change_id = "GX-matrix-drift";

    let propose = run_propose(
        workspace.path(),
        &cfg,
        data_home.path(),
        change_id,
        "make data.md say new value",
    );
    assert!(
        propose.status.success(),
        "gx propose failed: {}",
        String::from_utf8_lossy(&propose.stderr)
    );

    // DRIFT: advance HEAD past the proposal's base and push it, so apply's
    // post-pull head no longer matches the proposal's recorded base_sha.
    std::fs::write(repo.join("other.txt"), b"drift\n").unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "drift"], &repo);
    git(&["push", "--quiet", "origin", "main"], &repo);
    let before = worktree_snapshot(&repo);

    let apply = Command::new(gx_binary())
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "apply",
            change_id,
            "--yes",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx apply failed to spawn");
    assert!(
        apply.status.success(),
        "a drifted-repo apply is a LOUD per-repo refusal, not a process failure: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let stdout = String::from_utf8_lossy(&apply.stdout);
    assert!(
        stdout.contains("0 applied") && stdout.contains("1 drifted/failed"),
        "stdout must report the drift refusal: {stdout}"
    );
    assert_eq!(
        worktree_snapshot(&repo),
        before,
        "the real worktree must be byte-identical after a drift refusal"
    );
    assert!(
        !branch_local(&repo, change_id),
        "a drift refusal must not create the GX branch"
    );

    // The repo stays Proposed with the drift error recorded (design apply-pass
    // semantics: the remedy is a fresh propose for the stragglers).
    let state_path = data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    // The recorded slug is `<tmpdir-name>/app` (no recognizable origin host to
    // derive `org/repo` from a local bare remote), so look up the sole entry
    // rather than assume the key - the point under test is its VALUE.
    let repo_state = state["repositories"]
        .as_object()
        .and_then(|m| m.values().next())
        .expect("exactly one repo in the drifted change state");
    assert_eq!(repo_state["status"], "Proposed");
    assert!(
        repo_state["error"]
            .as_str()
            .unwrap_or("")
            .contains("drifted"),
        "recorded error must name the drift: {repo_state}"
    );
}
