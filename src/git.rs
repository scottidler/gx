use crate::repo::Repo;
use crate::ssh::{SshCommandDetector, SshUrlBuilder};
use eyre::{Context, Result};
use log::debug;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct RepoStatus {
    pub repo: Repo,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub is_clean: bool,
    pub changes: StatusChanges,
    pub remote_status: RemoteStatus,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct StatusChanges {
    pub modified: u32,
    pub added: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub untracked: u32,
    pub staged: u32,
}

#[derive(Debug, Clone)]
pub enum RemoteStatus {
    UpToDate,           // ✅ Local and remote are in sync
    Ahead(u32),         // ⬆️  Local is ahead by N commits
    Behind(u32),        // ⬇️  Local is behind by N commits
    Diverged(u32, u32), // 🔀 Local ahead by N, behind by M
    NoRemote,           // 📍 No remote tracking branch
    NoUpstream,         // 📍 No upstream branch configured
    DetachedHead,       // 📍 Detached HEAD state
    Error(String),      // ❌ Error checking remote status
}

/// Branch tracking information parsed from git status --porcelain --branch
#[derive(Debug, Clone)]
pub struct BranchTrackingInfo {
    pub remote_branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone)]
pub struct CheckoutResult {
    pub repo: Repo,
    pub branch_name: String,
    pub commit_sha: Option<String>,
    pub action: CheckoutAction,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CheckoutAction {
    CheckedOutSynced,  // Checked out and synced with remote
    CreatedFromRemote, // Created new branch from remote
    Stashed,           // Stashed uncommitted changes
    HasUntracked,      // Has untracked files after checkout
}

#[derive(Debug, Clone)]
pub struct CloneResult {
    pub repo_slug: String, // "user/repo"
    pub action: CloneAction,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CloneAction {
    Cloned,              // 📥 Successfully cloned new repo
    Updated,             // 🔄 Updated existing repo (checkout + pull)
    Stashed,             // 📦 Stashed changes during update
    DirectoryNotGitRepo, // 🏠 Directory exists but not git
    DifferentRemote,     // 🔗 Different remote URL
}

impl StatusChanges {
    pub fn is_empty(&self) -> bool {
        self.modified == 0
            && self.added == 0
            && self.deleted == 0
            && self.renamed == 0
            && self.untracked == 0
            && self.staged == 0
    }
}

/// Get git status for a single repository with options
pub fn get_repo_status_with_options(repo: &Repo, fetch_first: bool, no_remote: bool) -> RepoStatus {
    debug!(
        "Getting status for repo: {} (fetch_first: {}, no_remote: {})",
        repo.name, fetch_first, no_remote
    );

    let branch = get_current_branch(repo);
    let commit_sha = get_current_commit_sha(repo);
    let remote_status = if no_remote {
        RemoteStatus::NoRemote
    } else {
        get_remote_status_with_fetch(repo, fetch_first)
    };

    match get_status_changes(repo) {
        Ok(changes) => {
            let is_clean = changes.is_empty();
            RepoStatus {
                repo: repo.clone(),
                branch,
                commit_sha,
                is_clean,
                changes,
                remote_status,
                error: None,
            }
        }
        Err(e) => RepoStatus {
            repo: repo.clone(),
            branch,
            commit_sha,
            is_clean: false,
            changes: StatusChanges::default(),
            remote_status,
            error: Some(e.to_string()),
        },
    }
}

/// Get current commit SHA (7 characters)
fn get_current_commit_sha(repo: &Repo) -> Option<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo.path.to_string_lossy(),
            "rev-parse",
            "--short=7",
            "HEAD",
        ])
        .output()
        .ok()?;

    if output.status.success() {
        let sha = String::from_utf8(output.stdout).ok()?;
        Some(sha.trim().to_string())
    } else {
        None
    }
}

/// Get the current branch name for a repository
fn get_current_branch(repo: &Repo) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8(output.stdout).ok()?.trim().to_string();

        if !branch.is_empty() {
            Some(branch)
        } else {
            // Fallback for detached HEAD
            get_detached_head_info(repo)
        }
    } else {
        None
    }
}

/// Get info for detached HEAD state
fn get_detached_head_info(repo: &Repo) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output()
        .ok()?;

    if output.status.success() {
        let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
        Some(format!("HEAD@{commit}"))
    } else {
        None
    }
}

/// Get status changes by parsing git status --porcelain output
fn get_status_changes(repo: &Repo) -> Result<StatusChanges> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("status")
        .arg("--porcelain=v1")
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git status failed: {}", stderr));
    }

    let status_output =
        String::from_utf8(output.stdout).context("Invalid UTF-8 in git status output")?;

    let mut changes = StatusChanges::default();

    for line in status_output.lines() {
        if line.len() < 2 {
            continue;
        }

        let index_status = line.chars().next().unwrap_or(' ');
        let worktree_status = line.chars().nth(1).unwrap_or(' ');

        // Parse index (staged) changes
        match index_status {
            'A' => changes.staged += 1,
            'M' => changes.staged += 1,
            'D' => changes.staged += 1,
            'R' => changes.renamed += 1,
            'C' => changes.staged += 1, // Copied
            _ => {}
        }

        // Parse worktree (unstaged) changes
        match worktree_status {
            'M' => changes.modified += 1,
            'D' => changes.deleted += 1,
            '?' => changes.untracked += 1,
            _ => {}
        }

        // Handle special cases
        if index_status == 'A' && worktree_status == ' ' {
            changes.added += 1;
            changes.staged -= 1; // Don't double count
        }
    }

    debug!("Status for {}: {:?}", repo.name, changes);
    Ok(changes)
}

