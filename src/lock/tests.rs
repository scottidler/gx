use super::*;
use crate::test_utils::env_lock;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
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
fn test_lock_acquire_and_holder_metadata() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let lock = RepoLock::acquire(repo.path()).unwrap();
        let path = lock.path.clone();
        assert!(path.exists(), "lock file should exist while held");
        // Holder JSON is written AFTER the lock is held, for error messages.
        let holder = read_holder(&path);
        assert!(
            holder.contains(&format!("pid {}", std::process::id())),
            "holder metadata should name this process: {holder}"
        );
        drop(lock);
        // Under flock the file is NEVER unlinked on drop; it persists,
        // unlocked and reacquirable.
        assert!(
            path.exists(),
            "lock file must persist after drop (never unlinked)"
        );
    });
}

#[test]
fn test_second_acquire_fails_fast() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let _held = RepoLock::acquire(repo.path()).unwrap();
        // Same-process double-open: a separate open file description contends
        // on the same flock and returns WouldBlock -> fail-fast error.
        let second = RepoLock::acquire(repo.path());
        let err = match second {
            Ok(_) => panic!("second lock on same repo must fail"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("locked") && msg.contains("pid"),
            "error must name the live holder: {msg}"
        );
    });
}

#[test]
fn test_lock_reacquirable_after_holder_drops() {
    // Liveness (replaces the old staleness-reclaim test): a lock whose holder
    // is gone -- here the guard dropped, so the kernel released the flock -- is
    // reacquirable, with no reclaim machinery. The cross-process kill -9 analog
    // lives in tests/lock_contention_test.rs.
    //
    // Phase 5 flock-fix: under `otto ci`'s full-parallelism test load (dozens
    // of threads hammering the VFS with `open`/`flock`/`close` at once),
    // `close(2)`'s `flock` release is occasionally not yet visible to an
    // `open`+`try_lock` issued IMMEDIATELY after on the same thread -- a
    // harness/kernel-scheduling artifact under load, reproduced repeatedly by
    // running the full lib suite back to back (never reproduces running this
    // module alone). This is TIMING, not a production race: gx's real usage
    // never re-acquires a just-dropped lock in the same instant. Bounded-poll
    // the reacquire, mirroring `test_spawned_child_does_not_inherit_lock_fd`'s
    // existing poll-with-deadline idiom in this same file -- the LOGICAL
    // assertion (a dropped lock becomes reacquirable, no reclaim needed) is
    // unchanged; only the wall-clock margin widens.
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let first = RepoLock::acquire(repo.path()).unwrap();
        drop(first);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut last_err = None;
        let mut acquired = false;
        while std::time::Instant::now() < deadline {
            match RepoLock::acquire(repo.path()) {
                Ok(_second) => {
                    acquired = true;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        assert!(
            acquired,
            "a lock whose holder is gone must be reacquirable within the window: {last_err:?}"
        );
    });
}

#[test]
fn test_drop_never_unlinks_and_reopens_same_inode() {
    // Regression (panel must-fix 2026-07-12): Drop must NEVER unlink the lock
    // file. If it did, the classic 2-winner interleave reopens -- A holds
    // (inode1), B has a pending lock on inode1, A drops+unlinks, C creates a
    // FRESH file (inode2) at the same path and locks it while B still holds
    // inode1 -> two holders. Not unlinking keeps the path bound to ONE inode,
    // so every reopen of the path contends on the same lock (single-holder).
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let path = lock_path_for(repo.path()).unwrap();

        let a = RepoLock::acquire(repo.path()).unwrap();
        let inode_a = fs::metadata(&path).unwrap().ino();
        drop(a);

        assert!(path.exists(), "Drop must not unlink the lock file");
        let inode_after_drop = fs::metadata(&path).unwrap().ino();
        assert_eq!(
            inode_a, inode_after_drop,
            "the inode must not change on drop (file left in place)"
        );

        // A fresh acquire reopens the SAME inode -- no fresh-inode divergence.
        // Phase 5 flock-fix: bounded-poll the reacquire (see
        // `test_lock_reacquirable_after_holder_drops` for why: under `otto
        // ci`'s full parallel test load, a `close`'s flock release is
        // occasionally not yet visible to an immediately-following
        // `open`+`try_lock` on the same thread -- timing/harness, not a
        // production race). The logical assertion (same inode, no
        // fresh-inode divergence) is unchanged.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut b = None;
        let mut last_err = None;
        while std::time::Instant::now() < deadline {
            match RepoLock::acquire(repo.path()) {
                Ok(lock) => {
                    b = Some(lock);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        let b =
            b.unwrap_or_else(|| panic!("reacquire must succeed within the window: {last_err:?}"));
        let inode_b = fs::metadata(&path).unwrap().ino();
        assert_eq!(
            inode_a, inode_b,
            "reacquire must resolve to the same inode, not a fresh one"
        );
        drop(b);
    });
}

#[test]
fn test_contention_stress_exactly_one_winner_across_many_runs() {
    // Success criterion: contention stress shows exactly one winner per run.
    // Many threads each do a fresh open + non-blocking flock at a barrier;
    // winners HOLD their guard until the round ends, so at most one lock is
    // ever held at an instant and every other contender fails fast (WouldBlock).
    // Repeated to shake out ordering-dependent races.
    const RUNS: usize = 100;
    const RACERS: usize = 8;
    let data = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        for run in 0..RUNS {
            // Fresh repo (=> fresh lock path) per run so a leaked fd from a
            // PRIOR run -- e.g. a concurrent suite test that fork()ed a
            // subprocess mid-run and transiently dup'd this run's lock fd
            // before its exec() closed it (O_CLOEXEC) -- can never bleed into
            // the next run's contention. Within a single run, flock still
            // guarantees exactly one winner regardless of any such transient.
            let repo = TempDir::new().unwrap();
            let barrier = Arc::new(Barrier::new(RACERS));
            let successes = Arc::new(AtomicUsize::new(0));
            let repo_path = Arc::new(repo.path().to_path_buf());
            let (tx, rx) = std::sync::mpsc::channel();

            let handles: Vec<_> = (0..RACERS)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    let successes = Arc::clone(&successes);
                    let repo_path = Arc::clone(&repo_path);
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        if let Ok(lock) = RepoLock::acquire(&repo_path) {
                            successes.fetch_add(1, Ordering::SeqCst);
                            // Hold the guard until the round ends so no other
                            // racer can win by the holder having already released.
                            tx.send(lock).ok();
                        }
                    })
                })
                .collect();
            drop(tx);
            for h in handles {
                h.join().unwrap();
            }
            let winners: Vec<RepoLock> = rx.iter().collect();

            assert_eq!(
                successes.load(Ordering::SeqCst),
                1,
                "run {run}: exactly one racer must win the lock"
            );
            assert_eq!(winners.len(), 1, "run {run}: exactly one live guard");
            // Guards drop here, releasing the lock before the next run.
        }
    });
}

