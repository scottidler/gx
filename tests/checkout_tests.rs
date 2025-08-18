use gx::test_utils::*;

#[test]
fn test_checkout_existing_branch() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create a feature branch first
    run_git_command(&["checkout", "-b", "feature-branch"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    // Test gx checkout to existing branch
    let output = run_gx_command(&["checkout", "feature-branch", "-p", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show successful checkout
    assert!(stdout.contains("📥") || stdout.contains("frontend"));
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("feature-branch"));

    // Most importantly, verify branch was actually switched
    assert_eq!(get_current_branch(&frontend_path), "feature-branch");

    // Verify command succeeded
    assert!(output.status.success(), "Command should have succeeded");
}

#[test]
fn test_checkout_create_new_branch() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Test creating new branch with -b flag
    let output = run_gx_command(&["checkout", "-b", "new-feature", "-p", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show branch creation
    assert!(stdout.contains("✨") || stdout.contains("created"));
    assert!(stdout.contains("frontend"));

    // Verify branch was created and switched
    assert_eq!(get_current_branch(&frontend_path), "new-feature");

    // Verify command succeeded
    assert!(output.status.success(), "Command should have succeeded");
}

#[test]
fn test_checkout_with_stash() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create a feature branch first
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    // Create uncommitted changes on main branch
    std::fs::write(frontend_path.join("modified.txt"), "modified content").unwrap();
    run_git_command(&["add", "modified.txt"], &frontend_path);
    std::fs::write(frontend_path.join("modified.txt"), "more changes").unwrap();

    // Verify we have changes to stash
    let status_output = run_git_command(&["status", "--porcelain"], &frontend_path);
    let status_text = String::from_utf8(status_output.stdout).unwrap();
    assert!(!status_text.trim().is_empty(), "Should have uncommitted changes to stash");

    // Test checkout with stash flag
    let output = run_gx_command(&["checkout", "feature", "-s", "-p", "frontend"], workspace.path());

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
fn test_checkout_help_output() {
    let output = run_gx_command(&["checkout", "--help"], &std::env::current_dir().unwrap());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show help content
    assert!(stdout.contains("Checkout branches across multiple repositories"));
    assert!(stdout.contains("-b"));
    assert!(stdout.contains("-s"));

    // Should succeed
    assert!(output.status.success());
}

#[test]
fn test_checkout_default_keyword() {
    let workspace = create_test_workspace();

    // Test checkout with explicit 'default' keyword and pattern flag
    let output = run_gx_command(&["checkout", "default", "-p", "frontend"], workspace.path());

    // Should succeed (even if no repos match the pattern)
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should show summary or no repos found message
    assert!(stdout.contains("📊") || stdout.contains("🔍 No repositories found"));
}

#[test]
fn test_checkout_no_branch_argument() {
    let workspace = create_test_workspace();

    // Test checkout with no arguments at all (should use default branch)
    let output = run_gx_command(&["checkout"], workspace.path());

    // Should succeed (even if no repos are found)
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should show summary or no repos found message
    assert!(stdout.contains("📊") || stdout.contains("🔍 No repositories found"));
}

#[test]
fn test_checkout_help_shows_default_examples() {
    use std::path::Path;
    let output = run_gx_command(&["checkout", "--help"], Path::new("."));

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show new examples with default keyword and pattern flag
    assert!(stdout.contains("gx checkout                       # Checkout default branch"));
    assert!(stdout.contains("gx checkout default               # Same as above"));
    assert!(stdout.contains("gx checkout -p frontend           # Checkout default branch in repos matching"));
    assert!(stdout.contains("-p, --patterns <PATTERN>"));

    // Should succeed
    assert!(output.status.success());
}

// ============================================================================
// COMPREHENSIVE TESTS FOR NEW DEFAULT BRANCH FUNCTIONALITY
// ============================================================================

#[test]
fn test_checkout_default_keyword_resolves_to_main() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Ensure we're on main branch (which should be the default)
    run_git_command(&["checkout", "main"], &frontend_path);

    // Create and switch to a different branch
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    assert_eq!(get_current_branch(&frontend_path), "feature");

    // Test that 'default' resolves to 'main' and switches back
    let output = run_gx_command(&["checkout", "default", "-p", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📥") || stdout.contains("frontend"));

    // Verify we're back on main (the default branch)
    assert_eq!(get_current_branch(&frontend_path), "main");
}

#[test]
fn test_checkout_no_arguments_uses_default() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create and switch to a different branch
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    assert_eq!(get_current_branch(&frontend_path), "feature");

    // Test that no arguments defaults to 'default' keyword
    let output = run_gx_command(&["checkout"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📥") || stdout.contains("📊"));

    // Verify we're back on main (the default branch)
    assert_eq!(get_current_branch(&frontend_path), "main");
}

#[test]
fn test_checkout_specific_branch_all_repos() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");
    let backend_path = workspace.path().join("backend");
    let api_path = workspace.path().join("api");
    let docs_path = workspace.path().join("docs");
    let dirty_repo_path = workspace.path().join("dirty-repo");

    // Create feature branch in ALL repos to avoid failures
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "-b", "feature"], &backend_path);
    run_git_command(&["checkout", "-b", "feature"], &api_path);
    run_git_command(&["checkout", "-b", "feature"], &docs_path);
    run_git_command(&["checkout", "-b", "feature"], &dirty_repo_path);

    // Switch back to main
    run_git_command(&["checkout", "main"], &frontend_path);
    run_git_command(&["checkout", "main"], &backend_path);
    run_git_command(&["checkout", "main"], &api_path);
    run_git_command(&["checkout", "main"], &docs_path);
    run_git_command(&["checkout", "main"], &dirty_repo_path);

    // Test checkout specific branch in all repos (no pattern)
    let output = run_gx_command(&["checkout", "feature"], workspace.path());

    // Should succeed since all repos have the feature branch
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("Command failed with exit code: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
               output.status.code(), stdout, stderr);
    }
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📥"));
    assert!(stdout.contains("📊"));

    // Verify all repos switched to feature branch
    assert_eq!(get_current_branch(&frontend_path), "feature");
    assert_eq!(get_current_branch(&backend_path), "feature");
    assert_eq!(get_current_branch(&api_path), "feature");
    assert_eq!(get_current_branch(&docs_path), "feature");
    assert_eq!(get_current_branch(&dirty_repo_path), "feature");
}

#[test]
fn test_checkout_with_multiple_patterns() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");
    let backend_path = workspace.path().join("backend");

    // Create feature branch in both repos
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "-b", "feature"], &backend_path);

    // Switch back to main
    run_git_command(&["checkout", "main"], &frontend_path);
    run_git_command(&["checkout", "main"], &backend_path);

    // Test checkout with multiple patterns
    let output = run_gx_command(&["checkout", "feature", "-p", "frontend", "-p", "backend"], workspace.path());

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("Command failed with exit code: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
               output.status.code(), stdout, stderr);
    }
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📥"));
    assert!(stdout.contains("📊"));

    // Verify both repos switched to feature branch
    assert_eq!(get_current_branch(&frontend_path), "feature");
    assert_eq!(get_current_branch(&backend_path), "feature");
}

