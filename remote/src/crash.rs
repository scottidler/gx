//! Crash-injection test hook (design F15, Phase 8).
//!
//! [`maybe_crash`] aborts the process the instant `$GX_CRASH_POINT` names the
//! point it is called with. It is compiled into every build but INERT unless
//! the env var is set to one of the [`CRASH_POINTS`], so it never changes
//! production behavior (Risks table: "hook is a no-op unless the env var is
//! set; grep-guard test asserts no other call sites"). It exists solely so the
//! crash-injection e2e can kill a REAL `gx create` process at each phase
//! boundary and prove `gx rollback` recovers every one.
//!
//! [`std::process::abort`] (not `exit`) is used deliberately: it terminates
//! WITHOUT unwinding, so no `Drop` runs -- the transaction's recovery file and
//! backups are left exactly as the last write-ahead persist left them, the way
//! a real SIGKILL would. Every crash point sits AFTER the write-ahead persist
//! of the state that makes it recoverable, so the abort can never strand an
//! unrecorded mutation.

use log::error;

/// Env var that arms the crash hook. Unset -> every [`maybe_crash`] is a no-op.
pub const CRASH_ENV: &str = "GX_CRASH_POINT";

/// The six named crash points, one per phase boundary in the create pipeline.
/// Kept as a const list so the grep-guard test can assert the wired call sites
/// match this vocabulary exactly (no typos, no rogue points).
///
/// - `after-stash`: the user's WIP has been stashed (phase `mutating`).
/// - `after-branch`: the GX branch has been created (phase `mutating`).
/// - `after-commit`: the change is committed on the GX branch (phase `mutating`).
/// - `before-push`: the `pushing` phase is stamped but `git push` has NOT run;
///   recovery resolves the push state with a read-only `ls-remote` probe.
/// - `after-push`: the push succeeded and `pushed` is stamped; recovery keeps
///   the shared work.
/// - `mid-finalize`: `finalizing` is stamped inside `finalize()`; recovery keeps
///   the shared work.
pub const CRASH_POINTS: &[&str] = &[
    "after-stash",
    "after-branch",
    "after-commit",
    "before-push",
    "after-push",
    "mid-finalize",
];

/// Abort the process if `$GX_CRASH_POINT` names this `point`. Returns
/// immediately (inert) when the env var is unset or names a different point. A
/// set-but-unrecognized value is a loud no-op (fail loud on a typo'd point
/// rather than silently never crashing).
pub fn maybe_crash(point: &str) {
    let Ok(want) = std::env::var(CRASH_ENV) else {
        return;
    };
    if !CRASH_POINTS.contains(&want.as_str()) {
        error!(
            "{CRASH_ENV}={want:?} is not a known crash point (one of {CRASH_POINTS:?}); ignoring"
        );
        return;
    }
    if want == point {
        error!("{CRASH_ENV}={point} hit; aborting to simulate a crash");
        std::process::abort();
    }
}

#[cfg(test)]
mod tests;
