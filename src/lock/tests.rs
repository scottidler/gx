use super::*;
use crate::test_utils::env_lock;
use tempfile::TempDir;

fn with_data_home<F: FnOnce()>(dir: &Path, f: F) {
    let guard = env_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
    f();
    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

#[test]
fn test_lock_acquire_and_release() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let lock = RepoLock::acquire(repo.path()).unwrap();
        let path = lock.path.clone();
        assert!(path.exists(), "lock file should exist while held");
        drop(lock);
        assert!(!path.exists(), "lock file should be removed on drop");
    });
}

#[test]
fn test_second_acquire_fails_fast() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let _held = RepoLock::acquire(repo.path()).unwrap();
        let second = RepoLock::acquire(repo.path());
        assert!(second.is_err(), "second lock on same repo must fail");
    });
}

#[test]
fn test_stale_lock_is_reclaimed() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let path = lock_path_for(repo.path()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Write a lock owned by a pid that does not exist.
        let stale = LockInfo {
            pid: 4_000_000_000,
            cwd: "/tmp".to_string(),
            command: "gx create".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };
        fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

        // Acquire should reclaim the stale lock and succeed.
        let lock = RepoLock::acquire(repo.path());
        assert!(lock.is_ok(), "stale lock should be reclaimed");
    });
}

#[test]
fn test_fnv1a_stable() {
    assert_eq!(fnv1a_hex(b"hello"), fnv1a_hex(b"hello"));
    assert_ne!(fnv1a_hex(b"a"), fnv1a_hex(b"b"));
    assert_eq!(fnv1a_hex(b"").len(), 16);
}

#[test]
fn test_change_lock_acquire_and_release() {
    let data = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let lock = ChangeLock::acquire("GX-2026-07-11T00-00-00").unwrap();
        let path = lock.path.clone();
        assert!(path.exists(), "change lock file should exist while held");
        drop(lock);
        assert!(!path.exists(), "change lock file should be removed on drop");
    });
}

#[test]
fn test_change_lock_second_acquire_fails_fast() {
    let data = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let _held = ChangeLock::acquire("GX-same-change").unwrap();
        let second = ChangeLock::acquire("GX-same-change");
        assert!(second.is_err(), "second lock on same change-id must fail");
    });
}

#[test]
fn test_change_lock_distinct_change_ids_do_not_contend() {
    let data = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let _a = ChangeLock::acquire("GX-a").unwrap();
        let b = ChangeLock::acquire("GX-b");
        assert!(b.is_ok(), "locks on different change-ids must not contend");
    });
}

// --- F7 reclaim TOCTOU -------------------------------------------------

#[test]
fn test_is_stale_lock_content_never_treats_live_pid_as_stale() {
    // This is the guard reclaim re-checks on the RENAMED file before ever
    // removing it (F7). Weakening it is exactly the regression it exists to
    // catch: a live lock (our own pid, guaranteed alive) must never classify
    // as stale.
    let live = LockInfo {
        pid: std::process::id(),
        cwd: "/tmp".to_string(),
        command: "gx create".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let content = serde_json::to_string(&live).unwrap();
    assert!(
        !is_stale_lock_content(&content),
        "a lock held by a live pid must never be classified stale"
    );
}

#[test]
fn test_is_stale_lock_content_treats_dead_pid_as_stale() {
    let dead = LockInfo {
        pid: 4_000_000_000,
        cwd: "/tmp".to_string(),
        command: "gx create".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let content = serde_json::to_string(&dead).unwrap();
    assert!(is_stale_lock_content(&content));
}

#[test]
fn test_is_stale_lock_content_treats_unparseable_as_not_stale() {
    // A lock file mid-write (between `create_new` and its holder's
    // `writeln!` landing) reads as empty or truncated JSON to a concurrent
    // racer. Treating that as "stale" is the exact regression a Phase 7
    // rewrite briefly introduced here: every racer reclaimed (and deleted)
    // every OTHER racer's just-created, not-yet-written lock file, so most
    // or all of them ended up "winning" `create_new` at some point instead
    // of exactly one. A lock we cannot positively confirm dead must never be
    // reclaimed.
    assert!(!is_stale_lock_content(""));
    assert!(!is_stale_lock_content("not json"));
}

#[test]
fn test_concurrent_reclaim_never_loses_the_winning_live_lock() {
    // Real concurrency proof for F7: many threads race to reclaim the SAME
    // stale lock and re-acquire it. Exactly one must win; its live lock file
    // must survive every other thread's reclaim attempt (never deleted out
    // from under it).
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let path = lock_path_for(repo.path()).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let stale = LockInfo {
            pid: 4_000_000_000,
            cwd: "/tmp".to_string(),
            command: "gx create".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };
        fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();

        let repo_path = Arc::new(repo.path().to_path_buf());
        let successes = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let repo_path = Arc::clone(&repo_path);
                let successes = Arc::clone(&successes);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    if let Ok(lock) = RepoLock::acquire(&repo_path) {
                        successes.fetch_add(1, Ordering::SeqCst);
                        tx.send(lock).ok();
                    }
                })
            })
            .collect();
        drop(tx);
        for h in handles {
            h.join().unwrap();
        }
        let winners: Vec<RepoLock> = rx.try_iter().collect();

        assert_eq!(
            successes.load(Ordering::SeqCst),
            1,
            "exactly one racer should win the reclaim + acquire"
        );
        assert_eq!(winners.len(), 1);
        assert!(
            path.exists(),
            "the winner's live lock must survive every other racer's reclaim attempt"
        );
        drop(winners);
    });
}
