use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Process-wide lock serializing every test that mutates a shared environment
/// variable (`XDG_DATA_HOME`, `XDG_CONFIG_HOME`, ...). `std::env::set_var` is
/// global to the process, so tests across modules must share ONE lock: three
/// independent per-module locks over the same variable do NOT serialize each
/// other and raced under load (a concurrent test flipped `XDG_DATA_HOME` out
/// from under another, stranding its recovery fixtures).
#[cfg(test)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the env lock, recovering from a poisoned mutex so one panicking
/// test cannot cascade PoisonError failures across the suite.
#[cfg(test)]
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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

    // Add remote origin (required by new Repo struct)
    let remote_url = format!("git@github.com:testorg/{repo_name}.git");
    run_git_command(&["remote", "add", "origin", &remote_url], &repo_path);

    // Create one commit (minimal)
    let readme_path = repo_path.join("README.md");
    fs::write(&readme_path, format!("# {repo_name}")).expect("Failed to write README");
    run_git_command(&["add", "README.md"], &repo_path);
    run_git_command(&["commit", "--quiet", "-m", "Initial commit"], &repo_path);

    repo_path
}

/// Create a bare-container layout under `base_path`: a `.bare/` shared db, a
/// `.git` pointer file (`gitdir: ./.bare`), and a default `main` worktree, with
/// origin set to `remote_slug`. Returns the container directory path.
///
/// The transient source repo the bare clone is seeded from lives in its own
/// TempDir (dropped on return), so it never pollutes a discovery scan of
/// `base_path`.
pub fn create_bare_container(base_path: &Path, repo_name: &str, remote_slug: &str) -> PathBuf {
    // 1. A throwaway source repo with a single commit on `main`.
    let source_tmp = TempDir::new().expect("Failed to create source temp dir");
    let source = source_tmp.path().join("source");
    fs::create_dir_all(&source).expect("Failed to create source dir");
    run_git_command(&["init", "--quiet", "-b", "main"], &source);
    run_git_command(&["config", "user.email", "test@example.com"], &source);
    run_git_command(&["config", "user.name", "Test User"], &source);
    run_git_command(&["config", "commit.gpgsign", "false"], &source);
    fs::write(source.join("README.md"), format!("# {repo_name}")).expect("Failed to write README");
    run_git_command(&["add", "README.md"], &source);
    run_git_command(&["commit", "--quiet", "-m", "Initial commit"], &source);

    // 2. Container directory holding a bare clone at `.bare` plus the pointer.
    let container = base_path.join(repo_name);
    fs::create_dir_all(&container).expect("Failed to create container dir");
    let bare = container.join(".bare");
    run_git_command(
        &[
            "clone",
            "--quiet",
            "--bare",
            source.to_str().expect("source path is valid utf-8"),
            bare.to_str().expect("bare path is valid utf-8"),
        ],
        base_path,
    );
    fs::write(container.join(".git"), "gitdir: ./.bare\n").expect("Failed to write .git pointer");

    // 3. Point origin at the intended slug and materialize the default worktree.
    let container_str = container.to_str().expect("container path is valid utf-8");
    run_git_command(
        &[
            "-C",
            container_str,
            "config",
            "remote.origin.url",
            &format!("git@github.com:{remote_slug}.git"),
        ],
        base_path,
    );
    run_git_command(
        &[
            "-C",
            container_str,
            "worktree",
            "add",
            "--quiet",
            "main",
            "main",
        ],
        base_path,
    );

    container
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

/// Check if GitHub integration tests should be run (requires `$GITHUB_PAT_HOME`
/// to be set -- the persona-aware home token env var, replacing the retired
/// per-org token-file scheme).
pub fn should_run_github_tests() -> bool {
    std::env::var("GITHUB_PAT_HOME")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Get the GitHub token for testing from `$GITHUB_PAT_HOME`.
pub fn get_test_github_token() -> Option<String> {
    std::env::var("GITHUB_PAT_HOME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
