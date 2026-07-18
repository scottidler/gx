//! Per-repo and per-change advisory locking.
//!
//! Before mutating a repository, gx acquires an exclusive [`RepoLock`] so a
//! second concurrent invocation can't interleave stash/branch operations on
//! the same repo (design Q5). A [`ChangeLock`] gives the same guarantee for
//! `changes/<id>.json` read-modify-writes (Phase 7 [F6]): the atomic write
//! that `StateManager::save` already uses prevents a TORN file, not a LOST
//! update between two processes' load-mutate-save cycles, and the change lock
//! closes that race.
//!
//! Both lock kinds are backed by an OS advisory lock ([`std::fs::File::try_lock`],
//! stable since Rust 1.89) on a file under `$XDG_DATA_HOME/gx/locks/<hash>.lock`.
//! The kernel releases the lock automatically when the holding process exits --
//! including on `kill -9` -- so there is NO staleness concept and NO reclaim
//! machinery: a lock whose holder is gone is simply acquirable by the next
//! contender. `try_lock` returning `WouldBlock` preserves gx's fail-fast,
//! no-queueing semantics: a second live invocation errors immediately naming
//! the holder rather than blocking.
//!
//! Two invariants the tests pin (both are prior-bug regressions):
//!
//! - **Never truncate on open, never unlink on drop.** The lock file is opened
//!   read/write *without* truncation so a contender can never clobber a live
//!   holder's metadata; holder JSON is (re)written only AFTER the lock is held.
//!   `Drop` only unlocks/closes (the owned `File` drops) and NEVER unlinks the
//!   file. Unlinking under flock reintroduces a 2-winner interleave: A holds
//!   (inode1), B has a pending lock on inode1, A drops+unlinks, C creates a
//!   FRESH file (inode2) at the path and locks it while B still holds inode1.
//!   Not unlinking keeps the path bound to one inode, so every reopen contends
//!   on the same lock. Lock files persist harmlessly (unlocked = acquirable).
//! - **The `File` handle IS the lock.** The RAII guard owns it for the lock's
//!   full lifetime. Child processes (the spawned agent) must not inherit the
//!   fd; `O_CLOEXEC` is the Rust default and is asserted by a test, not assumed.
//!
//! Advisory locks are unreliable on network filesystems; `$XDG_DATA_HOME` is
//! local, so this is a non-issue here.

use eyre::{Context, Result};
use local::config::xdg_data_dir;
use log::debug;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Seek, SeekFrom, Write};
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

/// An acquired per-repo lock. The owned `File` holds the OS advisory lock for
/// the guard's lifetime; dropping the guard releases (unlocks) it. The lock
/// file itself is intentionally left in place -- see the module docs.
pub struct RepoLock {
    path: PathBuf,
    // The OS lock lives on this open file description. Held for the guard's
    // full lifetime; dropped (unlocked) when the guard drops. NEVER unlinked.
    _file: File,
}

impl RepoLock {
    /// Acquire the lock for `repo_path`. Fails fast (naming the holder) if
    /// another live process holds it; a lock whose holder has exited is
    /// acquired directly (the kernel already released it).
    pub fn acquire(repo_path: &Path) -> Result<Self> {
        let path = lock_path_for(repo_path)?;
        debug!(
            "RepoLock::acquire: repo_path={} lock={}",
            repo_path.display(),
            path.display()
        );
        let file = acquire_lock_file(&path)?;
        Ok(Self { path, _file: file })
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // `_file` drops here, releasing the OS advisory lock. The lock file is
        // NEVER unlinked (panel must-fix 2026-07-12): unlinking under flock
        // reopens the 2-winner interleave documented in the module header.
        debug!("RepoLock::drop: releasing lock {}", self.path.display());
    }
}

/// An acquired change-level lock. Held around every read-modify-write of
/// `changes/<id>.json` -- `review sync`, `review approve`/`delete`, `cleanup`,
/// `undo`, and the create-path incremental saves -- so two processes'
/// load-mutate-save cycles on the same change can never interleave and lose an
/// update (Phase 7 [F6]). Same OS-lock backing and lifetime as [`RepoLock`].
pub struct ChangeLock {
    path: PathBuf,
    _file: File,
}

impl ChangeLock {
    /// Acquire the lock for `change_id`. Same acquire/fail-fast semantics as
    /// [`RepoLock::acquire`].
    pub fn acquire(change_id: &str) -> Result<Self> {
        let path = change_lock_path_for(change_id)?;
        debug!(
            "ChangeLock::acquire: change_id={change_id} lock={}",
            path.display()
        );
        let file = acquire_lock_file(&path)?;
        Ok(Self { path, _file: file })
    }
}

impl Drop for ChangeLock {
    fn drop(&mut self) {
        debug!("ChangeLock::drop: releasing lock {}", self.path.display());
    }
}

/// Shared acquire logic for both lock kinds: open `path` read/write WITHOUT
/// truncation, take an exclusive OS advisory lock non-blockingly, and -- once
/// held -- (re)write the holder JSON for error messages. Returns the locked
/// `File` (the caller's guard owns it), or a fail-fast error naming the live
/// holder when the lock is already held.
fn acquire_lock_file(path: &Path) -> Result<File> {
    debug!("acquire_lock_file: path={}", path.display());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create lock dir: {}", parent.display()))?;
    }

    // NO truncate: a contender must never clobber a live holder's metadata.
    // Never `File::create` (which truncates). `create(true)` makes the file on
    // first use; an existing file is opened as-is.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Explicitly NO truncation: a contender must never clobber a live
        // holder's metadata by merely opening the file.
        .truncate(false)
        .open(path)
        .with_context(|| format!("Failed to open lock file: {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => {
            // We hold the lock exclusively now; it is safe to overwrite the
            // holder metadata. Written AFTER the lock is held so a contender
            // can never truncate a live holder's file.
            write_holder(&mut file, path);
            test_hold_delay();
            debug!("acquire_lock_file: acquired {}", path.display());
            Ok(file)
        }
        Err(TryLockError::WouldBlock) => {
            let holder = read_holder(path);
            debug!(
                "acquire_lock_file: contended {} held by {holder}",
                path.display()
            );
            Err(eyre::eyre!(
                "Locked by another gx process ({holder}); lock: {}",
                path.display()
            ))
        }
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("Failed to lock file: {}", path.display()))
        }
    }
}

/// Write the current holder's JSON into the (already-locked) lock file for use
/// in a contender's error message. Best-effort: the OS lock -- not this
/// content -- is what enforces mutual exclusion, so a write failure is logged
/// and swallowed rather than failing the acquire. Truncates first (safe: we
/// hold the exclusive lock) so a shorter record can't leave a stale tail.
fn write_holder(file: &mut File, path: &Path) {
    let info = LockInfo::current();
    let line = serde_json::to_string(&info).unwrap_or_default();
    let mut write = || -> std::io::Result<()> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{line}")?;
        file.flush()
    };
    if let Err(e) = write() {
        debug!(
            "write_holder: failed to record holder metadata in {}: {e}",
            path.display()
        );
    }
}

/// Test-only hold delay: if `GX_TEST_LOCK_DELAY_MS` is set, sleep for that many
/// milliseconds right after acquiring, before returning to the caller. Inert
/// unless the env var is set; exists solely so an integration test can create a
/// deterministic two-process contention window between two real spawned `gx`
/// binaries, rather than racing on uncontrolled process-startup timing.
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
