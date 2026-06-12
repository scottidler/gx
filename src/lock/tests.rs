use super::*;
use std::sync::Mutex;
use tempfile::TempDir;

// Serialize env-var mutation across tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_data_home<F: FnOnce()>(dir: &Path, f: F) {
    let guard = ENV_LOCK.lock().unwrap();
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