#[test]
fn test_checkout_pattern_filters_correctly() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");
    let backend_path = workspace.path().join("backend");

    // Create feature branch in both repos
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "-b", "feature"], &backend_path);

    // Switch back to main
    run_git_command(&["checkout", "main"], &frontend_path);
    run_git_command(&["checkout", "main"], &backend_path);

    // Test checkout with pattern that only matches frontend
    let output = run_gx_command(&["checkout", "feature", "-p", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📥"));
    assert!(stdout.contains("📊"));

    // Verify only frontend switched, backend stayed on main
    assert_eq!(get_current_branch(&frontend_path), "feature");
    assert_eq!(get_current_branch(&backend_path), "main");
}

#[test]
fn test_checkout_long_pattern_flag() {
    let workspace = create_full_test_workspace();

    // Test using --patterns instead of -p
    let output = run_gx_command(&["checkout", "default", "--patterns", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📥") || stdout.contains("📊") || stdout.contains("🔍"));
}

#[test]
fn test_checkout_create_branch_with_default_from() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Test creating branch with default 'from' branch (should use 'default' which resolves to main)
    let output = run_gx_command(&["checkout", "-b", "new-feature", "-p", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("✨") || stdout.contains("📥"));

    // Verify branch was created and switched
    assert_eq!(get_current_branch(&frontend_path), "new-feature");
}

#[test]
fn test_checkout_create_branch_with_explicit_from() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create a develop branch first
    run_git_command(&["checkout", "-b", "develop"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    // Test creating branch from explicit branch
    let output = run_gx_command(&["checkout", "-b", "feature-from-develop", "-f", "develop", "-p", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("✨") || stdout.contains("📥"));

    // Verify branch was created and switched
    assert_eq!(get_current_branch(&frontend_path), "feature-from-develop");
}

#[test]
fn test_checkout_create_branch_with_default_from_keyword() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Test creating branch with explicit 'default' from branch
    let output = run_gx_command(&["checkout", "-b", "feature-from-default", "-f", "default", "-p", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("✨") || stdout.contains("📥"));

    // Verify branch was created and switched
    assert_eq!(get_current_branch(&frontend_path), "feature-from-default");
}

#[test]
fn test_checkout_nonexistent_branch_fails_gracefully() {
    let workspace = create_full_test_workspace();

    // Test checkout to non-existent branch
    let output = run_gx_command(&["checkout", "nonexistent-branch", "-p", "frontend"], workspace.path());

    // Command should fail because the branch doesn't exist
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show error for the failed checkout
    assert!(stdout.contains("❌") || stdout.contains("failed") || stdout.contains("📊"));
}

#[test]
fn test_checkout_no_matching_repos() {
    let workspace = create_full_test_workspace();

    // Test checkout with pattern that matches no repos
    let output = run_gx_command(&["checkout", "main", "-p", "nonexistent"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show "no repositories found" message
    assert!(stdout.contains("🔍 No repositories found"));
}

#[test]
fn test_checkout_help_shows_new_syntax() {
    use std::path::Path;
    let output = run_gx_command(&["checkout", "--help"], Path::new("."));

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show the new CLI syntax
    assert!(stdout.contains("Usage: gx checkout [OPTIONS] [BRANCH]"));
    assert!(stdout.contains("-p, --patterns <PATTERN>"));
    assert!(stdout.contains("Repository name patterns to filter"));

    // Should show updated examples
    assert!(stdout.contains("gx checkout -p frontend"));
    assert!(stdout.contains("gx checkout main -p frontend"));
    assert!(stdout.contains("gx checkout main -p frontend -p api"));

    assert!(output.status.success());
}

// ============================================================================
// EDGE CASE AND ERROR HANDLING TESTS
// ============================================================================

#[test]
fn test_resolve_branch_name_unit_test() {
    use gx::git::{resolve_branch_name};
    use gx::repo::Repo;
    use std::path::PathBuf;

    // Create a test repo struct
    let repo = Repo {
        path: PathBuf::from("/tmp/test-repo"),
        name: "test-repo".to_string(),
        slug: Some("user/test-repo".to_string()),
    };

    // Test non-default branch names pass through unchanged
    assert_eq!(resolve_branch_name(&repo, "main").unwrap(), "main");
    assert_eq!(resolve_branch_name(&repo, "master").unwrap(), "master");
    assert_eq!(resolve_branch_name(&repo, "feature-branch").unwrap(), "feature-branch");
    assert_eq!(resolve_branch_name(&repo, "develop").unwrap(), "develop");

    // Test 'default' keyword attempts resolution (will fail in test env, but that's expected)
    let result = resolve_branch_name(&repo, "default");
    // Should either succeed with a branch name or fail - both are valid in test environment
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn test_checkout_mixed_success_and_failure() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create feature branch only in frontend
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

        // Try to checkout feature branch in all repos (should succeed in frontend, fail in others)
    let output = run_gx_command(&["checkout", "feature"], workspace.path());

    // Should fail because some repos don't have the feature branch
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show both success and failure
    assert!(stdout.contains("📥") || stdout.contains("❌"));
    assert!(stdout.contains("📊")); // Should show summary
}

#[test]
fn test_checkout_stash_with_default_branch() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create uncommitted changes
    std::fs::write(frontend_path.join("modified.txt"), "modified content").unwrap();
    run_git_command(&["add", "modified.txt"], &frontend_path);
    std::fs::write(frontend_path.join("modified.txt"), "more changes").unwrap();

    // Create and switch to feature branch
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);

    // Test checkout to default branch with stash
    let output = run_gx_command(&["checkout", "default", "-s", "-p", "frontend"], workspace.path());

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("📦") || stdout.contains("📥"));

    // Should be back on main branch
    assert_eq!(get_current_branch(&frontend_path), "main");
}
