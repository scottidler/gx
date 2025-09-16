use eyre::Result;
use gx::repo::discover_repos;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Test that demonstrates Stephen's issue: gx behaves differently when run from
/// inside a repo vs one level above (workspace directory)
#[test]
fn test_stephen_branch_detection_consistency_issue() {
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path();

    // Create a workspace with a single repository
    let repo_path = workspace_path.join("test-repo");
    fs::create_dir_all(&repo_path).unwrap();

    // Initialize git repository
    init_git_repo(&repo_path, "testuser", "test-repo").unwrap();

    // Test Case 1: Run discovery from INSIDE the repository
    let repos_from_inside = discover_repos(&repo_path, 3).unwrap();
    println!(
        "Discovery from inside repo: found {} repos",
        repos_from_inside.len()
    );

    // Test Case 2: Run discovery from ONE LEVEL ABOVE (workspace directory)
    let repos_from_above = discover_repos(workspace_path, 3).unwrap();
    println!(
        "Discovery from workspace: found {} repos",
        repos_from_above.len()
    );

    // These should be IDENTICAL behavior but currently they're not
    assert_eq!(
        repos_from_inside.len(),
        repos_from_above.len(),
        "Repository discovery should find the same number of repos regardless of execution directory"
    );

    // Both should find the same repository
    assert_eq!(
        repos_from_inside.len(),
        1,
        "Should find exactly one repository"
    );
    assert_eq!(
        repos_from_above.len(),
        1,
        "Should find exactly one repository"
    );

    let repo_inside = &repos_from_inside[0];
    let repo_above = &repos_from_above[0];

    // The discovered repository should be identical
    assert_eq!(
        repo_inside.path, repo_above.path,
        "Should discover the same repository path regardless of execution directory"
    );

    assert_eq!(
        repo_inside.name, repo_above.name,
        "Should discover the same repository name regardless of execution directory"
    );

    // This is where Stephen's issue manifests - inconsistent slug detection
    assert_eq!(
        repo_inside.slug, repo_above.slug,
        "Repository slug should be identical regardless of execution directory"
    );

    // The slug should be properly detected from git config
    assert_eq!(repo_inside.slug, "testuser/test-repo");
}

/// Test with multiple repositories to ensure workspace discovery works correctly
#[test]
fn test_multiple_repo_workspace_consistency() {
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path();

    // Create multiple repositories
    let repo1_path = workspace_path.join("repo1");
    let repo2_path = workspace_path.join("repo2");
    let repo3_path = workspace_path.join("nested").join("repo3");

    fs::create_dir_all(&repo1_path).unwrap();
    fs::create_dir_all(&repo2_path).unwrap();
    fs::create_dir_all(&repo3_path).unwrap();

    // Initialize git repositories with different users/orgs
    init_git_repo(&repo1_path, "user1", "repo1").unwrap();
    init_git_repo(&repo2_path, "org2", "repo2").unwrap();
    init_git_repo(&repo3_path, "user3", "repo3").unwrap();

    // Discovery from workspace should find all repos
    let repos = discover_repos(workspace_path, 3).unwrap();

    assert_eq!(repos.len(), 3, "Should discover all 3 repositories");

    // Each repository should have its own user/org detected from git config
    let repo1 = repos.iter().find(|r| r.name == "repo1").unwrap();
    let repo2 = repos.iter().find(|r| r.name == "repo2").unwrap();
    let repo3 = repos.iter().find(|r| r.name == "repo3").unwrap();

    // Slugs should be properly detected per repository
    assert_eq!(repo1.slug, "user1/repo1");
    assert_eq!(repo2.slug, "org2/repo2");
    assert_eq!(repo3.slug, "user3/repo3");
}

/// Test that running discovery from different subdirectories gives consistent results
#[test]
fn test_subdirectory_execution_consistency() {
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path();

    // Create nested directory structure with repositories
    let repo1_path = workspace_path.join("frontend");
    let repo2_path = workspace_path.join("backend");
    let subdir_path = workspace_path.join("docs");

    fs::create_dir_all(&repo1_path).unwrap();
    fs::create_dir_all(&repo2_path).unwrap();
    fs::create_dir_all(&subdir_path).unwrap();

    init_git_repo(&repo1_path, "myorg", "frontend").unwrap();
    init_git_repo(&repo2_path, "myorg", "backend").unwrap();

    // Test discovery from workspace root
    let repos_from_root = discover_repos(workspace_path, 3).unwrap();

    // Test discovery from subdirectory (this should behave consistently)
    let repos_from_subdir = discover_repos(&subdir_path, 3).unwrap();

    // Both should find the same repositories
    assert_eq!(
        repos_from_root.len(),
        repos_from_subdir.len(),
        "Discovery should be consistent regardless of execution subdirectory"
    );

    // Repository information should be identical
    for repo_root in &repos_from_root {
        let matching_repo = repos_from_subdir
            .iter()
            .find(|r| r.path == repo_root.path)
            .expect("Should find matching repository");

        assert_eq!(repo_root.name, matching_repo.name);
        assert_eq!(repo_root.slug, matching_repo.slug);
    }
}

/// Test per-repository user/org detection from git config
#[test]
fn test_per_repo_user_org_detection() {
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path();

    // Create repositories with different remote configurations
    let personal_repo = workspace_path.join("personal-project");
    let work_repo = workspace_path.join("work-project");

    fs::create_dir_all(&personal_repo).unwrap();
    fs::create_dir_all(&work_repo).unwrap();

    // Initialize with different users - this simulates mixed workspace
    init_git_repo(&personal_repo, "johndoe", "personal-project").unwrap();
    init_git_repo(&work_repo, "acme-corp", "work-project").unwrap();

    let repos = discover_repos(workspace_path, 3).unwrap();
    assert_eq!(repos.len(), 2);

    let personal = repos.iter().find(|r| r.name == "personal-project").unwrap();
    let work = repos.iter().find(|r| r.name == "work-project").unwrap();

    // Each repo should have its own user/org from its git config
    assert_eq!(personal.slug, "johndoe/personal-project");
    assert_eq!(work.slug, "acme-corp/work-project");
}

/// Helper function to initialize a git repository with remote configuration
fn init_git_repo(repo_path: &PathBuf, user_or_org: &str, repo_name: &str) -> Result<()> {
    // Initialize git repository
    Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()?;

    // Configure user for commits
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .output()?;

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .output()?;

    // Add remote origin (this is what we'll parse for user/org detection)
    let remote_url = format!("git@github.com:{}/{}.git", user_or_org, repo_name);
    Command::new("git")
        .args(["remote", "add", "origin", &remote_url])
        .current_dir(repo_path)
        .output()?;

    // Create initial commit so the repo has a proper HEAD
    fs::write(repo_path.join("README.md"), "# Test Repository")?;

    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(repo_path)
        .output()?;

    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()?;

    Ok(())
}
