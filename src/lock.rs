//! Per-repo and per-change advisory locking.
//!
//! Before mutating a repository, gx acquires an exclusive [`RepoLock`] so a
//! second concurrent invocation can't interleave stash/branch operations on
//! the same repo (design Q5). A [`ChangeLock`] gives the same guarantee for
//! `changes/<id>.json` read-modify-writes (Phase 7 [F6]): the atomic write
//! that `StateManager::save` already uses prevents a TORN file, not a LOST
//! update between two processes' load-mutate-save cycles, and the change lock
//! closes that race. Both lock kinds are a file created with `O_EXCL`
//! semantics (`create_new`) under `$XDG_DATA_HOME/gx/locks/<hash>.lock`,
//! carrying the holder's pid / cwd / command / start time. A stale lock
//! (holder pid gone) is reclaimed with a warning.
//!
//! Reclaim is TOCTOU-safe (Phase 7 [F7]): the stale file is renamed to a
//! private name FIRST, re-verified there, and only then removed. A racing
//! reclaimer that loses the rename sees the failure and simply retries
//! `acquire` rather than blindly deleting whatever now sits at the shared
//! path -- so a lock another process has since recreated as its own live
//! lock is never destroyed.

use crate::config::xdg_data_dir;
use eyre::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Metadata recorded in a lock file about its holder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub pid: u32,
    pub cwd: String,
    pub command: String,
    pub started_at: String,
}

impl LockInfo {
    fn current() -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let command = std::env::args().collect::<Vec<_>>().join(" ");
        Self {
            pid: std::process::id(),
            cwd,
            command,
            started_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// An acquired per-repo lock. Releases (removes the lock file) on drop.
pub struct RepoLock {
    path: PathBuf,
}

impl RepoLock {
    /// Acquire the lock for `repo_path`. Fails fast if another live process
    /// holds it; reclaims the lock if the holder's pid is gone.
    pub fn acquire(repo_path: &Path) -> Result<Self> {
        let path = lock_path_for(repo_path)?;
        debug!(
            "RepoLock::acquire: repo_path={} lock={}",
            repo_path.display(),
            path.display()
        );
        acquire_lock_file(&path)?;
        Ok(Self { path })
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            warn!("Failed to release lock {}: {}", self.path.display(), e);
        }
    }
}

/// An acquired change-level lock. Releases (removes the lock file) on drop.
/// Held around every read-modify-write of `changes/<id>.json` -- `review
/// sync`, `review approve`/`delete`, `cleanup`, `undo`, and the create-path
/// incremental saves -- so two processes' load-mutate-save cycles on the same
/// change can never interleave and lose an update (Phase 7 [F6]).
pub struct ChangeLock {
    path: PathBuf,
}

impl ChangeLock {
    /// Acquire the lock for `change_id`. Same acquire/fail-fast/reclaim
    /// semantics as [`RepoLock::acquire`].
    pub fn acquire(change_id: &str) -> Result<Self> {
        let path = change_lock_path_for(change_id)?;
        debug!(
            "ChangeLock::acquire: change_id={change_id} lock={}",
            path.display()
        );
        acquire_lock_file(&path)?;
        Ok(Self { path })
    }
}

impl Drop for ChangeLock {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            warn!("Failed to release lock {}: {}", self.path.display(), e);
        }
    }
}

/// Shared acquire logic for both lock kinds: create `path` with `O_EXCL`
/// semantics, reclaiming a stale holder (and retrying) as needed. Returns once
/// the caller holds the lock, or a fail-fast error naming the live holder.
fn acquire_lock_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create lock dir: {}", parent.display()))?;
    }

    loop {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                let info = LockInfo::current();
                let line = serde_json::to_string(&info).unwrap_or_default();
                // Best-effort: the lock's existence is what matters.
                let _ = writeln!(file, "{line}");
                drop(file);

                // Re-verify OUR content is still at `path` before declaring
                // victory. A concurrent racer's reclaim of a DIFFERENT stale
                // entry can (rarely) sweep this brand-new file away in the gap
                // between `create_new` succeeding and this check (it renames
                // away whatever it finds to verify staleness, then restores it
                // -- see `reclaim_if_stale`). Retrying here, instead of
                // reporting success unconditionally, closes that window: we
                // never hand back a "successful" lock we don't actually hold.
                match fs::read_to_string(path) {
                    Ok(confirm) if confirm.trim() == line.trim() => {
                        test_hold_delay();
                        return Ok(());
                    }
                    _ => continue,
                }
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if reclaim_if_stale(path)? {
                    continue;
                }
                let holder = read_holder(path);
                return Err(eyre::eyre!(
                    "Locked by another gx process ({holder}); lock: {}",
                    path.display()
                ));
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("Failed to create lock file: {}", path.display()));
            }
        }
    }
}

