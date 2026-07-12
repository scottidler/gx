use super::*;
use crate::test_utils::env_lock;

/// The hook is inert when the env var is unset, and inert when it names a
/// DIFFERENT point. (The abort path is exercised out-of-process by the
/// crash-injection e2e; calling it here would kill the test process.)
#[test]
fn test_maybe_crash_inert_without_matching_env() {
    let _guard = env_lock();
    let prior = std::env::var(CRASH_ENV).ok();

    // Unset: every point is a no-op.
    unsafe { std::env::remove_var(CRASH_ENV) };
    for point in CRASH_POINTS {
        maybe_crash(point); // returns => did not abort
    }

    // Set to a point we never call here: still a no-op for the others.
    unsafe { std::env::set_var(CRASH_ENV, "after-push") };
    maybe_crash("after-stash");
    maybe_crash("before-push");
    maybe_crash("mid-finalize");

    match prior {
        Some(v) => unsafe { std::env::set_var(CRASH_ENV, v) },
        None => unsafe { std::env::remove_var(CRASH_ENV) },
    }
}

/// The vocabulary is exactly the six documented phase boundaries, in order.
#[test]
fn test_crash_points_vocabulary() {
    assert_eq!(
        CRASH_POINTS,
        &[
            "after-stash",
            "after-branch",
            "after-commit",
            "before-push",
            "after-push",
            "mid-finalize",
        ]
    );
}

/// Recursively collect `(relative-path, contents)` for every `.rs` file under
/// `src/`.
fn all_src_files() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                out.push((rel, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    walk(&root.join("src"), root, &mut out);
    out
}

/// Grep-guard (Risks table: "hook is a no-op unless the env var is set;
/// grep-guard test asserts no other call sites"): the crash hook is wired at
/// EXACTLY the six named points, only in the two pipeline files, and the abort
/// lives only inside the env-var-guarded hook.
#[test]
fn test_crash_hook_call_sites_are_exactly_the_wired_points() {
    // Only PRODUCTION source is scanned: test files (this one included)
    // legitimately mention the call string inside assertions/format strings, and
    // the hook's own module defines it. Neither is a production call site.
    let files: Vec<(String, String)> = all_src_files()
        .into_iter()
        .filter(|(rel, _)| !rel.ends_with("tests.rs") && rel != "src/crash.rs")
        .collect();

    // Every production call site uses the fully-qualified `crate::crash::maybe_crash(`.
    let mut sites: Vec<(String, String)> = Vec::new(); // (file, point)
    for (rel, src) in &files {
        for point in CRASH_POINTS {
            let needle = format!(r#"crate::crash::maybe_crash("{point}")"#);
            for _ in src.matches(&needle) {
                sites.push((rel.clone(), (*point).to_string()));
            }
        }
        // No fully-qualified call may use a point OUTSIDE the vocabulary.
        let total_calls = src.matches("crate::crash::maybe_crash(").count();
        let known_calls: usize = CRASH_POINTS
            .iter()
            .map(|p| {
                src.matches(&format!(r#"crate::crash::maybe_crash("{p}")"#))
                    .count()
            })
            .sum();
        assert_eq!(
            total_calls, known_calls,
            "{rel} has a crash call with an unknown point (not in CRASH_POINTS)"
        );
    }

    // Exactly six call sites, one per named point.
    assert_eq!(
        sites.len(),
        CRASH_POINTS.len(),
        "expected exactly {} crash call sites, got: {sites:?}",
        CRASH_POINTS.len()
    );
    for point in CRASH_POINTS {
        let count = sites.iter().filter(|(_, p)| p == point).count();
        assert_eq!(count, 1, "crash point {point:?} must be wired exactly once");
    }

    // Only the two pipeline files may wire the hook.
    for (file, point) in &sites {
        assert!(
            file == "src/create.rs" || file == "src/transaction.rs",
            "unexpected crash call site in {file} for {point}"
        );
    }
    assert_eq!(
        sites.iter().filter(|(f, _)| f == "src/create.rs").count(),
        5,
        "create.rs must wire five crash points (all but mid-finalize)"
    );
    assert_eq!(
        sites
            .iter()
            .filter(|(f, _)| f == "src/transaction.rs")
            .count(),
        1,
        "transaction.rs must wire exactly mid-finalize"
    );

    // The abort lives ONLY inside the hook itself. Any other file that aborts
    // could crash production unconditionally, which the hook exists to avoid.
    for (rel, src) in &files {
        if rel == "src/crash.rs" {
            continue;
        }
        assert!(
            !src.contains("std::process::abort("),
            "{rel} must not call std::process::abort(); only the guarded crash hook may"
        );
    }
}