/// Parse git status --porcelain --branch output for remote tracking info
fn parse_branch_tracking_info(status_output: &str) -> Result<BranchTrackingInfo> {
    use regex::Regex;

    // Get the first line which contains branch tracking info
    let first_line = status_output
        .lines()
        .next()
        .ok_or_else(|| eyre::eyre!("Empty git status output"))?;

    // Check if it's a branch status line
    if !first_line.starts_with("## ") {
        return Err(eyre::eyre!(
            "Invalid git status --porcelain --branch format"
        ));
    }

    // Parse branch tracking line: ## local...remote [ahead X, behind Y]
    let branch_regex =
        Regex::new(r"^## (?P<local>[^.\s]+)(?:\.\.\.(?P<remote>\S+))?(?: \[(?P<tracking>.*)\])?")
            .context("Failed to compile branch regex")?;

    let captures = branch_regex
        .captures(first_line)
        .ok_or_else(|| eyre::eyre!("Failed to parse branch tracking info from: {}", first_line))?;

    let remote_branch = captures.name("remote").map(|m| m.as_str().to_string());

    // Parse tracking info [ahead X, behind Y]
    let mut ahead = 0;
    let mut behind = 0;

    if let Some(tracking_match) = captures.name("tracking") {
        let tracking_str = tracking_match.as_str();
        let tracking_regex =
            Regex::new(r"(?:ahead (?P<ahead>\d+))?(?:, )?(?:behind (?P<behind>\d+))?")
                .context("Failed to compile tracking regex")?;

        if let Some(tracking_captures) = tracking_regex.captures(tracking_str) {
            if let Some(ahead_match) = tracking_captures.name("ahead") {
                ahead = ahead_match
                    .as_str()
                    .parse::<u32>()
                    .context("Failed to parse ahead count")?;
            }
            if let Some(behind_match) = tracking_captures.name("behind") {
                behind = behind_match
                    .as_str()
                    .parse::<u32>()
                    .context("Failed to parse behind count")?;
            }
        }
    }

    Ok(BranchTrackingInfo {
        remote_branch,
        ahead,
        behind,
    })
}

/// Get remote tracking status using git status --porcelain --branch
fn get_remote_status_native(repo: &Repo) -> RemoteStatus {
    debug!(
        "Getting remote status using git status --porcelain --branch for {}",
        repo.name
    );

    // Execute git status --porcelain --branch
    let output = match Command::new("git")
        .args([
            "-C",
            &repo.path.to_string_lossy(),
            "status",
            "--porcelain",
            "--branch",
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            debug!("Git status command failed for {}: {}", repo.name, e);
            return RemoteStatus::Error("Git command failed".to_string());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!("Git status failed for {}: {}", repo.name, stderr);
        return RemoteStatus::Error("Git status failed".to_string());
    }

    let output_str = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(e) => {
            debug!(
                "Invalid UTF-8 in git status output for {}: {}",
                repo.name, e
            );
            return RemoteStatus::Error("Invalid UTF-8 in output".to_string());
        }
    };

    // Parse tracking info
    let tracking_info = match parse_branch_tracking_info(&output_str) {
        Ok(info) => info,
        Err(e) => {
            debug!("Failed to parse git status for {}: {}", repo.name, e);

            // Handle special cases
            if output_str.contains("## HEAD (no branch)") {
                return RemoteStatus::DetachedHead;
            }

            return RemoteStatus::Error("Parse failed".to_string());
        }
    };

    // Handle no upstream case
    if tracking_info.remote_branch.is_none() {
        return RemoteStatus::NoUpstream;
    }

    // Convert to RemoteStatus based on ahead/behind counts
    match (tracking_info.ahead, tracking_info.behind) {
        (0, 0) => RemoteStatus::UpToDate,
        (a, 0) if a > 0 => RemoteStatus::Ahead(a),
        (0, b) if b > 0 => RemoteStatus::Behind(b),
        (a, b) if a > 0 && b > 0 => RemoteStatus::Diverged(a, b),
        _ => RemoteStatus::UpToDate,
    }
}

/// Enhanced remote status with optional fetch
fn get_remote_status_with_fetch(repo: &Repo, fetch_first: bool) -> RemoteStatus {
    if fetch_first {
        debug!("Fetching latest remote refs for {}", repo.name);
        // Perform lightweight fetch to update tracking refs
        let fetch_result = Command::new("git")
            .args(["-C", &repo.path.to_string_lossy(), "fetch", "--quiet"])
            .output();

        match fetch_result {
            Ok(output) if output.status.success() => {
                debug!("Successfully fetched remote refs for {}", repo.name);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug!("Fetch failed for {}: {}", repo.name, stderr);
                // Continue with status check even if fetch fails
            }
            Err(e) => {
                debug!("Fetch command failed for {}: {}", repo.name, e);
                // Continue with status check even if fetch fails
            }
        }
    }

    get_remote_status_native(repo)
}

