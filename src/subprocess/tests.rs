use super::*;
use std::process::Command;
use std::time::{Duration, Instant};

/// A fast command returns Ok with its captured stdout and a success status.
#[test]
fn test_run_checked_captures_stdout() {
    let mut cmd = Command::new("echo");
    cmd.arg("hello-run-checked");
    let output = run_checked(&mut cmd, Duration::from_secs(10)).unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello-run-checked"
    );
}

/// A non-zero exit is Ok (like `Command::output`); the caller inspects status.
/// Only a timeout is an Err. Break-the-guard: if run_checked ever mapped a
/// non-zero exit to Err, callers that log stderr themselves would break.
#[test]
fn test_run_checked_nonzero_exit_is_ok() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "exit 3"]);
    let output = run_checked(&mut cmd, Duration::from_secs(10)).unwrap();
    assert_eq!(output.status.code(), Some(3));
}

/// A command that outruns the timeout is KILLED and returns an Err naming the
/// timeout -- it does NOT block for the full sleep. Bite: without the
/// process-group kill this test would hang for 30s; the elapsed assertion fails
/// loudly if the kill is removed.
#[test]
fn test_run_checked_kills_on_timeout() {
    let start = Instant::now();
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 30"]);
    let result = run_checked(&mut cmd, Duration::from_millis(300));
    let elapsed = start.elapsed();

    let err = result.expect_err("a command that outruns its timeout must Err");
    assert!(
        err.to_string().contains("timed out"),
        "error should name the timeout, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "killed command should return promptly, took {elapsed:?}"
    );
}

/// The killed child's process group is felled, including grandchildren. Bite:
/// the outer `sh` spawns a child `sleep` that would outlive a lone-pid kill; the
/// process-group kill fells both, so run_checked still returns promptly.
#[test]
fn test_run_checked_kills_grandchild_process_group() {
    let start = Instant::now();
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 30 & wait"]);
    let result = run_checked(&mut cmd, Duration::from_millis(300));
    let elapsed = start.elapsed();

    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_secs(5),
        "group kill should fell the grandchild too, took {elapsed:?}"
    );
}

/// stdin is nulled: a command that reads stdin gets immediate EOF and exits
/// fast instead of hanging to the timeout. Bite: without `Stdio::null()` `cat`
/// would block on stdin until the 10s timeout and this would take ~10s (and
/// return Err); with it, it returns Ok promptly.
#[test]
fn test_run_checked_nulls_stdin() {
    let start = Instant::now();
    let mut cmd = Command::new("cat");
    let output = run_checked(&mut cmd, Duration::from_secs(10)).unwrap();
    let elapsed = start.elapsed();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        elapsed < Duration::from_secs(5),
        "nulled stdin should EOF immediately, took {elapsed:?}"
    );
}

/// Output larger than the ~64 KB pipe buffer does not deadlock -- both pipes are
/// drained concurrently. Bite: sequential single-pipe draining would deadlock
/// here and trip the timeout (Err); concurrent draining returns the full 200 KB.
#[test]
fn test_run_checked_drains_large_output_without_deadlock() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "yes payload | head -c 200000"]);
    let output = run_checked(&mut cmd, Duration::from_secs(10)).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 200000);
}

/// Absent any `init_subprocess_timeout`, the effective timeout is the compiled
/// default (no magic number at the call site).
#[test]
fn test_subprocess_timeout_defaults_to_const() {
    assert_eq!(
        subprocess_timeout(),
        Duration::from_secs(DEFAULT_SUBPROCESS_TIMEOUT_SECS)
    );
}
