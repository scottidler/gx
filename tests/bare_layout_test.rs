//! Integration tests: gx discovery over a scan set that mixes flat repos and a
//! bare container. A container must count as ONE logical repo (its default
//! worktree), never fan out over its N worktrees.

use gx::repo::{discover_repos, Layout};
use gx::test_utils::{create_bare_container, create_minimal_test_repo};
use tempfile::TempDir;

#[test]
fn test_bare_container_counts_as_one_repo() {
    let temp = TempDir::new().unwrap();
    // Two flat repos plus one bare container in the same scan set.
    create_minimal_test_repo(temp.path(), "flat-a");
    create_minimal_test_repo(temp.path(), "flat-b");
    create_bare_container(temp.path(), "gx", "scottidler/gx");

    let repos = discover_repos(temp.path(), 4, &[]).unwrap();
    let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();

    // Three logical repos total: the container is exactly one.
    assert_eq!(repos.len(), 3, "discovered: {names:?}");
    assert!(names.contains(&"flat-a"));
    assert!(names.contains(&"flat-b"));
    // The container's logical name is the container dir name, not "main".
    assert!(names.contains(&"gx"));
    assert!(!names.contains(&"main"));
    assert!(!names.contains(&".bare"));

    // Layout is known at discovery: flat repos are Flat, the container is
    // Bare - the mixed fixture yields exactly the two-layout set {Flat, Bare}.
    let layouts: Vec<Layout> = repos.iter().map(|r| r.layout).collect();
    assert!(layouts.contains(&Layout::Flat));
    assert!(layouts.contains(&Layout::Bare));
    assert!(!layouts.contains(&Layout::Unknown));
    assert_eq!(
        layouts.iter().filter(|l| **l == Layout::Flat).count(),
        2,
        "both flat repos classified Flat"
    );
    assert_eq!(
        layouts.iter().filter(|l| **l == Layout::Bare).count(),
        1,
        "the container classified Bare exactly once"
    );

    let flat_a = repos.iter().find(|r| r.name == "flat-a").unwrap();
    assert_eq!(flat_a.layout, Layout::Flat);
    let flat_b = repos.iter().find(|r| r.name == "flat-b").unwrap();
    assert_eq!(flat_b.layout, Layout::Flat);
    let gx_repo = repos.iter().find(|r| r.name == "gx").unwrap();
    assert_eq!(gx_repo.layout, Layout::Bare);
}

#[test]
fn test_bare_container_repo_points_at_default_worktree() {
    let temp = TempDir::new().unwrap();
    let container = create_bare_container(temp.path(), "gx", "scottidler/gx");

    let repos = discover_repos(temp.path(), 4, &[]).unwrap();
    let repo = repos
        .iter()
        .find(|r| r.name == "gx")
        .expect("container discovered as repo 'gx'");

    // git operations run in the default worktree, not the container root.
    assert_eq!(repo.path, container.join("main"));
    assert!(
        repo.path.join(".git").is_file(),
        "worktree has a .git pointer file"
    );

    // Slug is resolved from origin, exactly as a flat repo of the same name.
    assert_eq!(repo.slug, "scottidler/gx");

    // A bare container's repo is always classified Bare.
    assert_eq!(repo.layout, Layout::Bare);

    // git actually runs at that path (the container root would fail here).
    let output = gx::test_utils::run_git_command(&["status", "--porcelain"], &repo.path);
    assert!(
        output.status.success(),
        "git status must succeed in the default worktree"
    );
}