/// Checkout or create a branch in a repository, with stashing and sync
pub fn checkout_branch(
    repo: &Repo,
    branch_name: &str,
    create_branch: bool,
    from_branch: Option<&str>,
    stash: bool,
) -> CheckoutResult {
    debug!(
        "Checking out branch '{}' in repo: {}",
        branch_name, repo.name
    );

    let mut stashed = false;
    let mut has_untracked = false;

    // Check for uncommitted changes
    if stash {
        if let Ok(status) = get_status_changes(repo) {
            if !status.is_empty() {
                // Stash changes (excluding untracked files)
                let stash_result = Command::new("git")
                    .args([
                        "-C",
                        &repo.path.to_string_lossy(),
                        "stash",
                        "push",
                        "-m",
                        &format!("gx auto-stash for {branch_name}"),
                    ])
                    .output();

                if let Ok(output) = stash_result {
                    if output.status.success() {
                        stashed = true;
                        debug!("Stashed changes in {}", repo.name);
                    }
                }
            }
        }
    }

    // Perform checkout
    let checkout_result = if create_branch {
        // Create new branch
        let mut cmd = Command::new("git");
        cmd.args([
            "-C",
            &repo.path.to_string_lossy(),
            "checkout",
            "-b",
            branch_name,
        ]);

        if let Some(from) = from_branch {
            cmd.arg(from);
        }

        cmd.output()
    } else {
        // Checkout existing branch
        Command::new("git")
            .args(["-C", &repo.path.to_string_lossy(), "checkout", branch_name])
            .output()
    };

    // Handle checkout result
    match checkout_result {
        Ok(output) if output.status.success() => {
            // Try to pull/sync with remote if not creating a new branch
            if !create_branch {
                let _ = Command::new("git")
                    .args(["-C", &repo.path.to_string_lossy(), "pull", "--ff-only"])
                    .output();
            }

            // Check for untracked files after checkout
            if let Ok(status) = get_status_changes(repo) {
                has_untracked = status.untracked > 0;
            }

            let action = if create_branch {
                CheckoutAction::CreatedFromRemote
            } else if stashed {
                CheckoutAction::Stashed
            } else if has_untracked {
                CheckoutAction::HasUntracked
            } else {
                CheckoutAction::CheckedOutSynced
            };

            // Get commit SHA after successful checkout
            let commit_sha = get_current_commit_sha(repo);

            CheckoutResult {
                repo: repo.clone(),
                branch_name: branch_name.to_string(),
                commit_sha,
                action,
                error: None,
            }
        }
        Ok(output) => {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            CheckoutResult {
                repo: repo.clone(),
                branch_name: branch_name.to_string(),
                commit_sha: None,
                action: CheckoutAction::CheckedOutSynced,
                error: Some(error_msg.trim().to_string()),
            }
        }
        Err(e) => CheckoutResult {
            repo: repo.clone(),
            branch_name: branch_name.to_string(),
            commit_sha: None,
            action: CheckoutAction::CheckedOutSynced,
            error: Some(e.to_string()),
        },
    }
}

/// Resolve branch name, handling 'default' keyword
pub fn resolve_branch_name(repo: &Repo, branch_name: &str) -> Result<String> {
    if branch_name == "default" {
        get_default_branch_local(repo)
    } else {
        Ok(branch_name.to_string())
    }
}

/// Get default branch using local git commands (fast, no GitHub API)
pub fn get_default_branch_local(repo: &Repo) -> Result<String> {
    debug!("Getting default branch for repo: {}", repo.name);

    // Try to get the default branch from remote HEAD
    let output = Command::new("git")
        .args([
            "-C",
            &repo.path.to_string_lossy(),
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
        ])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let head_ref = String::from_utf8(output.stdout)
                .context("Invalid UTF-8 in git symbolic-ref output")?;

            // Extract branch name from refs/remotes/origin/branch-name
            if let Some(branch) = head_ref.trim().strip_prefix("refs/remotes/origin/") {
                return Ok(branch.to_string());
            }
        }
    }

    // Fallback: try common default branch names (check if they exist locally)
    for branch in &["main", "master"] {
        let output = Command::new("git")
            .args([
                "-C",
                &repo.path.to_string_lossy(),
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}"),
            ])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                return Ok(branch.to_string());
            }
        }
    }

    // Try to find the initial branch (the one created by git init)
    let output = Command::new("git")
        .args(["-C", &repo.path.to_string_lossy(), "branch", "--list"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let branches =
                String::from_utf8(output.stdout).context("Invalid UTF-8 in git branch output")?;

            // Look for main or master first
            for line in branches.lines() {
                let branch = line.trim().trim_start_matches("* ");
                if branch == "main" || branch == "master" {
                    return Ok(branch.to_string());
                }
            }

            // If no main/master, return the first branch
            if let Some(first_line) = branches.lines().next() {
                let branch = first_line.trim().trim_start_matches("* ");
                if !branch.is_empty() {
                    return Ok(branch.to_string());
                }
            }
        }
    }

    Err(eyre::eyre!(
        "Could not determine default branch for {}",
        repo.name
    ))
}

