use super::*;
use local::test_utils::run_git_command;
use std::fs;
use tempfile::TempDir;

/// Initialize a git repo in `dir` with identity configured (fail-loud).
fn git_init(dir: &Path) {
    let out = run_git_command(&["init", "--quiet"], dir);
    assert!(out.status.success(), "git init failed");
    run_git_command(&["config", "user.email", "test@example.com"], dir);
    run_git_command(&["config", "user.name", "Test User"], dir);
    run_git_command(&["config", "commit.gpgsign", "false"], dir);
}

/// Stage everything and commit, fail-loud.
fn git_commit_all(dir: &Path, message: &str) {
    let add = run_git_command(&["add", "-A"], dir);
    assert!(add.status.success(), "git add failed");
    let commit = run_git_command(&["commit", "--quiet", "-m", message], dir);
    assert!(commit.status.success(), "git commit failed");
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[test]
fn test_candidates_tracked_only_excludes_git_and_untracked() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    git_init(repo);

    write(&repo.join("tracked.txt"), "tracked");
    write(&repo.join("src/main.rs"), "fn main() {}");
    git_commit_all(repo, "initial");

    // Untracked file - must never be a candidate.
    write(&repo.join("untracked.txt"), "wip");

    let candidates = FileSet::candidates(repo).unwrap();
    let names: Vec<String> = candidates
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(names.contains(&"tracked.txt".to_string()));
    assert!(names.iter().any(|n| n == "src/main.rs"));
    assert!(!names.iter().any(|n| n.contains("untracked")));
    // .git internals are never listed by git ls-files.
    assert!(!names.iter().any(|n| n.contains(".git")));
}

#[test]
fn test_candidates_excludes_gitignored() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    git_init(repo);

    write(&repo.join(".gitignore"), "ignored.txt\n");
    write(&repo.join("kept.txt"), "kept");
    write(&repo.join("ignored.txt"), "should be ignored");
    git_commit_all(repo, "initial");

    let candidates = FileSet::candidates(repo).unwrap();
    let names: Vec<String> = candidates
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(names.contains(&"kept.txt".to_string()));
    assert!(names.contains(&".gitignore".to_string()));
    assert!(!names.contains(&"ignored.txt".to_string()));
}

#[test]
fn test_matching_glob_never_matches_git_or_untracked() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    git_init(repo);

    write(&repo.join("a.txt"), "a");
    write(&repo.join("nested/b.txt"), "b");
    git_commit_all(repo, "initial");

    write(&repo.join("untracked.txt"), "wip");

    let matched = FileSet::matching_any(repo, &["**/*".to_string()]).unwrap();
    let names: Vec<String> = matched
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(names.contains(&"a.txt".to_string()));
    assert!(names.iter().any(|n| n == "nested/b.txt"));
    assert!(!names.iter().any(|n| n.contains("untracked")));
    assert!(!names.iter().any(|n| n.contains(".git")));
}

#[test]
fn test_matching_tracked_dotfile_pattern() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    git_init(repo);

    write(&repo.join(".github/workflows/ci.yml"), "name: ci");
    write(&repo.join(".github/workflows/release.yml"), "name: release");
    write(&repo.join("README.md"), "# readme");
    git_commit_all(repo, "initial");

    let matched = FileSet::matching_any(repo, &[".github/workflows/*.yml".to_string()]).unwrap();
    let names: Vec<String> = matched
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert_eq!(names.len(), 2);
    assert!(names.iter().any(|n| n == ".github/workflows/ci.yml"));
    assert!(names.iter().any(|n| n == ".github/workflows/release.yml"));
}

#[test]
fn test_matching_star_does_not_cross_directories() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    git_init(repo);

    write(&repo.join("top.txt"), "top");
    write(&repo.join("sub/inner.txt"), "inner");
    git_commit_all(repo, "initial");

    let matched = FileSet::matching_any(repo, &["*.txt".to_string()]).unwrap();
    let names: Vec<String> = matched
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert_eq!(names, vec!["top.txt".to_string()]);
}

