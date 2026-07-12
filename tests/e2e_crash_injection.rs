//! Crash-injection matrix (design Phase 8, F15): kill a REAL `gx create`
//! process at each of the six phase boundaries via the `GX_CRASH_POINT` hook,
//! then prove `gx rollback` recovers every one. For each point we assert:
//!
//! - `gx rollback list` shows the RIGHT phase for the crash boundary,
//! - `gx rollback execute` returns the worktree byte-identical to pre-run
//!   (content AND git's TRACKED mode -- see the MODE CAVEAT below), and
//! - the remote state is correct: the pushed branch is RETAINED for
//!   `after-push`/`mid-finalize` (recovery keeps shared work), ABSENT for
//!   `before-push` (the ls-remote probe finds it absent -> full reverse) and
//!   for the earlier `mutating`-phase points.
//!
//! MODE CAVEAT: git only ever tracks the executable bit (100755/100644), never
//! the finer rwx bits, and `finalize()`'s branch switch recreates changed files
//! under the process umask. So byte-identity is asserted via git's own tracked
//! state (`git ls-files -s`: mode + blob sha per tracked file) plus HEAD and the
//! working-tree status -- NOT raw `std::fs` permission bits, which would
//! false-fail on umask group-bit drift.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_stdout(args: &[&str], dir: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn gx_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("gx");
    path
}

/// A dirty repo at `workspace/app` with a bare remote at `remotes/app.git`,
/// containing a tracked `data.md` ("old value"), a tracked executable
/// `run.sh` (0755, to prove git's tracked mode survives), and an UNTRACKED
/// `wip.txt` so every run stashes WIP (arming the `after-stash` point and
/// proving the stash is restored on recovery).
#[cfg(unix)]
fn make_fixture(workspace: &Path, remotes: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bare = remotes.join("app.git");
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );

    let repo = workspace.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &repo);
    git(&["config", "user.email", "t@e.com"], &repo);
    git(&["config", "user.name", "T"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);

    std::fs::write(repo.join("data.md"), "old value\n").unwrap();
    let script = repo.join("run.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);

    // Uncommitted WIP so the create pipeline stashes (arms `after-stash`).
    std::fs::write(repo.join("wip.txt"), "WIP\n").unwrap();

    repo
}

/// A byte-identity snapshot of the working tree that is robust to git's umask
/// mode drift: HEAD sha, sorted porcelain status, `git ls-files -s` (tracked
/// mode + blob sha per file), and the content of the two data files.
fn worktree_snapshot(repo: &Path) -> String {
    let head = git_stdout(&["rev-parse", "HEAD"], repo);
    let mut porcelain: Vec<String> = git_stdout(&["status", "--porcelain"], repo)
        .lines()
        .map(|l| l.to_string())
        .collect();
    porcelain.sort();
    let tracked = git_stdout(&["ls-files", "-s"], repo);
    let data = std::fs::read_to_string(repo.join("data.md")).unwrap_or_default();
    let wip = std::fs::read_to_string(repo.join("wip.txt")).unwrap_or_default();
    format!(
        "HEAD={head}\nPORCELAIN=\n{}\nTRACKED=\n{tracked}\nDATA={data}\nWIP={wip}",
        porcelain.join("\n")
    )
}

/// True if `branch` exists on the bare remote at `remotes/app.git`.
fn branch_on_bare(remotes: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            remotes.join("app.git").to_str().unwrap(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}

/// The single recovery transaction id + its recorded phase under `data_home`.
fn sole_recovery(data_home: &Path) -> (String, String) {
    let dir = data_home.join("gx").join("recovery");
    let files: Vec<_> = std::fs::read_dir(&dir)
        .expect("recovery dir must exist after a crash")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        files.len(),
        1,
        "exactly one recovery file expected after a single-repo crash"
    );
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
    let tx_id = json["transaction_id"].as_str().unwrap().to_string();
    let phase = json["phase"].as_str().unwrap().to_string();
    (tx_id, phase)
}

#[test]
#[cfg(unix)]
fn test_crash_matrix_all_six_points() {
    let change_id = "GX-crash";
    // (crash point, expected recorded phase, remote branch retained after recovery)
    let matrix = [
        ("after-stash", "mutating", false),
        ("after-branch", "mutating", false),
        ("after-commit", "mutating", false),
        ("before-push", "pushing", false),
        ("after-push", "pushed", true),
        ("mid-finalize", "finalizing", true),
    ];

    for (point, expected_phase, remote_retained) in matrix {
        // Fresh org + data dir per point so recovery files never collide.
        let workspace = TempDir::new().unwrap();
        let remotes = TempDir::new().unwrap();
        let data_home = TempDir::new().unwrap();
        let repo = make_fixture(workspace.path(), remotes.path());

        let before = worktree_snapshot(&repo);

        // 1. Spawn a REAL gx create and crash it at `point`.
        let create = Command::new(gx_binary())
            .args([
                "--cwd",
                workspace.path().to_str().unwrap(),
                "--log-level",
                "off",
                "create",
                "--files",
                "**/*.md",
                "--change-id",
                change_id,
                "--commit",
                "crash: old to new",
                "--yes",
                "sub",
                "old",
                "new",
            ])
            .env("XDG_DATA_HOME", data_home.path())
            .env("GX_CRASH_POINT", point)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("gx create failed to spawn");
        assert!(
            !create.status.success(),
            "[{point}] gx create must die at the crash point, not exit cleanly"
        );

        // 2. The recovery file records the RIGHT phase for this boundary.
        let (tx_id, phase) = sole_recovery(data_home.path());
        assert_eq!(
            phase, expected_phase,
            "[{point}] recovery file must record phase {expected_phase}"
        );

        // 3. `gx rollback list` surfaces that phase.
        let list = Command::new(gx_binary())
            .args(["rollback", "list"])
            .env("XDG_DATA_HOME", data_home.path())
            .output()
            .expect("gx rollback list failed to spawn");
        assert!(list.status.success(), "[{point}] gx rollback list failed");
        let list_out = String::from_utf8_lossy(&list.stdout);
        assert!(
            list_out.contains(&format!("Phase: {expected_phase}")),
            "[{point}] rollback list must show 'Phase: {expected_phase}', got:\n{list_out}"
        );

        // 4. `gx rollback execute` recovers.
        let exec = Command::new(gx_binary())
            .args(["rollback", "execute", &tx_id, "--force", "--yes"])
            .env("XDG_DATA_HOME", data_home.path())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("gx rollback execute failed to spawn");
        assert!(
            exec.status.success(),
            "[{point}] gx rollback execute failed: {}",
            String::from_utf8_lossy(&exec.stderr)
        );

        // 5. Worktree byte-identical to pre-run (content + git-tracked mode).
        let after = worktree_snapshot(&repo);
        assert_eq!(
            before, after,
            "[{point}] worktree must be byte-identical to pre-run after recovery"
        );

        // 6. Remote state correct for the phase.
        assert_eq!(
            branch_on_bare(remotes.path(), change_id),
            remote_retained,
            "[{point}] remote branch retention mismatch (expected retained={remote_retained})"
        );

        // 7. The local GX branch mirrors the remote decision: retained under
        //    keep-work, gone under full reverse.
        let local = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{change_id}")])
            .current_dir(&repo)
            .output()
            .unwrap()
            .status
            .success();
        assert_eq!(
            local, remote_retained,
            "[{point}] local GX branch retention must match the phase decision"
        );
    }
}