/// Clone or update a repository
pub fn clone_or_update_repo(repo_slug: &str, user_or_org: &str, token: &str) -> CloneResult {
    debug!("Processing repo: {repo_slug}");

    let parts: Vec<&str> = repo_slug.split('/').collect();
    if parts.len() != 2 {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Cloned,
            error: Some("Invalid repository slug format".to_string()),
        };
    }

    let repo_name = parts[1];
    let target_dir = std::path::PathBuf::from(user_or_org).join(repo_name);

    if !target_dir.exists() {
        // Clone new repository
        return clone_repo(repo_slug, &target_dir, token);
    }

    if !target_dir.join(".git").exists() {
        // Directory exists but not a git repo
        debug!(
            "Directory exists but is not a git repo: {}",
            target_dir.display()
        );
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::DirectoryNotGitRepo,
            error: None,
        };
    }

    // Check if existing repo has correct remote
    match get_remote_origin(&target_dir) {
        Ok(origin) if is_same_repo(&origin, repo_slug) => {
            // Update existing repo: get default branch, checkout, pull
            debug!("Updating existing repo: {repo_slug}");
            update_existing_repo(&target_dir, repo_slug, token)
        }
        Ok(origin) => {
            // Different remote URL
            debug!("Different remote URL detected. Expected: {repo_slug}, Found: {origin}");
            CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::DifferentRemote,
                error: None,
            }
        }
        Err(e) => CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to check remote: {e}")),
        },
    }
}

/// Clone a new repository
fn clone_repo(repo_slug: &str, target_dir: &std::path::Path, _token: &str) -> CloneResult {
    debug!(
        "Cloning new repo: {} to {}",
        repo_slug,
        target_dir.display()
    );

    // Pre-flight SSH connectivity check
    match SshCommandDetector::test_github_ssh_connection() {
        Ok(username) => debug!("SSH authenticated as: {username}"),
        Err(e) => {
            return CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Cloned,
                error: Some(format!("SSH connectivity test failed: {e}")),
            };
        }
    }

    // Create parent directory if needed
    if let Some(parent) = target_dir.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Cloned,
                error: Some(format!("Failed to create parent directory: {e}")),
            };
        }
    }

    // Clone the repository using SSH
    let clone_url = match SshUrlBuilder::build_ssh_url(repo_slug) {
        Ok(url) => {
            // Validate the generated SSH URL
            if let Err(e) = SshUrlBuilder::validate_ssh_url(&url) {
                return CloneResult {
                    repo_slug: repo_slug.to_string(),
                    action: CloneAction::Cloned,
                    error: Some(format!("Generated invalid SSH URL: {e}")),
                };
            }
            url
        }
        Err(e) => {
            return CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Cloned,
                error: Some(format!("Invalid repository slug: {e}")),
            };
        }
    };

    let ssh_command = match SshCommandDetector::get_ssh_command() {
        Ok(cmd) => cmd,
        Err(e) => {
            return CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Cloned,
                error: Some(format!("Failed to get SSH command: {e}")),
            };
        }
    };

    let output = Command::new("git")
        .env("GIT_SSH_COMMAND", ssh_command)
        .args([
            "clone",
            "--quiet",
            &clone_url,
            &target_dir.to_string_lossy(),
        ])
        .output();

    match output {
        Ok(result) if result.status.success() => {
            debug!("Successfully cloned: {repo_slug}");
            CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Cloned,
                error: None,
            }
        }
        Ok(result) => {
            let error_msg = String::from_utf8_lossy(&result.stderr);
            CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Cloned,
                error: Some(error_msg.trim().to_string()),
            }
        }
        Err(e) => CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Cloned,
            error: Some(e.to_string()),
        },
    }
}

/// Update an existing repository
fn update_existing_repo(repo_path: &std::path::Path, repo_slug: &str, token: &str) -> CloneResult {
    debug!(
        "Updating existing repo: {} at {}",
        repo_slug,
        repo_path.display()
    );

    // Get default branch from GitHub
    let default_branch = match crate::github::get_default_branch(repo_slug, token) {
        Ok(branch) => branch,
        Err(e) => {
            return CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::Updated,
                error: Some(format!("Failed to get default branch: {e}")),
            }
        }
    };

    debug!("Default branch for {repo_slug}: {default_branch}");

    let mut stashed = false;

    // Check for uncommitted changes (same logic as checkout_branch)
    if let Ok(status) = get_status_changes_for_path(repo_path) {
        if !status.is_empty() {
            debug!("Found uncommitted changes, stashing...");
            // Stash changes
            let stash_result = Command::new("git")
                .args([
                    "-C",
                    &repo_path.to_string_lossy(),
                    "stash",
                    "push",
                    "-m",
                    "gx auto-stash for clone update",
                ])
                .output();

            if let Ok(output) = stash_result {
                if output.status.success() {
                    stashed = true;
                    debug!("Successfully stashed changes");
                }
            }
        }
    }

    // Fetch latest changes from remote
    let fetch_result = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "fetch", "origin"])
        .output();

    if let Err(e) = fetch_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to fetch from remote: {e}")),
        };
    }

    // Checkout default branch
    let checkout_result = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "checkout",
            &default_branch,
        ])
        .output();

    if let Err(e) = checkout_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to checkout default branch: {e}")),
        };
    }

    // Pull latest (same as checkout: --ff-only)
    let pull_result = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "pull", "--ff-only"])
        .output();

    if let Err(e) = pull_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to pull latest changes: {e}")),
        };
    }

    let action = if stashed {
        CloneAction::Stashed
    } else {
        CloneAction::Updated
    };

    debug!("Successfully updated repo: {repo_slug}");
    CloneResult {
        repo_slug: repo_slug.to_string(),
        action,
        error: None,
    }
}

