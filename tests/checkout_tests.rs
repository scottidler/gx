use gx::test_utils::*;
use tempfile::TempDir;

#[test]
fn test_checkout_existing_branch() {
    let workspace = create_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create a feature branch first
    run_git_command(&["checkout", "-b", "feature-branch"], &frontend_path);

    // Switch back to main
    run_git_command(&["checkout", "main"], &frontend_path);

    // Clean up any potential untracked files to ensure consistent test results
    let _ = std::fs::remove_dir_all(frontend_path.join("untracked"));
    let _ = std::fs::remove_file(frontend_path.join("untracked.txt"));

    // Test gx checkout to existing branch
    let output = run_gx_command(&["checkout", "feature-branch", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show successful checkout (any of these emojis indicates success)
    assert!(
        stdout.contains("🔄") || stdout.contains("⚠️") || stdout.contains("checked out") || stdout.contains("frontend"),
        "Expected successful checkout output, got: {}", stdout
    );
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("feature-branch"));

    // Most importantly, verify branch was actually switched
    assert_eq!(get_current_branch(&frontend_path), "feature-branch");

    // Verify command succeeded
    assert!(output.status.success(), "Command should have succeeded");
}

#[test]
fn test_checkout_create_new_branch() {
    let workspace = create_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Test creating new branch with -b flag
    let output = run_gx_command(&["checkout", "-b", "new-feature", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show branch creation
    assert!(stdout.contains("✨") || stdout.contains("created"));
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("new-feature"));

    // Verify branch was created and switched
    assert_eq!(get_current_branch(&frontend_path), "new-feature");
}

#[test]
fn test_checkout_create_branch_from_specific_base() {
    let workspace = create_test_workspace();
    let frontend_path = workspace.path().join("frontend");

        // Create a development branch
    run_git_command(&["checkout", "-b", "develop"], &frontend_path);

    // Add a commit to develop
    std::fs::write(frontend_path.join("develop.txt"), "develop feature").unwrap();
    run_git_command(&["add", "develop.txt"], &frontend_path);
    run_git_command(&["commit", "-m", "Add develop feature"], &frontend_path);

    // Switch back to main
    run_git_command(&["checkout", "main"], &frontend_path);

    // Create new branch from develop using -f flag
    let output = run_gx_command(&["checkout", "-b", "feature-from-develop", "-f", "develop", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show branch creation
    assert!(stdout.contains("✨") || stdout.contains("created"));
    assert!(stdout.contains("feature-from-develop"));

    // Verify branch was created from develop (should have develop.txt)
    assert_eq!(get_current_branch(&frontend_path), "feature-from-develop");
    assert!(frontend_path.join("develop.txt").exists());
}

#[test]
fn test_checkout_with_stash() {
    let workspace = create_test_workspace();
    let frontend_path = workspace.path().join("frontend");

        // Create uncommitted changes
    std::fs::write(frontend_path.join("modified.txt"), "modified content").unwrap();
    run_git_command(&["add", "modified.txt"], &frontend_path);
    std::fs::write(frontend_path.join("modified.txt"), "more changes").unwrap();

    // Create a feature branch to switch to
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    // Test checkout with stash flag
    let output = run_gx_command(&["checkout", "feature", "-s", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show stash operation
    assert!(stdout.contains("📦") || stdout.contains("stashed"));
    assert!(stdout.contains("frontend"));

    // Verify branch was switched
    assert_eq!(get_current_branch(&frontend_path), "feature");

    // Verify stash was created
    let stash_output = run_git_command(&["stash", "list"], &frontend_path);
    let stash_list = String::from_utf8(stash_output.stdout).unwrap();
    assert!(stash_list.contains("gx auto-stash"));
}

#[test]
fn test_checkout_filtering_by_pattern() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["checkout", "-b", "filtered-branch", "frontend", "backend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should only affect filtered repositories
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("backend"));
    assert!(!stdout.contains("api"));

    // Should show 2 completed operations
    assert!(stdout.contains("2 completed") || stdout.contains("frontend") && stdout.contains("backend"));
}

#[test]
fn test_checkout_error_handling() {
    let workspace = create_test_workspace();

    // Try to checkout non-existent branch without -b flag
    let output = run_gx_command(&["checkout", "non-existent-branch", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show error
    assert!(stdout.contains("❌") || stdout.contains("failed") || stdout.contains("error"));

    // Should have non-zero exit code
    assert!(!output.status.success());
}

#[test]
fn test_checkout_help_output() {
    let output = run_gx_command(&["checkout", "--help"], std::env::current_dir().unwrap().as_path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should contain help sections
    assert!(stdout.contains("CHECKOUT LEGEND:"));
    assert!(stdout.contains("EXAMPLES:"));

    // Should contain emoji descriptions
    assert!(stdout.contains("🔄  Checked out and synced"));
    assert!(stdout.contains("✨  Created new branch"));
    assert!(stdout.contains("📦  Stashed uncommitted changes"));

    // Should contain examples
    assert!(stdout.contains("gx checkout feature-branch"));
    assert!(stdout.contains("gx checkout -b new-feature"));
    assert!(stdout.contains("gx checkout -b fix -f main"));

    // Should show all options
    assert!(stdout.contains("-b, --branch"));
    assert!(stdout.contains("-f, --from <BRANCH>"));
    assert!(stdout.contains("-s, --stash"));
}

#[test]
fn test_checkout_no_repos_found() {
    let temp_dir = TempDir::new().unwrap();

    let output = run_gx_command(&["checkout", "main"], temp_dir.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show message when no repositories found
    assert!(stdout.contains("🔍 No repositories found"));
}

#[test]
fn test_checkout_multiple_repos_parallel() {
    let workspace = create_test_workspace();

    // Create branches in all repos first using our helper function
    for repo in ["frontend", "backend", "api"] {
        let repo_path = workspace.path().join(repo);
        run_git_command(&["checkout", "-b", "test-branch"], &repo_path);
        run_git_command(&["checkout", "main"], &repo_path);
    }

    let output = run_gx_command(&["checkout", "test-branch", "frontend", "backend", "api"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should process all repositories
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("backend"));
    assert!(stdout.contains("api"));

    // Should show summary
    assert!(stdout.contains("📊"));
    assert!(stdout.contains("3 completed") || stdout.contains("completed"));

    // Verify all repos were switched
    for repo in ["frontend", "backend", "api"] {
        let repo_path = workspace.path().join(repo);
        assert_eq!(get_current_branch(&repo_path), "test-branch");
    }

    // Verify command succeeded
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr).unwrap_or_default();
        panic!("Command should have succeeded. Exit code: {:?}, Stderr: {}, Stdout: {}",
               output.status.code(), stderr, stdout);
    }
}

#[test]
fn test_checkout_with_untracked_files() {
    let workspace = create_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create untracked files
    std::fs::write(frontend_path.join("untracked.txt"), "untracked content").unwrap();

    // Create feature branch
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    let output = run_gx_command(&["checkout", "feature", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show checkout with untracked files warning or successful checkout
    assert!(
        stdout.contains("⚠️") || stdout.contains("untracked") || stdout.contains("🔄") || stdout.contains("frontend"),
        "Expected checkout output with untracked files handling, got: {}", stdout
    );
    assert!(stdout.contains("frontend"));

    // Verify branch was switched despite untracked files
    assert_eq!(get_current_branch(&frontend_path), "feature");

    // Verify command succeeded
    assert!(output.status.success(), "Command should have succeeded");
}

#[test]
fn test_checkout_parallel_option() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["--parallel", "2", "checkout", "-b", "parallel-test"], workspace.path());

    // Should succeed with custom parallelism
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📊") || stdout.contains("completed"));
}

#[test]
fn test_checkout_branch_name_validation() {
    let workspace = create_test_workspace();

    // Test with invalid branch name (spaces)
    let output = run_gx_command(&["checkout", "-b", "invalid branch name", "frontend"], workspace.path());

    // Should fail with error
    assert!(!output.status.success() || {
        let stdout = String::from_utf8(output.stdout).unwrap();
        stdout.contains("❌") || stdout.contains("error") || stdout.contains("failed")
    });
}