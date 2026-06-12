//! Per-repo advisory locking.
//!
//! Before mutating a repository, gx acquires an exclusive lock so a second
//! concurrent invocation can't interleave stash/branch operations on the same
//! repo (design Q5). The lock is a file created with `O_EXCL` semantics
//! (`create_new`) under `$XDG_DATA_HOME/gx/locks/<hash>.lock`, carrying the
//! holder's pid / cwd / command / start time. A stale lock (holder pid gone) is
//! reclaimed with a warning.

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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create lock dir: {}", parent.display()))?;
        }

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let info = LockInfo::current();
                    let line = serde_json::to_string(&info).unwrap_or_default();
                    // Best-effort: the lock's existence is what matters.
                    let _ = writeln!(file, "{line}");
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if reclaim_if_stale(&path)? {
                        continue;
                    }
                    let holder = read_holder(&path);
                    return Err(eyre::eyre!(
                        "Repository is locked by another gx process ({holder}); lock: {}",
                        path.display()
                    ));
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to create lock file: {}", path.display())
                    });
                }
            }
        }
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            warn!("Failed to release lock {}: {}", self.path.display(), e);
        }
    }
}

/// Compute the lock file path for a repository from its canonical path.
fn lock_path_for(repo_path: &Path) -> Result<PathBuf> {
    let canonical = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let hash = fnv1a_hex(canonical.to_string_lossy().as_bytes());
    let dir = xdg_data_dir()
        .ok_or_else(|| eyre::eyre!("Could not determine data dir (set HOME or XDG_DATA_HOME)"))?
        .join("gx")
        .join("locks");
    Ok(dir.join(format!("{hash}.lock")))
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

/// If the lock's holder process is gone, remove the lock and return true.
fn reclaim_if_stale(path: &Path) -> Result<bool> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    let info: LockInfo = match serde_json::from_str(content.trim()) {
        Ok(i) => i,
        Err(_) => return Ok(false),
    };
    if process_alive(info.pid) {
        return Ok(false);
    }
    warn!(
        "Reclaiming stale lock at {} (holder pid {} is gone)",
        path.display(),
        info.pid
    );
    // Best effort: another process may win the race; that's fine.
    let _ = fs::remove_file(path);
    Ok(true)
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