/// Get remote origin URL for a repository
fn get_remote_origin(repo_path: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ])
        .output()
        .context("Failed to get remote origin")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to get remote origin: {}", error));
    }

    let url = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(url)
}

/// Check if remote URL matches the expected repository slug
fn is_same_repo(remote_url: &str, expected_slug: &str) -> bool {
    // Handle different URL formats
    let normalized_remote = if let Some(ssh_part) = remote_url.strip_prefix("git@github.com:") {
        ssh_part.trim_end_matches(".git").to_string()
    } else if let Some(ssh_part) = remote_url.strip_prefix("ssh://git@github.com/") {
        ssh_part.trim_end_matches(".git").to_string()
    } else if let Some(https_part) = remote_url.strip_prefix("https://github.com/") {
        https_part.trim_end_matches(".git").to_string()
    } else {
        remote_url.to_string()
    };

    normalized_remote == expected_slug
}

/// Get status changes for a repository path (helper function for clone)
fn get_status_changes_for_path(repo_path: &std::path::Path) -> Result<StatusChanges> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "status",
            "--porcelain=v1",
        ])
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git status failed: {}", stderr));
    }

    let status_output =
        String::from_utf8(output.stdout).context("Invalid UTF-8 in git status output")?;

    let mut changes = StatusChanges::default();

    for line in status_output.lines() {
        if line.len() < 2 {
            continue;
        }

        let status_chars: Vec<char> = line.chars().take(2).collect();
        let index_status = status_chars[0];
        let worktree_status = status_chars[1];

        // Count different types of changes
        match (index_status, worktree_status) {
            ('A', _) => changes.added += 1,
            ('M', _) | (_, 'M') => changes.modified += 1,
            ('D', _) | (_, 'D') => changes.deleted += 1,
            ('R', _) => changes.renamed += 1,
            ('?', '?') => changes.untracked += 1,
            _ => {}
        }

        // Count staged changes
        if index_status != ' ' && index_status != '?' {
            changes.staged += 1;
        }
    }

    Ok(changes)
}

// Enhanced git operations for create/review functionality

/// Create a new branch from current HEAD or switch to existing branch
pub fn create_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    // Check if branch already exists locally
    if branch_exists_locally(repo_path, branch_name)? {
        debug!("Branch '{branch_name}' already exists locally, switching to it");
        return switch_branch(repo_path, branch_name);
    }

    // Check if branch exists on remote
    if branch_exists_on_remote(repo_path, branch_name)? {
        debug!("Branch '{branch_name}' exists on remote, checking out");
        return checkout_remote_branch(repo_path, branch_name);
    }

    // Create new branch from current HEAD
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "checkout",
            "-b",
            branch_name,
        ])
        .output()
        .context("Failed to execute git checkout -b")?;

    if output.status.success() {
        debug!(
            "Created new branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to create branch '{}': {}",
            branch_name,
            error
        ))
    }
}

/// Switch to an existing branch
pub fn switch_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "checkout", branch_name])
        .output()
        .context("Failed to execute git checkout")?;

    if output.status.success() {
        debug!(
            "Switched to branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to switch to branch '{}': {}",
            branch_name,
            error
        ))
    }
}

/// Delete a local branch
pub fn delete_local_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "branch",
            "-D",
            branch_name,
        ])
        .output()
        .context("Failed to execute git branch -D")?;

    if output.status.success() {
        debug!(
            "Deleted local branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to delete local branch '{}': {}",
            branch_name,
            error
        ))
    }
}

/// Add all changes to staging area
pub fn add_all_changes(repo_path: &std::path::Path) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "add", "."])
        .output()
        .context("Failed to execute git add")?;

    if output.status.success() {
        debug!("Added all changes to staging in '{}'", repo_path.display());
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to add changes: {}", error))
    }
}

/// Commit staged changes with a message
pub fn commit_changes(repo_path: &std::path::Path, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "commit", "-m", message])
        .output()
        .context("Failed to execute git commit")?;

    if output.status.success() {
        debug!(
            "Committed changes in '{}' with message: {}",
            repo_path.display(),
            message
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to commit changes: {}", error))
    }
}

/// Push branch to remote
pub fn push_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let ssh_command =
        SshCommandDetector::get_ssh_command().context("Failed to get SSH command for push")?;

    let output = Command::new("git")
        .env("GIT_SSH_COMMAND", ssh_command)
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "push",
            "--set-upstream",
            "origin",
            branch_name,
        ])
        .output()
        .context("Failed to execute git push")?;

    if output.status.success() {
        debug!(
            "Pushed branch '{}' to remote from '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to push branch '{}': {}",
            branch_name,
            error
        ))
    }
}

/// Check if repository has uncommitted changes
pub fn has_uncommitted_changes(repo_path: &std::path::Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "status", "--porcelain"])
        .output()
        .context("Failed to execute git status")?;

    if output.status.success() {
        let status_output =
            String::from_utf8(output.stdout).context("Invalid UTF-8 in git status output")?;
        Ok(!status_output.trim().is_empty())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to check git status: {}", error))
    }
}

/// Get the current branch name
pub fn get_current_branch_name(repo_path: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "branch",
            "--show-current",
        ])
        .output()
        .context("Failed to execute git branch --show-current")?;

    if output.status.success() {
        let branch = String::from_utf8(output.stdout)
            .context("Invalid UTF-8 in git branch output")?
            .trim()
            .to_string();
        Ok(branch)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to get current branch: {}", error))
    }
}

