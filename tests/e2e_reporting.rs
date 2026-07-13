//! Phase 1 (`docs/design/2026-07-12-gx-production-hardening.md`): airtight,
//! scriptable `gx create` reporting, exercised end to end against a real
//! multi-repo workspace.
//!
//! - `GX_TEST_FORCE_REPO_ERROR` (a test-only fault-injection hook mirroring
//!   `GX_TEST_FAIL_STATE_SAVE`) deterministically fails exactly one repo's
//!   result without needing to fabricate a real git failure, so the non-zero
//!   exit code and `--report` file can be asserted precisely.
//! - `GX_TEST_PANIC_WORKER` panics the rayon worker processing a named repo,
//!   proving the `main` panic hook logs an ERROR diagnostic rather than the
//!   process aborting with zero trace.

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

/// A repo at `workspace/<name>` with a bare remote under `remotes` (head
/// branch resolution needs `origin/HEAD`, per `e2e_create_lifecycle.rs`'s
/// `make_repo`; these tests never commit or push, only dry-run `sub`).
fn make_repo(workspace: &Path, remotes: &Path, name: &str) -> std::path::PathBuf {
    let bare = remotes.join(format!("{name}.git"));
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );

    let repo = workspace.join(name);
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

/// A `gx create` (dry-run: no `--commit`) forcing `broken` to fail via
/// `GX_TEST_FORCE_REPO_ERROR` must exit non-zero, name the failing repo in
/// the on-screen summary, AND write a `--report` file that parses as JSON and
/// lists that failure - the three Phase 1 success criteria in one run.
#[test]
fn test_create_exits_nonzero_and_reports_forced_repo_failure() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    make_repo(workspace.path(), remotes.path(), "clean");
    make_repo(workspace.path(), remotes.path(), "broken");

    let report_path = workspace.path().join("report.json");

    let output = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "--files",
            "**/*.md",
            "--report",
            report_path.to_str().unwrap(),
            "sub",
            "old",
            "new",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .env("GX_TEST_FORCE_REPO_ERROR", "broken")
        .output()
        .expect("gx create failed to spawn");

    // Criterion 1: non-zero exit on a forced per-repo failure.
    assert!(
        !output.status.success(),
        "gx create must exit non-zero when a repo result carries an error"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "exit code should equal the error count (1 forced failure)"
    );

    // Criterion 2: the failing repo is named in the on-screen summary.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("broken"),
        "on-screen summary must name the failing repo; got:\n{stdout}"
    );

    // Criterion 3: --report produces a file that parses as JSON and lists
    // the failure.
    let report_json = std::fs::read_to_string(&report_path).expect("report file must exist");
    let parsed: serde_json::Value =
        serde_json::from_str(&report_json).expect("report file must parse as JSON");
    let entries = parsed.as_array().expect("report is a JSON array");
    assert_eq!(entries.len(), 1, "exactly the one forced failure");
    assert!(
        entries[0]["repo"].as_str().unwrap().contains("broken"),
        "report entry must name the failing repo; got: {entries:?}"
    );
    assert_eq!(entries[0]["phase"], "dry-run");
    assert!(
        entries[0]["error"]
            .as_str()
            .unwrap()
            .contains("GX_TEST_FORCE_REPO_ERROR"),
        "report entry must carry the injected error; got: {entries:?}"
    );
}

/// Break-the-guard: a healthy run (no forced failure) still exits 0 and
/// writes an empty-array report - proves the exit-code/report machinery
/// doesn't false-positive on success.
#[test]
fn test_create_exits_zero_and_reports_no_failures_when_healthy() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    make_repo(workspace.path(), remotes.path(), "clean");

    let report_path = workspace.path().join("report.json");

    let output = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "--files",
            "**/*.md",
            "--report",
            report_path.to_str().unwrap(),
            "sub",
            "old",
            "new",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .output()
        .expect("gx create failed to spawn");

    assert!(
        output.status.success(),
        "a healthy run must still exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report_json = std::fs::read_to_string(&report_path).expect("report file must exist");
    let parsed: serde_json::Value = serde_json::from_str(&report_json).unwrap();
    assert_eq!(
        parsed.as_array().unwrap().len(),
        0,
        "no repo failed, so the report must be an empty array"
    );
}

/// A worker panicking mid-run (`GX_TEST_PANIC_WORKER`) must produce an ERROR
/// log line via the `main` panic hook naming the thread, source location, and
/// message - not a bare, undiagnosable process abort.
#[test]
fn test_panicking_worker_logs_an_error_line_via_panic_hook() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    make_repo(workspace.path(), remotes.path(), "panicky");

    let output = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "create",
            "--files",
            "**/*.md",
            "sub",
            "old",
            "new",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .env("GX_TEST_PANIC_WORKER", "panicky")
        .output()
        .expect("gx create failed to spawn");

    assert!(
        !output.status.success(),
        "a worker panic must not look like a successful run"
    );

    let log_path = data_home.path().join("gx").join("logs").join("gx.log");
    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("log file must exist at {}: {e}", log_path.display()));

    assert!(
        log.contains("ERROR") && log.contains("panic"),
        "log must carry an ERROR panic diagnostic; got:\n{log}"
    );
    assert!(
        log.contains("GX_TEST_PANIC_WORKER: simulated worker panic for panicky"),
        "log must carry the panic message; got:\n{log}"
    );
    assert!(
        log.contains("core.rs"),
        "log must carry the panic's source location; got:\n{log}"
    );
}
