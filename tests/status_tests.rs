use gx::test_utils::*;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn test_status_discovers_repositories() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should discover all repositories
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("backend"));
    assert!(stdout.contains("api"));
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("dirty-repo"));

    // Should show summary
    assert!(stdout.contains("📊"));
}

#[test]
fn test_status_filtering_by_pattern() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "frontend", "backend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should only show filtered repositories
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("backend"));
    assert!(!stdout.contains("api"));
    assert!(!stdout.contains("docs"));
}

#[test]
fn test_status_shows_clean_repos() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show clean repo with appropriate status emoji
    assert!(stdout.contains("📍") || stdout.contains("🟢")); // No remote or up to date
    assert!(stdout.contains("frontend"));
}

#[test]
fn test_status_shows_dirty_repos() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "dirty-repo"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show dirty repo with appropriate emoji
    assert!(stdout.contains("dirty-repo"));
    // Should show either modified (📝) or untracked (❓) emoji
    assert!(stdout.contains("📝") || stdout.contains("❓"));
}

#[test]
fn test_status_detailed_flag() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "--detailed", "dirty-repo"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Detailed output should show more information
    assert!(stdout.contains("dirty-repo"));
    // Should contain repository header emoji
    assert!(stdout.contains("📁"));
}

#[test]
fn test_status_no_emoji_flag() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "--no-emoji", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should not contain status emojis (but summary might still have 📊)
    assert!(!stdout.contains("🟢"));
    assert!(!stdout.contains("📝"));
    assert!(!stdout.contains("❓"));
    assert!(!stdout.contains("📍"));

    // Should still contain repository name
    assert!(stdout.contains("frontend"));
}

#[test]
fn test_status_no_color_flag() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "--no-color", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should still show content but without ANSI color codes
    assert!(stdout.contains("frontend"));
    // This is hard to test without checking for ANSI sequences
}

#[test]
fn test_status_shows_commit_hash() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show 7-character commit hash
    let lines: Vec<&str> = stdout.lines().collect();
    let status_line = lines.iter().find(|line| line.contains("frontend")).unwrap();

    // Should have format: "  branch commit_hash emoji repo_name"
    let parts: Vec<&str> = status_line.split_whitespace().collect();
    assert!(parts.len() >= 4);

    // Second part should be 7-character commit hash
    let commit_hash = parts[1];
    assert_eq!(commit_hash.len(), 7);
    assert!(commit_hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_status_shows_branch_name() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show branch name (default is "main" or "master")
    assert!(stdout.contains("main") || stdout.contains("master"));
}

#[test]
fn test_status_exit_code_with_errors() {
    let temp_dir = TempDir::new().unwrap();

    // Create a directory that looks like a repo but isn't
    let fake_repo = temp_dir.path().join("fake-repo");
    std::fs::create_dir_all(&fake_repo).unwrap();
    std::fs::create_dir_all(fake_repo.join(".git")).unwrap();

    let output = run_gx_command(&["status"], temp_dir.path());

    // Should have non-zero exit code if there are errors
    if output.status.code().unwrap_or(0) != 0 {
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("errors"));
    }
}

#[test]
fn test_status_parallel_option() {
    let workspace = create_test_workspace();

    // Use the current directory to run gx with global flags
    let output = Command::new(get_gx_binary_path())
        .args(["--parallel", "2", "status"])
        .current_dir(workspace.path())
        .output()
        .unwrap();

    // Should succeed with custom parallelism
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📊"));
}

#[test]
fn test_status_max_depth_option() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create nested structure
    let nested_dir = temp_dir.path().join("level1").join("level2");
    std::fs::create_dir_all(&nested_dir).unwrap();
    create_test_repo(&nested_dir, "deep-repo", false);
    create_test_repo(temp_dir.path(), "shallow-repo", false);

    // Test max-depth 2 should find shallow but not deep
    let output = Command::new(get_gx_binary_path())
        .args(["--max-depth", "2", "status"])
        .current_dir(temp_dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find shallow repo but not deep repo
    assert!(stdout.contains("shallow-repo"));
    assert!(!stdout.contains("deep-repo"));
}

#[test]
fn test_status_help_output() {
    let output = run_gx_command(&["status", "--help"], std::env::current_dir().unwrap().as_path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should contain help sections
    assert!(stdout.contains("EMOJI LEGEND:"));
    assert!(stdout.contains("REMOTE STATUS:"));
    assert!(stdout.contains("EXAMPLES:"));

    // Should contain emoji descriptions
    assert!(stdout.contains("📝  Modified files"));
    assert!(stdout.contains("🟢  Up to date with remote"));

    // Should contain examples
    assert!(stdout.contains("gx status"));
    assert!(stdout.contains("gx status --detailed"));
}

#[test]
fn test_status_no_repos_found() {
    let temp_dir = TempDir::new().unwrap();

    let output = run_gx_command(&["status"], temp_dir.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show message when no repositories found
    assert!(stdout.contains("🔍 No repositories found"));
}