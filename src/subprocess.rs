//! Shared subprocess chokepoint for every `git`/`gh` invocation.
//!
//! `run_checked` is the single seam every blocking `git`/`gh` `Command` routes
//! through (design doc `2026-07-12-gx-production-hardening.md`, Phase 2). It
//! harvests the process-group-kill mechanism from `create/core/propose.rs`'s
//! `run_agent`: spawn the child in its OWN process group, poll `try_wait` to a
//! wall-clock deadline, and on expiry `kill -KILL -<pgid>` fells the whole
//! group (any credential/ssh helpers the child spawned) and reaps it. Unlike
//! `run_agent` (which redirects stdio to a log file), `run_checked` must RETURN
//! the captured `Output`, so it drains stdout and stderr CONCURRENTLY on their
//! own threads while the parent polls -- reading one pipe to completion before
//! the other deadlocks the child once it fills the ~64 KB pipe buffer
//! (`rust.md` subprocess hygiene).
//!
//! The child's stdin is nulled: a `gh` waiting on an interactive/auth prompt or
//! a `git` waiting on a credential prompt is itself a wedge; a closed stdin
//! turns that prompt-hang into a fast EOF failure instead of blocking to the
//! timeout.

use eyre::{Context, Result};
use log::{debug, warn};
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::config::DEFAULT_SUBPROCESS_TIMEOUT_SECS;

/// Poll interval while waiting for the child to exit (mirrors `propose.rs`).
const POLL_INTERVAL_MS: u64 = 50;

/// Process-global subprocess timeout, initialized once from `Config` in `main`.
/// Write-once (`OnceLock`) so the many parallel rayon workers all read the same
/// value with no shared mutable state; the git/gh call sites live too deep to
/// thread `Config` through, so they read this global via [`subprocess_timeout`].
static SUBPROCESS_TIMEOUT: OnceLock<Duration> = OnceLock::new();

/// Install the configured subprocess timeout (called once from `main` after the
/// config loads). A second call is a no-op -- the first value wins.
pub fn init_subprocess_timeout(timeout: Duration) {
    debug!("init_subprocess_timeout: timeout={}s", timeout.as_secs());
    if SUBPROCESS_TIMEOUT.set(timeout).is_err() {
        warn!("init_subprocess_timeout: already initialized; ignoring second value");
    }
}

/// The effective subprocess timeout: the value installed from config, or the
/// compiled-in default when nothing initialized it (tests / library callers).
pub fn subprocess_timeout() -> Duration {
    SUBPROCESS_TIMEOUT
        .get()
        .copied()
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_SUBPROCESS_TIMEOUT_SECS))
}

/// Human-readable `program arg arg ...` for diagnostics (never the captured
/// output, per the logging rule's large-payload clause).
fn describe(cmd: &Command) -> String {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.is_empty() {
        program
    } else {
        format!("{program} {}", args.join(" "))
    }
}

/// SIGKILL an entire process group by pgid, via `/bin/kill -KILL -<pgid>`. std
/// exposes no group-kill and `libc` is not a dependency; the `kill` binary is
/// always present on the Unix targets gx runs on (harvested from `propose.rs`).
fn kill_process_group(pgid: u32) {
    let status = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pgid}"))
        .status();
    if let Err(e) = status {
        warn!("kill_process_group: failed to signal group {pgid}: {e}");
    }
}

/// Run `cmd` to completion under a wall-clock `timeout`, returning its captured
/// [`Output`]. On expiry the child's whole process group is SIGKILLed and reaped
/// and an `Err` naming the command + timeout is returned. A non-zero EXIT is
/// still `Ok` (the caller inspects `output.status`, exactly as `Command::output`
/// behaves); only a timeout (or a spawn/poll failure) is an `Err`.
///
/// The child is spawned in its own process group with stdin nulled and both
/// output pipes drained concurrently -- see the module docs for why.
pub fn run_checked(cmd: &mut Command, timeout: Duration) -> Result<Output> {
    use std::os::unix::process::CommandExt;

    let desc = describe(cmd);
    debug!("run_checked: cmd=\"{desc}\" timeout={}s", timeout.as_secs());

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // New process group (pgid == child pid) so a timeout kill fells the
        // whole tree, including any git credential / ssh helper it spawned.
        .process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn `{desc}`"))?;
    let pgid = child.id();

    // Drain BOTH pipes concurrently on their own threads so a child that
    // produces more than the ~64 KB pipe buffer can't block on the pipe we
    // aren't reading (which would look exactly like a hang and trip the kill).
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| eyre::eyre!("child stdout pipe missing for `{desc}`"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| eyre::eyre!("child stderr pipe missing for `{desc}`"))?;
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().context("Failed to poll child process")? {
            Some(status) => {
                let stdout = stdout_reader.join().unwrap_or_default();
                let stderr = stderr_reader.join().unwrap_or_default();
                debug!(
                    "run_checked: cmd=\"{desc}\" exited status={:?} stdout_len={} stderr_len={}",
                    status.code(),
                    stdout.len(),
                    stderr.len()
                );
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None => {
                if Instant::now() >= deadline {
                    warn!(
                        "run_checked: cmd=\"{desc}\" timed out after {}s; killing process group {pgid}",
                        timeout.as_secs()
                    );
                    kill_process_group(pgid);
                    // Reap so no zombie lingers; the exact status is irrelevant.
                    let _ = child.wait();
                    // The reader threads EOF now that the pipes are closed.
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(eyre::eyre!(
                        "command `{desc}` timed out after {}s (process group killed)",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }
}

#[cfg(test)]
mod tests;