#[test]
fn test_candidates_excludes_symlinks() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    git_init(repo);

    write(&repo.join("real.txt"), "real");
    #[cfg(unix)]
    std::os::unix::fs::symlink("real.txt", repo.join("link.txt")).unwrap();
    git_commit_all(repo, "initial");

    let candidates = FileSet::candidates(repo).unwrap();
    let names: Vec<String> = candidates
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(names.contains(&"real.txt".to_string()));
    #[cfg(unix)]
    assert!(!names.contains(&"link.txt".to_string()));
}

#[test]
fn test_apply_substitution_skips_binary() {
    let temp = TempDir::new().unwrap();
    let file_path = temp.path().join("data.bin");
    // Invalid UTF-8 bytes.
    fs::write(&file_path, [0xff, 0xfe, 0x00, 0x01, 0x80]).unwrap();

    let result = apply_substitution_to_file(&file_path, "x", "y", 1).unwrap();
    assert!(matches!(
        result,
        local::diff::SubstitutionResult::SkippedBinary
    ));

    let regex_result = apply_regex_to_file(&file_path, "x", "y", 1).unwrap();
    assert!(matches!(
        regex_result,
        local::diff::SubstitutionResult::SkippedBinary
    ));
}

#[test]
fn test_match_count_multi_match() {
    let temp = TempDir::new().unwrap();
    let file_path = temp.path().join("multi.txt");
    fs::write(&file_path, "foo foo foo\nbar foo").unwrap();

    let result = apply_substitution_to_file(&file_path, "foo", "qux", 1).unwrap();
    if let local::diff::SubstitutionResult::Changed { matches, .. } = result {
        assert_eq!(matches, 4);
    } else {
        panic!("expected Changed");
    }
}

#[test]
fn test_atomic_write_creates_and_overwrites() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("nested").join("file.txt");

    atomic_write(&path, b"first").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "first");

    atomic_write(&path, b"second").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
}

#[test]
#[cfg(unix)]
fn test_atomic_write_preserves_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("script.sh");
    fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

    // Before F3, atomic_write's NamedTempFile persisted at 0600 regardless of
    // the target's existing mode - this proves the rewrite preserves it.
    atomic_write(&path, b"#!/bin/sh\necho bye\n").unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(
        mode, 0o755,
        "atomic_write must preserve the existing target's mode"
    );
}

// Raw FFI to libc's `umask(2)`: the design explicitly rules out a new crate
// dependency for mode handling, and this is the only syscall needed to prove
// `atomic_write`'s new-file mode is set explicitly rather than umask-derived.
#[cfg(unix)]
extern "C" {
    fn umask(mask: u32) -> u32;
}

#[test]
#[cfg(unix)]
fn test_atomic_write_new_file_mode_under_restrictive_umask() {
    use std::os::unix::fs::PermissionsExt;

    // umask is process-global state; serialize with every other test that
    // mutates shared environment/process state.
    let _guard = local::test_utils::env_lock();

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("new.txt");

    // A maximally restrictive umask: a raw open()/creat() would leave the
    // temp file well below 0644. atomic_write's explicit chmod must win.
    let prior = unsafe { umask(0o077) };
    let result = atomic_write(&path, b"content");
    unsafe { umask(prior) };
    result.unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(
        mode, 0o644,
        "a brand-new file must be 0644 regardless of the process umask"
    );
}

#[test]
fn test_validate_new_file_path_accepts_normal() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    let full = validate_new_file_path(repo, "docs/new.md").unwrap();
    assert_eq!(full, repo.join("docs/new.md"));
}

#[test]
fn test_validate_new_file_path_rejects_absolute() {
    let temp = TempDir::new().unwrap();
    assert!(validate_new_file_path(temp.path(), "/etc/passwd").is_err());
}

#[test]
fn test_validate_new_file_path_rejects_parent_traversal() {
    let temp = TempDir::new().unwrap();
    assert!(validate_new_file_path(temp.path(), "../escape.txt").is_err());
    assert!(validate_new_file_path(temp.path(), "a/../../escape.txt").is_err());
}