#[test]
fn test_spawned_child_does_not_inherit_lock_fd() {
    // Success criterion: a spawned child must NOT inherit the lock fd
    // (O_CLOEXEC is the Rust default, ASSERTED here not assumed). Proof: hold
    // the lock, spawn a long-lived child, then drop the guard. If the child had
    // inherited the fd, the flock would stay held (the open file description
    // outlives the parent's close) and a reacquire would fail. With O_CLOEXEC
    // the child never got the fd, so the drop fully releases the lock and a
    // fresh acquire succeeds while the child is still alive.
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let lock = RepoLock::acquire(repo.path()).unwrap();

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sleep child");

        // Release the parent's lock while the child is still running.
        drop(lock);

        // Poll the reacquire for a bounded window. If our `sleep 30` child had
        // inherited the lock fd (O_CLOEXEC missing / broken), it would hold the
        // flock for its full 30s life and this would NEVER succeed inside the
        // window -> the test bites. A brief failure from an UNRELATED concurrent
        // suite test fork()ing a subprocess (which transiently dup's every open
        // fd until its exec() closes the O_CLOEXEC ones, milliseconds later) is
        // tolerated: the window is orders of magnitude longer than that transient
        // yet far shorter than the child's 30s hold.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut ok = false;
        while std::time::Instant::now() < deadline {
            if RepoLock::acquire(repo.path()).is_ok() {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        // Clean up the child regardless of the assertion outcome.
        child.kill().ok();
        child.wait().ok();

        assert!(
            ok,
            "child inherited the lock fd (missing O_CLOEXEC): reacquire never \
             succeeded within the window while the child still held the flock"
        );
    });
}

#[test]
fn test_fnv1a_stable() {
    assert_eq!(fnv1a_hex(b"hello"), fnv1a_hex(b"hello"));
    assert_ne!(fnv1a_hex(b"a"), fnv1a_hex(b"b"));
    assert_eq!(fnv1a_hex(b"").len(), 16);
}

#[test]
fn test_change_lock_acquire_and_persists() {
    let data = TempDir::new().unwrap();
    with_data_home(data.path(), || {
        let lock = ChangeLock::acquire("GX-2026-07-11T00-00-00").unwrap();
        let path = lock.path.clone();
        assert!(path.exists(), "change lock file should exist while held");
        drop(lock);
        assert!(
            path.exists(),
            "change lock file must persist after drop (never unlinked)"
        );
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
