use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Get the path to the compiled gx binary for testing
pub fn get_gx_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    if path.ends_with("deps") {
        path.pop(); // Remove deps directory
    }
    path.push("gx");
    path
}

/// Run a gx command and return the output
pub fn run_gx_command(args: &[&str], working_dir: &Path) -> std::process::Output {
    Command::new(get_gx_binary_path())
        .args(args)
        .current_dir(working_dir)
        .output()
        .expect("Failed to execute gx command")
}

/// Run a git command in the specified directory
pub fn run_git_command(args: &[&str], working_dir: &Path) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(working_dir)
        .output()
        .expect("Failed to execute git command")
}

/// Get the current branch name for a repository
pub fn get_current_branch(repo_path: &Path) -> String {
    let output = run_git_command(&["branch", "--show-current"], repo_path);
    String::from_utf8(output.stdout)
        .expect("Failed to parse branch name")
        .trim()
        .to_string()
}

/// Create a test repository at the specified location
pub fn create_test_repo(base_path: &Path, repo_name: &str, with_remote: bool) -> PathBuf {
    let repo_path = base_path.join(repo_name);
    fs::create_dir_all(&repo_path).expect("Failed to create repo directory");

    // Initialize git repo
    run_git_command(&["init"], &repo_path);
    run_git_command(&["config", "user.email", "test@example.com"], &repo_path);
    run_git_command(&["config", "user.name", "Test User"], &repo_path);
    run_git_command(&["config", "commit.gpgsign", "false"], &repo_path);

    // Create initial commit
    let readme_path = repo_path.join("README.md");
    fs::write(&readme_path, format!("# {repo_name}\n\nTest repository"))
        .expect("Failed to write README");

    run_git_command(&["add", "README.md"], &repo_path);
    run_git_command(&["commit", "-m", "Initial commit"], &repo_path);

    if with_remote {
        // Add a fake remote URL
        let remote_url = format!("git@github.com:testuser/{repo_name}.git");
        run_git_command(&["remote", "add", "origin", &remote_url], &repo_path);
    }

    repo_path
}

/// Create a minimal test workspace with fast setup
pub fn create_test_workspace() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create minimal test repositories (fast)
    create_minimal_test_repo(temp_dir.path(), "frontend");
    create_minimal_test_repo(temp_dir.path(), "backend");
    create_minimal_test_repo(temp_dir.path(), "api");
    create_minimal_test_repo(temp_dir.path(), "docs");

    // Create a dirty repository
    let dirty_repo_path = create_minimal_test_repo(temp_dir.path(), "dirty-repo");
    let dirty_file = dirty_repo_path.join("dirty.txt");
    fs::write(&dirty_file, "This file is dirty").expect("Failed to write dirty file");

    temp_dir
}

/// Create a test workspace with full git setup (for tests that need it)
pub fn create_full_test_workspace() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create multiple test repositories
    create_test_repo(temp_dir.path(), "frontend", true);
    create_test_repo(temp_dir.path(), "backend", true);
    create_test_repo(temp_dir.path(), "api", true);
    create_test_repo(temp_dir.path(), "docs", true);

    // Create a dirty repository
    let dirty_repo_path = create_test_repo(temp_dir.path(), "dirty-repo", true);
    let dirty_file = dirty_repo_path.join("dirty.txt");
    fs::write(&dirty_file, "This file is dirty").expect("Failed to write dirty file");

    temp_dir
}

/// Create a minimal test repository with one commit - FASTER than full setup
pub fn create_minimal_test_repo(base_path: &Path, repo_name: &str) -> PathBuf {
    let repo_path = base_path.join(repo_name);
    fs::create_dir_all(&repo_path).expect("Failed to create repo directory");

    // Initialize git repo (minimal setup)
    run_git_command(&["init", "--quiet"], &repo_path);
    run_git_command(&["config", "user.email", "test@example.com"], &repo_path);
    run_git_command(&["config", "user.name", "Test User"], &repo_path);
    run_git_command(&["config", "commit.gpgsign", "false"], &repo_path);

    // Create one commit (minimal)
    let readme_path = repo_path.join("README.md");
    fs::write(&readme_path, format!("# {repo_name}")).expect("Failed to write README");
    run_git_command(&["add", "README.md"], &repo_path);
    run_git_command(&["commit", "--quiet", "-m", "Initial commit"], &repo_path);

    repo_path
}