#[test]
fn test_validate_new_file_path_rejects_dot_git() {
    let temp = TempDir::new().unwrap();
    assert!(validate_new_file_path(temp.path(), ".git/config").is_err());
    assert!(validate_new_file_path(temp.path(), "sub/.git/hooks/pre-commit").is_err());
}

#[test]
#[cfg(unix)]
fn test_validate_new_file_path_rejects_symlink_escape() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&outside).unwrap();
    // A symlinked subdir that points outside the repo.
    std::os::unix::fs::symlink(&outside, repo.join("escape")).unwrap();

    assert!(validate_new_file_path(&repo, "escape/evil.txt").is_err());
}

#[test]
fn test_apply_substitution_to_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "Hello world\nThis is a test\nHello again").unwrap();

    let result = apply_substitution_to_file(&file_path, "Hello", "Hi", 1).unwrap();
    if let local::diff::SubstitutionResult::Changed {
        content, matches, ..
    } = result
    {
        assert_eq!(content, "Hi world\nThis is a test\nHi again");
        assert_eq!(matches, 2);
    } else {
        panic!("expected Changed");
    }
}

#[test]
fn test_apply_regex_to_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "version 1.2.3\nother line\nversion 4.5.6").unwrap();

    let result =
        apply_regex_to_file(&file_path, r"version \d+\.\d+\.\d+", "version X.X.X", 1).unwrap();
    if let local::diff::SubstitutionResult::Changed {
        content, matches, ..
    } = result
    {
        assert_eq!(content, "version X.X.X\nother line\nversion X.X.X");
        assert_eq!(matches, 2);
    } else {
        panic!("expected Changed");
    }
}

#[test]
fn test_create_file_with_content() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("new_file.txt");

    let (content, diff) = create_file_with_content(&file_path, "Hello world", 1).unwrap();
    assert_eq!(content, "Hello world\n");
    assert!(!diff.is_empty());
    assert!(file_path.exists());
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "Hello world\n");
}

#[test]
fn test_delete_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("to_delete.txt");
    fs::write(&file_path, "content").unwrap();
    assert!(file_path.exists());

    delete_file(&file_path).unwrap();
    assert!(!file_path.exists());
}

#[test]
fn test_write_file_content_with_nested_dirs() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("nested").join("dir").join("file.txt");

    write_file_content(&file_path, "nested content").unwrap();
    assert!(file_path.exists());
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "nested content");
}

#[test]
fn test_create_and_restore_out_of_tree_backup() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("original.txt");
    let backup_path = temp_dir
        .path()
        .join("backups")
        .join("tx-1")
        .join("original.txt");
    fs::write(&file_path, "original content").unwrap();

    // Backup lives outside the worktree and the original is untouched.
    let mode = create_backup(&file_path, &backup_path).unwrap();
    assert!(backup_path.exists());
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "original content");

    // Modify, then restore from the out-of-tree backup.
    fs::write(&file_path, "modified content").unwrap();
    restore_backup(&backup_path, &file_path, mode).unwrap();
    assert_eq!(fs::read_to_string(&file_path).unwrap(), "original content");
    // The backup is left in place (the tx dir is cleaned wholesale later).
    assert!(backup_path.exists());
}

#[test]
#[cfg(unix)]
fn test_restore_backup_restores_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("run.sh");
    let backup_path = temp_dir.path().join("backups").join("tx-1").join("run.sh");

    fs::write(&file_path, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o755)).unwrap();

    let mode = create_backup(&file_path, &backup_path).unwrap();
    assert_eq!(mode, 0o755);

    // Delete-then-restore (the `gx create delete` rollback path): the original
    // is gone entirely, so `atomic_write` alone has no existing target to
    // preserve a mode from - the mode captured at backup time must still be
    // applied. Without threading it through, the restored file would come
    // back at atomic_write's new-file default (0644), silently dropping the
    // executable bit.
    fs::remove_file(&file_path).unwrap();

    restore_backup(&backup_path, &file_path, mode).unwrap();

    let restored_mode = fs::metadata(&file_path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(
        restored_mode, 0o755,
        "restore_backup must reapply the mode captured at backup time"
    );
}
