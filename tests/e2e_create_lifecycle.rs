//! End-to-end lifecycle test ([A31]): a temp "org" of three repos - one
//! master-default, one dirty, one named `reporting` (which the old discovery
//! heuristic wrongly hid) - exercised through create -> state, fully offline
//! (bare-repo remotes, no GitHub / no `--pr`).

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

fn gx_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("gx");
    path
}

/// Create a repo at `workspace/<name>` with a bare remote under `remotes`,
/// on default branch `branch`, containing a `data.md` with "old value".
fn make_repo(workspace: &Path, remotes: &Path, name: &str, branch: &str) -> std::path::PathBuf {
    let bare = remotes.join(format!("{name}.git"));
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );

    let repo = workspace.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(
        &["init", "--quiet", &format!("--initial-branch={branch}")],
        &repo,
    );
    git(&["config", "user.email", "t@e.com"], &repo);
    git(&["config", "user.name", "T"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);
    std::fs::write(repo.join("data.md"), "old value\n").unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", branch], &repo);
    // Set origin/HEAD so default-branch resolution finds `branch`.
    git(&["remote", "set-head", "origin", branch], &repo);
    repo
}

#[test]
fn test_create_lifecycle_offline() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();

    // Three diverse repos.
    make_repo(workspace.path(), remotes.path(), "frontend", "main");
    let dirty = make_repo(workspace.path(), remotes.path(), "dirty-repo", "main");
    make_repo(workspace.path(), remotes.path(), "reporting", "master");

    // Make dirty-repo actually dirty (untracked + modified).
    std::fs::write(dirty.join("data.md"), "old value\nlocal edit\n").unwrap();
    std::fs::write(dirty.join("wip.txt"), "work in progress").unwrap();

    // Run: gx create over all repos, substituting old -> new in *.md, committing.
    let output = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "--files",
            "**/*.md",
            "--commit",
            "e2e: old to new",
            "--yes",
            "sub",
            "old",
            "new",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .output()
        .expect("gx failed to spawn");
    assert!(
        output.status.success(),
        "gx create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Each repo's bare remote now has a GX- branch pushed.
    for name in ["frontend.git", "dirty-repo.git", "reporting.git"] {
        let bare = remotes.path().join(name);
        let refs = Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "branch",
                "--list",
                "GX-*",
            ])
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&refs.stdout);
        assert!(
            branches.contains("GX-"),
            "{name} should have a pushed GX- branch, got: {branches:?}"
        );
    }

    // A change-state file was written under XDG_DATA_HOME.
    let changes = data_home.path().join("gx").join("changes");
    let files: Vec<_> = std::fs::read_dir(&changes)
        .expect("changes dir should exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    assert_eq!(files.len(), 1, "exactly one change-state file expected");
    let state_json = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(state_json.contains("frontend"));
    assert!(state_json.contains("reporting"));
    assert!(state_json.contains("dirty-repo"));

    // The dirty repo's WIP survived (stash applied back on the original branch).
    assert_eq!(
        std::fs::read_to_string(dirty.join("wip.txt")).unwrap(),
        "work in progress"
    );

    // cleanup --list reads the state back without error.
    let cleanup = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "cleanup",
            "--list",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .output()
        .expect("gx cleanup failed to spawn");
    assert!(cleanup.status.success(), "gx cleanup --list failed");
}