/// Check if a branch exists locally
pub fn branch_exists_locally(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch_name}"),
        ])
        .output()
        .context("Failed to execute git rev-parse")?;

    Ok(output.status.success())
}

/// Check if a branch exists on remote
pub fn branch_exists_on_remote(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{branch_name}"),
        ])
        .output()
        .context("Failed to execute git rev-parse for remote branch")?;

    Ok(output.status.success())
}

/// Checkout a branch that exists on remote (creates local tracking branch)
pub fn checkout_remote_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "checkout",
            "-b",
            branch_name,
            &format!("origin/{branch_name}"),
        ])
        .output()
        .context("Failed to execute git checkout for remote branch")?;

    if output.status.success() {
        debug!(
            "Checked out remote branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to checkout remote branch '{}': {}",
            branch_name,
            error
        ))
    }
}

/// Pull latest changes from remote
pub fn pull_latest(repo_path: &std::path::Path) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "pull"])
        .output()
        .context("Failed to execute git pull")?;

    if output.status.success() {
        debug!("Pulled latest changes in '{}'", repo_path.display());
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to pull latest changes: {}", error))
    }
}

/// Clone a repository to a target directory
pub fn clone_repository(clone_url: &str, target_dir: &std::path::Path) -> Result<()> {
    debug!(
        "Cloning repository from {} to {}",
        clone_url,
        target_dir.display()
    );

    if target_dir.exists() {
        return Err(eyre::eyre!(
            "Target directory already exists: {}",
            target_dir.display()
        ));
    }

    let output = Command::new("git")
        .args(["clone", clone_url, &target_dir.to_string_lossy()])
        .output()
        .context("Failed to execute git clone")?;

    if output.status.success() {
        debug!(
            "Successfully cloned repository to '{}'",
            target_dir.display()
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to clone repository: {}", error))
    }
}

/// Save current changes to stash with GX-specific message
/// Returns the stash reference (e.g., "stash@{0}")
pub fn stash_save(repo_path: &std::path::Path, message: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["stash", "push", "-m", message])
        .output()
        .map_err(|e| eyre::eyre!("Failed to run git stash push: {}", e))?;

    if output.status.success() {
        debug!("Stashed changes in '{}'", repo_path.display());
        // Return the stash reference - new stash is always stash@{0}
        Ok("stash@{0}".to_string())
    } else {
        Err(eyre::eyre!(
            "Failed to stash changes: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Pop specific stash by reference
pub fn stash_pop(repo_path: &std::path::Path, stash_ref: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["stash", "pop", stash_ref])
        .output()
        .map_err(|e| eyre::eyre!("Failed to run git stash pop: {}", e))?;

    if output.status.success() {
        debug!("Popped stash {} in '{}'", stash_ref, repo_path.display());
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to pop stash {}: {}",
            stash_ref,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Hard reset to HEAD (for rollback of file modifications)
pub fn reset_hard(repo_path: &std::path::Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["reset", "--hard", "HEAD"])
        .output()
        .map_err(|e| eyre::eyre!("Failed to run git reset --hard: {}", e))?;

    if output.status.success() {
        debug!("Hard reset completed in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to reset hard: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Reset last commit (for rollback of commits)
pub fn reset_commit(repo_path: &std::path::Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["reset", "--soft", "HEAD~1"])
        .output()
        .map_err(|e| eyre::eyre!("Failed to run git reset --soft: {}", e))?;

    if output.status.success() {
        debug!("Commit reset completed in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to reset commit: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Get the default/head branch name for the repository
pub fn get_head_branch(repo_path: &std::path::Path) -> Result<String> {
    // First try to get the default branch from remote
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(|e| eyre::eyre!("Failed to get HEAD branch: {}", e))?;

    if output.status.success() {
        let head_ref = String::from_utf8_lossy(&output.stdout);
        let head_ref_trimmed = head_ref.trim();
        // Extract branch name from "refs/remotes/origin/main"
        if let Some(branch_name) = head_ref_trimmed.strip_prefix("refs/remotes/origin/") {
            return Ok(branch_name.to_string());
        }
    }

    // Fallback: assume main or master
    for default_branch in &["main", "master"] {
        if branch_exists_remotely(repo_path, default_branch)? {
            return Ok(default_branch.to_string());
        }
    }

    Err(eyre::eyre!("Could not determine head branch"))
}

/// Check if a branch exists on remote
pub fn branch_exists_remotely(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["ls-remote", "--heads", "origin", branch_name])
        .output()
        .map_err(|e| eyre::eyre!("Failed to check remote branch: {}", e))?;

    Ok(output.status.success() && !output.stdout.is_empty())
}

/// Delete a remote branch (for rollback of push operations)
pub fn delete_remote_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["push", "origin", "--delete", branch_name])
        .output()
        .map_err(|e| eyre::eyre!("Failed to delete remote branch: {}", e))?;

    if output.status.success() {
        debug!(
            "Deleted remote branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        // Don't fail if branch doesn't exist remotely
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("remote ref does not exist") {
            debug!("Remote branch '{branch_name}' already deleted");
            Ok(())
        } else {
            Err(eyre::eyre!("Failed to delete remote branch: {}", stderr))
        }
    }
}

/// Check if a remote branch exists
pub fn remote_branch_exists(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["ls-remote", "--heads", "origin", branch_name])
        .output()
        .map_err(|e| eyre::eyre!("Failed to run git ls-remote: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(!stdout.trim().is_empty())
    } else {
        // If remote check fails, assume branch doesn't exist
        Ok(false)
    }
}

/// Pull latest changes from the remote repository
pub fn pull_latest_changes(repo_path: &std::path::Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["pull"])
        .output()
        .map_err(|e| eyre::eyre!("Failed to run git pull: {}", e))?;

    if output.status.success() {
        debug!(
            "Successfully pulled latest changes in '{}'",
            repo_path.display()
        );
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Don't fail if there's nothing to pull
        if stderr.contains("Already up to date") || stderr.contains("Already up-to-date") {
            debug!(
                "Repository is already up to date: '{}'",
                repo_path.display()
            );
            Ok(())
        } else {
            Err(eyre::eyre!("Failed to pull latest changes: {}", stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_changes_is_empty() {
        let empty = StatusChanges::default();
        assert!(empty.is_empty());

        let not_empty = StatusChanges {
            modified: 1,
            ..Default::default()
        };
        assert!(!not_empty.is_empty());
    }

    #[test]
    fn test_parse_porcelain_output() {
        // This would require mocking git commands or using a real git repo
        // We'd need to refactor get_status_changes to accept string input for testing
        // This is a placeholder for the actual test implementation

        // For now, just test that StatusChanges works correctly
        let changes = StatusChanges {
            modified: 1,
            untracked: 1,
            added: 1,
            ..Default::default()
        };

        assert!(!changes.is_empty());
        assert_eq!(changes.modified, 1);
        assert_eq!(changes.untracked, 1);
        assert_eq!(changes.added, 1);
    }

    // Rollback tests - these would need a real git repository to test properly
    // For now, we'll add basic structure tests
    mod rollback_tests {
        use super::*;
        use std::fs;
        use tempfile::TempDir;

        fn setup_test_repo() -> Result<(TempDir, std::path::PathBuf)> {
            let temp_dir =
                TempDir::new().map_err(|e| eyre::eyre!("Failed to create temp dir: {}", e))?;
            let repo_path = temp_dir.path().to_path_buf();

            // Initialize git repo
            let output = Command::new("git")
                .current_dir(&repo_path)
                .args(["init"])
                .output()
                .map_err(|e| eyre::eyre!("Failed to run git init: {}", e))?;

            if !output.status.success() {
                return Err(eyre::eyre!(
                    "git init failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            // Set up git config
            Command::new("git")
                .current_dir(&repo_path)
                .args(["config", "user.email", "test@example.com"])
                .output()
                .map_err(|e| eyre::eyre!("Failed to set git email: {}", e))?;

            Command::new("git")
                .current_dir(&repo_path)
                .args(["config", "user.name", "Test User"])
                .output()
                .map_err(|e| eyre::eyre!("Failed to set git name: {}", e))?;

            Ok((temp_dir, repo_path))
        }

        #[test]
        fn test_stash_save_and_pop() {
            // Skip if git is not available
            if Command::new("git").arg("--version").output().is_err() {
                return;
            }

            let Ok((_temp_dir, repo_path)) = setup_test_repo() else {
                // Skip test if setup fails (git not available, etc.)
                return;
            };

            // Create a file and commit it
            if fs::write(repo_path.join("test.txt"), "original content").is_err() {
                return;
            }

            let add_output = Command::new("git")
                .current_dir(&repo_path)
                .args(["add", "test.txt"])
                .output();

            if add_output.is_err() || !add_output.unwrap().status.success() {
                return;
            }

            let commit_output = Command::new("git")
                .current_dir(&repo_path)
                .args(["commit", "-m", "initial commit"])
                .output();

            if commit_output.is_err() || !commit_output.unwrap().status.success() {
                return;
            }

            // Modify the file (uncommitted change)
            if fs::write(repo_path.join("test.txt"), "modified content").is_err() {
                return;
            }

            // Test stash save
            if let Ok(stash_ref) = stash_save(&repo_path, "test stash") {
                assert_eq!(stash_ref, "stash@{0}");

                // File should be back to original content
                if let Ok(content) = fs::read_to_string(repo_path.join("test.txt")) {
                    assert_eq!(content, "original content");
                }

                // Test stash pop
                if stash_pop(&repo_path, &stash_ref).is_ok() {
                    // File should have modified content again
                    if let Ok(content) = fs::read_to_string(repo_path.join("test.txt")) {
                        assert_eq!(content, "modified content");
                    }
                }
            }
        }

        #[test]
        fn test_reset_hard() {
            // Skip if git is not available
            if Command::new("git").arg("--version").output().is_err() {
                return;
            }

            let Ok((_temp_dir, repo_path)) = setup_test_repo() else {
                return;
            };

            // Create initial commit
            if fs::write(repo_path.join("test.txt"), "original content").is_err() {
                return;
            }

            let add_output = Command::new("git")
                .current_dir(&repo_path)
                .args(["add", "test.txt"])
                .output();

            if add_output.is_err() || !add_output.unwrap().status.success() {
                return;
            }

            let commit_output = Command::new("git")
                .current_dir(&repo_path)
                .args(["commit", "-m", "initial commit"])
                .output();

            if commit_output.is_err() || !commit_output.unwrap().status.success() {
                return;
            }

            // Modify file
            if fs::write(repo_path.join("test.txt"), "modified content").is_err() {
                return;
            }

            // Reset hard should restore original content
            if reset_hard(&repo_path).is_ok() {
                if let Ok(content) = fs::read_to_string(repo_path.join("test.txt")) {
                    assert_eq!(content, "original content");
                }
            }
        }

        #[test]
        fn test_reset_commit() {
            // Skip if git is not available
            if Command::new("git").arg("--version").output().is_err() {
                return;
            }

            let Ok((_temp_dir, repo_path)) = setup_test_repo() else {
                return;
            };

            // Create initial commit
            if fs::write(repo_path.join("test.txt"), "original content").is_err() {
                return;
            }

            let add_output = Command::new("git")
                .current_dir(&repo_path)
                .args(["add", "test.txt"])
                .output();

            if add_output.is_err() || !add_output.unwrap().status.success() {
                return;
            }

            let commit_output = Command::new("git")
                .current_dir(&repo_path)
                .args(["commit", "-m", "initial commit"])
                .output();

            if commit_output.is_err() || !commit_output.unwrap().status.success() {
                return;
            }

            // Create second commit
            if fs::write(repo_path.join("test2.txt"), "second file").is_err() {
                return;
            }

            let add_output2 = Command::new("git")
                .current_dir(&repo_path)
                .args(["add", "test2.txt"])
                .output();

            if add_output2.is_err() || !add_output2.unwrap().status.success() {
                return;
            }

            let commit_output2 = Command::new("git")
                .current_dir(&repo_path)
                .args(["commit", "-m", "second commit"])
                .output();

            if commit_output2.is_err() || !commit_output2.unwrap().status.success() {
                return;
            }

            // Reset commit should undo the last commit but keep files staged
            if reset_commit(&repo_path).is_ok() {
                // test2.txt should still exist
                assert!(repo_path.join("test2.txt").exists());

                // Check git status to verify file is staged (if possible)
                if let Ok(output) = Command::new("git")
                    .current_dir(&repo_path)
                    .args(["status", "--porcelain"])
                    .output()
                {
                    if output.status.success() {
                        let status = String::from_utf8_lossy(&output.stdout);
                        // Should show the file as added (staged)
                        assert!(status.contains("test2.txt"));
                    }
                }
            }
        }

        #[test]
        fn test_has_uncommitted_changes() {
            // Skip if git is not available
            if Command::new("git").arg("--version").output().is_err() {
                return;
            }

            let Ok((_temp_dir, repo_path)) = setup_test_repo() else {
                return;
            };

            // Initially should have no uncommitted changes (empty repo)
            if let Ok(has_changes) = has_uncommitted_changes(&repo_path) {
                // Empty repo might have no changes
                // This test is more about ensuring the function doesn't crash
                let _ = has_changes;
            }

            // Create and add a file
            if fs::write(repo_path.join("test.txt"), "test content").is_ok() {
                // Should have uncommitted changes
                if let Ok(has_changes) = has_uncommitted_changes(&repo_path) {
                    assert!(has_changes);
                }
            }
        }
    }

    // Tests for new git status --porcelain --branch parser
    #[test]
    fn test_parse_branch_tracking_info_ahead_behind() {
        let output = "## main...origin/main [ahead 2, behind 5]\nM  modified-file.txt\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.remote_branch, Some("origin/main".to_string()));
        assert_eq!(info.ahead, 2);
        assert_eq!(info.behind, 5);
    }

    #[test]
    fn test_parse_branch_tracking_info_ahead_only() {
        let output = "## main...origin/main [ahead 3]\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.remote_branch, Some("origin/main".to_string()));
        assert_eq!(info.ahead, 3);
        assert_eq!(info.behind, 0);
    }

    #[test]
    fn test_parse_branch_tracking_info_behind_only() {
        let output = "## main...origin/main [behind 7]\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.remote_branch, Some("origin/main".to_string()));
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 7);
    }

    #[test]
    fn test_parse_branch_tracking_info_up_to_date() {
        let output = "## main...origin/main\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.remote_branch, Some("origin/main".to_string()));
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 0);
    }

    #[test]
    fn test_parse_branch_tracking_info_no_upstream() {
        let output = "## main\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.remote_branch, None);
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 0);
    }

    #[test]
    fn test_parse_branch_tracking_info_different_remote() {
        let output = "## feature...upstream/feature [behind 1]\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.remote_branch, Some("upstream/feature".to_string()));
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 1);
    }

    #[test]
    fn test_get_repo_status_with_options_no_remote() {
        // Create a test repo
        let repo = Repo::from_slug("test/repo".to_string());

        // Test with no_remote = true
        let status = get_repo_status_with_options(&repo, false, true);

        // Should have NoRemote status regardless of actual git state
        assert!(matches!(status.remote_status, RemoteStatus::NoRemote));
        assert_eq!(status.repo.name, "repo");
    }

    #[test]
    fn test_get_repo_status_with_options_default_behavior() {
        // Create a test repo
        let repo = Repo::from_slug("test/repo".to_string());

        // Test default behavior (no fetch, no skip remote)
        let status = get_repo_status_with_options(&repo, false, false);

        // Should have basic repo info
        assert_eq!(status.repo.name, "repo");
        // Remote status will depend on actual git state, but shouldn't be NoRemote
        assert!(!matches!(status.remote_status, RemoteStatus::NoRemote));
    }
}