/// Create a comprehensive test workspace with 5 diverse repositories for multi-repo testing
pub fn create_comprehensive_test_workspace() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // 1. Frontend repo - React-like structure with multiple branches
    let _frontend_path = create_test_repo_with_branches(
        temp_dir.path(),
        "frontend",
        "gx-testing/frontend",
        &["main", "develop", "feature/auth"],
    );

    // 2. Backend repo - API service with different commit history
    let _backend_path = create_test_repo_with_branches(
        temp_dir.path(),
        "backend",
        "gx-testing/backend",
        &["main", "staging"],
    );

    // 3. Mobile app repo - with untracked files
    let mobile_path = create_test_repo_with_branches(
        temp_dir.path(),
        "mobile-app",
        "gx-testing/mobile-app",
        &["main", "ios-fixes", "android-fixes"],
    );
    // Add untracked files
    fs::write(mobile_path.join("temp.log"), "temporary log file")
        .expect("Failed to create temp file");
    fs::write(mobile_path.join("build.cache"), "build cache").expect("Failed to create cache file");

    // 4. Infrastructure repo - with uncommitted changes
    let infra_path = create_test_repo_with_branches(
        temp_dir.path(),
        "infrastructure",
        "gx-testing/infrastructure",
        &["main", "production", "development"],
    );
    // Add staged changes
    fs::write(
        infra_path.join("terraform.tf"),
        "# Updated terraform config",
    )
    .expect("Failed to write terraform file");
    run_git_command(&["add", "terraform.tf"], &infra_path);

    // 5. Documentation repo - clean repo, multiple commits
    let _docs_path = create_test_repo_with_commits(
        temp_dir.path(),
        "documentation",
        "gx-testing/documentation",
        &[
            ("Initial docs", "README.md", "# Project Documentation"),
            ("Add API docs", "api.md", "# API Documentation"),
            (
                "Update installation guide",
                "install.md",
                "# Installation Guide",
            ),
        ],
    );

    temp_dir
}

/// Create a test repository with multiple branches
pub fn create_test_repo_with_branches(
    base_path: &Path,
    repo_name: &str,
    remote_slug: &str,
    branches: &[&str],
) -> PathBuf {
    let repo_path = base_path.join(repo_name);
    fs::create_dir_all(&repo_path).expect("Failed to create repo directory");

    // Initialize git repo
    run_git_command(&["init"], &repo_path);
    run_git_command(&["config", "user.email", "test@example.com"], &repo_path);
    run_git_command(&["config", "user.name", "Test User"], &repo_path);
    run_git_command(&["config", "commit.gpgsign", "false"], &repo_path);

    // Create initial commit on main
    let readme_path = repo_path.join("README.md");
    fs::write(
        &readme_path,
        format!("# {repo_name}\n\nTest repository for {remote_slug}"),
    )
    .expect("Failed to write README");

    run_git_command(&["add", "README.md"], &repo_path);
    run_git_command(&["commit", "-m", "Initial commit"], &repo_path);

    // Add remote
    let remote_url = format!("git@github.com:{remote_slug}.git");
    run_git_command(&["remote", "add", "origin", &remote_url], &repo_path);

    // Create additional branches
    for &branch in branches.iter().skip(1) {
        // Skip main/first branch
        run_git_command(&["checkout", "-b", branch], &repo_path);

        // Add a branch-specific file
        let branch_file = repo_path.join(format!("{}.md", branch.replace('/', "_")));
        fs::write(
            &branch_file,
            format!("# {branch}\n\nBranch-specific content for {branch}"),
        )
        .expect("Failed to write branch file");

        run_git_command(
            &["add", &format!("{}.md", branch.replace('/', "_"))],
            &repo_path,
        );
        run_git_command(
            &["commit", "-m", &format!("Add {branch} specific changes")],
            &repo_path,
        );
    }

    // Return to main branch
    run_git_command(&["checkout", "main"], &repo_path);

    repo_path
}

/// Create a test repository with multiple commits
pub fn create_test_repo_with_commits(
    base_path: &Path,
    repo_name: &str,
    remote_slug: &str,
    commits: &[(&str, &str, &str)],
) -> PathBuf {
    let repo_path = base_path.join(repo_name);
    fs::create_dir_all(&repo_path).expect("Failed to create repo directory");

    // Initialize git repo
    run_git_command(&["init"], &repo_path);
    run_git_command(&["config", "user.email", "test@example.com"], &repo_path);
    run_git_command(&["config", "user.name", "Test User"], &repo_path);
    run_git_command(&["config", "commit.gpgsign", "false"], &repo_path);

    // Add remote
    let remote_url = format!("git@github.com:{remote_slug}.git");
    run_git_command(&["remote", "add", "origin", &remote_url], &repo_path);

    // Create commits
    for (commit_msg, filename, content) in commits {
        let file_path = repo_path.join(filename);
        fs::write(&file_path, content).expect("Failed to write file");
        run_git_command(&["add", filename], &repo_path);
        run_git_command(&["commit", "-m", commit_msg], &repo_path);
    }

    repo_path
}

/// Check if GitHub integration tests should be run (requires scottidler token file)
pub fn should_run_github_tests() -> bool {
    let token_path = std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
        + "/.config/github/tokens/scottidler";
    std::path::Path::new(&token_path).exists()
}

/// Get the GitHub token for testing from token file
pub fn get_test_github_token() -> Option<String> {
    let token_path = std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
        + "/.config/github/tokens/scottidler";
    std::fs::read_to_string(token_path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Create a test environment configured for gx-testing organization
pub fn create_gx_testing_workspace() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Create repos that match the gx-testing organization structure
    let repos = [
        ("frontend", "gx-testing/frontend"),
        ("backend", "gx-testing/backend"),
        ("mobile-app", "gx-testing/mobile-app"),
        ("infrastructure", "gx-testing/infrastructure"),
        ("documentation", "gx-testing/documentation"),
    ];

    for (name, slug) in &repos {
        let repo_path = create_test_repo(temp_dir.path(), name, true);

        // Update remote to point to gx-testing org
        run_git_command(
            &[
                "remote",
                "set-url",
                "origin",
                &format!("git@github.com:{slug}.git"),
            ],
            &repo_path,
        );
    }

    temp_dir
}