/// Test-only hold delay: if `GX_TEST_LOCK_DELAY_MS` is set, sleep for that many
/// milliseconds right after acquiring, before returning to the caller. Inert
/// unless the env var is set (same "compiled in, inert by default" shape as
/// the crash-injection hook this design introduces later); exists solely so
/// an integration test can create a deterministic two-process contention
/// window between two real spawned `gx` binaries, rather than racing on
/// uncontrolled process-startup timing.
fn test_hold_delay() {
    if let Ok(ms) = std::env::var("GX_TEST_LOCK_DELAY_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

/// Compute the lock file path for a repository from its canonical path.
fn lock_path_for(repo_path: &Path) -> Result<PathBuf> {
    let canonical = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let hash = fnv1a_hex(canonical.to_string_lossy().as_bytes());
    Ok(locks_dir()?.join(format!("{hash}.lock")))
}

/// Compute the lock file path for a change id.
fn change_lock_path_for(change_id: &str) -> Result<PathBuf> {
    let hash = fnv1a_hex(change_id.as_bytes());
    Ok(locks_dir()?.join(format!("change-{hash}.lock")))
}

/// The shared lock directory both lock kinds create their files under.
fn locks_dir() -> Result<PathBuf> {
    Ok(xdg_data_dir()
        .ok_or_else(|| eyre::eyre!("Could not determine data dir (set HOME or XDG_DATA_HOME)"))?
        .join("gx")
        .join("locks"))
}

/// Read the holder description from a lock file (for error messages).
fn read_holder(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<LockInfo>(content.trim()) {
            Ok(info) => format!(
                "pid {} running `{}` since {}",
                info.pid, info.command, info.started_at
            ),
            Err(_) => "unknown holder".to_string(),
        },
        Err(_) => "unknown holder".to_string(),
    }
}

/// Whether lock-file `content`, if parseable, names a pid that is no longer
/// alive. Unparseable content (e.g. a file mid-write, between `create_new`
/// and the holder's `writeln!` landing) is treated as NOT stale: a lock we
/// cannot positively confirm dead must never be reclaimed. This is the single
/// guard reclaim relies on both up front and, critically, when re-verifying
/// the renamed file below -- flipping this default is exactly the class of
/// bug F7 closes (it briefly let concurrent racers all "win" by reclaiming
/// each other's just-created, not-yet-written lock files).
fn is_stale_lock_content(content: &str) -> bool {
    match serde_json::from_str::<LockInfo>(content.trim()) {
        Ok(info) => !process_alive(info.pid),
        Err(_) => false,
    }
}

/// Monotonic counter giving each reclaim attempt (even concurrent ones in the
/// same process) a distinct staging filename.
static RECLAIM_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// If the lock's holder process is gone, reclaim it and return `true` (the
/// caller should retry `acquire`). Returns `false` only when `path` is
/// confirmed to still be held by a live process.
///
/// Fixed TOCTOU (F7): the old code re-read nothing before its `remove_file`,
/// so a racing reclaimer that had already removed-and-recreated the lock as
/// its own live one could have that fresh lock deleted out from under it.
/// Now the file is renamed to a private staging name FIRST -- an atomic
/// operation, so at most one racer can win it; a losing racer's rename fails
/// (ENOENT, someone else already moved it) and just retries `acquire`. The
/// winner then re-verifies staleness on the file it now exclusively owns: if
/// a fresh live lock got swept up in the rename (another process finished its
/// own reclaim-and-acquire in the interim), it is renamed straight back,
/// untouched -- never removed.
fn reclaim_if_stale(path: &Path) -> Result<bool> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    if !is_stale_lock_content(&content) {
        return Ok(false);
    }

    let staged = path.with_extension(format!(
        "reclaim-{}-{}",
        std::process::id(),
        RECLAIM_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if let Err(e) = fs::rename(path, &staged) {
        // Another process is already mid-reclaim (or has already removed or
        // recreated this exact lock); never remove_file blindly here -- just
        // signal the caller to retry acquire from scratch.
        debug!("Lock reclaim rename raced away for {}: {e}", path.display());
        return Ok(true);
    }

    let staged_content = fs::read_to_string(&staged).unwrap_or_default();
    if is_stale_lock_content(&staged_content) {
        warn!("Reclaiming stale lock at {} (holder gone)", path.display());
        let _ = fs::remove_file(&staged);
        Ok(true)
    } else {
        // A racing reclaimer already recreated a live lock here. Restore it
        // via hard_link, NEVER a blind rename: a plain `rename(staged, path)`
        // would silently CLOBBER a fourth racer that legitimately created its
        // own fresh lock at `path` while it sat vacated during this check
        // (the exact class of bug F7 exists to close, one step later). A
        // failing hard_link just means someone else already correctly holds
        // `path` again -- nothing to restore.
        let _ = fs::hard_link(&staged, path);
        let _ = fs::remove_file(&staged);
        Ok(false)
    }
}

/// Whether a process with the given pid is currently alive.
fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Conservative on non-Linux: assume alive so we never wrongly reclaim.
        let _ = pid;
        true
    }
}

/// FNV-1a 64-bit hash, rendered as hex. Stable across runs and toolchains
/// (unlike `DefaultHasher`); used only for lock filenames.
fn fnv1a_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests;
