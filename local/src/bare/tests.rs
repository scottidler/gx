use super::*;
use crate::test_utils::{create_bare_container, create_minimal_test_repo};
use tempfile::TempDir;

#[test]
fn test_is_bare_container_detects_real_container() {
    let temp = TempDir::new().unwrap();
    let container = create_bare_container(temp.path(), "gx", "scottidler/gx");
    assert!(is_bare_container(&container));
}

#[test]
fn test_is_bare_container_rejects_flat_repo() {
    let temp = TempDir::new().unwrap();
    let flat = create_minimal_test_repo(temp.path(), "flat");
    // A flat repo has a `.git` directory and no `.bare/`.
    assert!(!is_bare_container(&flat));
}

#[test]
fn test_is_bare_container_rejects_lone_bare_dir() {
    // A bare `.bare/` directory WITHOUT the `.git` pointer file is not a
    // container - detection is strict (the design doc's open question).
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("loner");
    std::fs::create_dir_all(dir.join(".bare")).unwrap();
    assert!(!is_bare_container(&dir));
}

#[test]
fn test_is_bare_container_rejects_git_dir_named_bare_pointer() {
    // A `.git` *directory* (not a pointer file) alongside a `.bare/` dir is not
    // a container either - the pointer must be a regular file.
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("weird");
    std::fs::create_dir_all(dir.join(".bare")).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    assert!(!is_bare_container(&dir));
}

#[test]
fn test_is_git_path_covers_all_layouts() {
    let temp = TempDir::new().unwrap();
    let flat = create_minimal_test_repo(temp.path(), "flat");
    let container = create_bare_container(temp.path(), "gx", "scottidler/gx");

    assert!(is_git_path(&flat)); // `.git` directory
    assert!(is_git_path(&container)); // `.git` pointer + `.bare/`
    assert!(is_git_path(&container.join("main"))); // linked worktree (`.git` file)
    assert!(!is_git_path(temp.path())); // plain directory
}

#[test]
fn test_resolve_worktrees_lists_default_and_bare() {
    let temp = TempDir::new().unwrap();
    let container = create_bare_container(temp.path(), "gx", "scottidler/gx");

    let worktrees = resolve_worktrees(&container).unwrap();
    // Exactly one real checkout (`main`) plus the `.bare` entry.
    let checkouts: Vec<_> = worktrees.iter().filter(|w| !w.bare).collect();
    assert_eq!(checkouts.len(), 1);
    assert_eq!(checkouts[0].branch.as_deref(), Some("main"));
    assert!(checkouts[0].path.ends_with("main"));
    assert!(worktrees.iter().any(|w| w.bare));
}

#[test]
fn test_parse_worktree_porcelain_shapes() {
    // Detached HEAD, a locked branch worktree, and the bare entry.
    let sample = "worktree /repo/.bare\nbare\n\n\
                  worktree /repo/main\nHEAD abc123\nbranch refs/heads/main\n\n\
                  worktree /repo/detached\nHEAD def456\ndetached\n";
    let rows = parse_worktree_porcelain(sample);
    assert_eq!(rows.len(), 3);

    assert!(rows[0].bare);
    assert_eq!(rows[0].branch, None);

    assert!(!rows[1].bare);
    assert_eq!(rows[1].branch.as_deref(), Some("main"));

    assert!(!rows[2].bare);
    assert_eq!(rows[2].branch, None); // detached HEAD carries no branch
}

#[test]
fn test_default_worktree_resolves_to_default_branch_checkout() {
    let temp = TempDir::new().unwrap();
    let container = create_bare_container(temp.path(), "gx", "scottidler/gx");

    let worktree = default_worktree(&container).unwrap();
    assert!(worktree.ends_with("main"));
    // The resolved worktree is a real checkout: git status runs there.
    assert!(worktree.join(".git").is_file());
}

#[test]
fn test_origin_url_reads_slug_from_container() {
    let temp = TempDir::new().unwrap();
    let container = create_bare_container(temp.path(), "gx", "scottidler/gx");

    let url = origin_url(&container).unwrap();
    assert_eq!(url, "git@github.com:scottidler/gx.git");
}

#[test]
fn test_origin_url_errors_without_origin() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("noremote");
    std::fs::create_dir_all(&dir).unwrap();
    crate::test_utils::run_git_command(&["init", "--quiet"], &dir);
    assert!(origin_url(&dir).is_err());
}
