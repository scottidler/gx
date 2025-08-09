use gx::test_utils::*;

#[test]
fn test_comprehensive_workspace_creation() {
    let workspace = create_comprehensive_test_workspace();

    // Verify all 5 repositories were created
    let expected_repos = ["frontend", "backend", "mobile-app", "infrastructure", "documentation"];

    for repo_name in &expected_repos {
        let repo_path = workspace.path().join(repo_name);
        assert!(repo_path.exists(), "Repository {} should exist", repo_name);
        assert!(repo_path.join(".git").exists(), "Repository {} should be a git repo", repo_name);
        assert!(repo_path.join("README.md").exists(), "Repository {} should have README.md", repo_name);
    }

    // Test specific repo characteristics

    // 1. Frontend should have multiple branches
    let frontend_path = workspace.path().join("frontend");
    let branch_output = run_git_command(&["branch", "-a"], &frontend_path);
    let branches = String::from_utf8(branch_output.stdout).unwrap();
    assert!(branches.contains("develop"));
    assert!(branches.contains("feature/auth"));

    // 2. Mobile app should have untracked files
    let mobile_path = workspace.path().join("mobile-app");
    assert!(mobile_path.join("temp.log").exists());
    assert!(mobile_path.join("build.cache").exists());

    // 3. Infrastructure should have staged changes
    let infra_path = workspace.path().join("infrastructure");
    let status_output = run_git_command(&["status", "--porcelain"], &infra_path);
    let status = String::from_utf8(status_output.stdout).unwrap();
    assert!(status.contains("A  terraform.tf"), "Infrastructure should have staged terraform.tf");

    // 4. Documentation should have multiple commits
    let docs_path = workspace.path().join("documentation");
    let log_output = run_git_command(&["log", "--oneline"], &docs_path);
    let log = String::from_utf8(log_output.stdout).unwrap();
    let commit_count = log.lines().count();
    assert!(commit_count >= 3, "Documentation should have at least 3 commits, found {}", commit_count);

    // 5. All repos should have gx-testing remote URLs
    for repo_name in &expected_repos {
        let repo_path = workspace.path().join(repo_name);
        let remote_output = run_git_command(&["remote", "get-url", "origin"], &repo_path);
        let remote_url = String::from_utf8(remote_output.stdout).unwrap();
        assert!(remote_url.contains("gx-testing"), "Repository {} should have gx-testing remote", repo_name);
        assert!(remote_url.contains(repo_name), "Repository {} remote should contain repo name", repo_name);
    }
}

#[test]
fn test_gx_status_with_comprehensive_workspace() {
    let workspace = create_comprehensive_test_workspace();

    let output = run_gx_command(&["status"], workspace.path());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show all 5 repositories
    assert!(stdout.contains("gx-testing/frontend"));
    assert!(stdout.contains("gx-testing/backend"));
    assert!(stdout.contains("gx-testing/mobile-app"));
    assert!(stdout.contains("gx-testing/infrastructure"));
    assert!(stdout.contains("gx-testing/documentation"));

    // Should show different statuses - check for any status indicators
    let has_status_indicators = stdout.contains("📝") || stdout.contains("M") ||
                               stdout.contains("📦") || stdout.contains("⚠️") ||
                               stdout.contains("✅") || stdout.contains("main");
    assert!(has_status_indicators, "Should show some status indicators. Output:\n{}", stdout);

    // Should show summary
    assert!(stdout.contains("📊") || stdout.contains("clean") || stdout.contains("dirty"), "Should show summary");

    println!("Status output:\n{}", stdout);
}

#[test]
fn test_gx_checkout_with_comprehensive_workspace() {
    let workspace = create_comprehensive_test_workspace();

    // Test creating a new branch across all repos
    let output = run_gx_command(&["checkout", "-b", "test-feature"], workspace.path());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show results for all repositories
    assert!(stdout.contains("gx-testing/frontend"));
    assert!(stdout.contains("gx-testing/backend"));

    // Verify branches were actually created
    for repo_name in &["frontend", "backend", "mobile-app", "infrastructure", "documentation"] {
        let repo_path = workspace.path().join(repo_name);
        let current_branch = get_current_branch(&repo_path);
        assert_eq!(current_branch, "test-feature", "Repository {} should be on test-feature branch", repo_name);
    }

    println!("Checkout output:\n{}", stdout);
}

#[cfg(test)]
mod github_integration_tests {
    use super::*;

    #[test]
    fn test_github_token_detection() {
        // This test will be skipped if GX_TEST_GITHUB_TOKEN is not set
        if !should_run_github_tests() {
            println!("Skipping GitHub integration tests - GX_TEST_GITHUB_TOKEN not set");
            return;
        }

        let token = get_test_github_token();
        assert!(token.is_some(), "GitHub token should be available");
        assert!(!token.unwrap().is_empty(), "GitHub token should not be empty");
    }

    #[test]
    fn test_gx_testing_workspace() {
        let workspace = create_gx_testing_workspace();

        // Verify all repos point to gx-testing organization
        let expected_repos = ["frontend", "backend", "mobile-app", "infrastructure", "documentation"];

        for repo_name in &expected_repos {
            let repo_path = workspace.path().join(repo_name);
            assert!(repo_path.exists(), "Repository {} should exist", repo_name);

            let remote_output = run_git_command(&["remote", "get-url", "origin"], &repo_path);
            let remote_url = String::from_utf8(remote_output.stdout).unwrap();
            assert!(remote_url.contains("gx-testing"), "Repository {} should have gx-testing remote", repo_name);
        }
    }
}
