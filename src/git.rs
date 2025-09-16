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
#[allow(dead_code)]
pub enum RemoteStatus {
    UpToDate,           // ✅ Local and remote are in sync
    Ahead(u32),         // ⬆️  Local is ahead by N commits
    Behind(u32),        // ⬇️  Local is behind by N commits
    Diverged(u32, u32), // 🔀 Local ahead by N, behind by M
    NoRemote,           // 📍 No remote tracking branch
    Error(String),      // ❌ Error checking remote status
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

/// Get git status for a single repository
pub fn get_repo_status(repo: &Repo) -> RepoStatus {
    debug!("Getting status for repo: {}", repo.name);

    let branch = get_current_branch(repo);
    let commit_sha = get_current_commit_sha(repo);
    let remote_status = get_remote_status(repo, &branch);

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

/// Get current commit SHA (full length)
fn get_current_commit_sha_full(repo: &Repo) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", &repo.path.to_string_lossy(), "rev-parse", "HEAD"])
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

/// Get remote tracking status for a repository branch
fn get_remote_status(repo: &Repo, branch: &Option<String>) -> RemoteStatus {
    let branch = match branch {
        Some(b) if !b.starts_with("HEAD@") => b,
        _ => return RemoteStatus::NoRemote,
    };

    debug!("Checking remote status for {}: {}", repo.name, branch);

    // Get local HEAD SHA (current commit, not branch tip)
    let local_sha = match get_current_commit_sha_full(repo) {
        Some(sha) => sha,
        None => return RemoteStatus::Error("Failed to get local HEAD SHA".to_string()),
    };

    // Get remote SHA using ls-remote (non-destructive!)
    let remote_sha = match get_remote_sha_ls_remote(repo, branch) {
        Ok(sha) => sha,
        Err(e) => return RemoteStatus::Error(e.to_string()),
    };

    debug!(
        "Remote status for {}: local={}, remote={}",
        repo.name,
        &local_sha[..7],
        &remote_sha[..7]
    );

    // Quick comparison first
    if local_sha == remote_sha {
        return RemoteStatus::UpToDate;
    }

    // Count ahead/behind using actual SHAs
    // When remote SHA doesn't exist locally, we can't count commits accurately
    // This indicates the repo needs to be fetched/synced
    let behind = count_commits_between(&local_sha, &remote_sha, repo).unwrap_or_else(|_| {
        debug!(
            "Cannot count commits behind for {} - remote SHA not in local repo",
            repo.name
        );
        1 // Indicate that we're behind, but can't count exactly
    });

    let ahead = count_commits_between(&remote_sha, &local_sha, repo).unwrap_or_else(|_| {
        debug!(
            "Cannot count commits ahead for {} - remote SHA not in local repo",
            repo.name
        );
        0 // If we can't count, assume we're not ahead
    });

    debug!(
        "Remote status for {}: ahead={}, behind={}",
        repo.name, ahead, behind
    );

    match (ahead, behind) {
        (0, 0) => RemoteStatus::UpToDate,
        (a, 0) if a > 0 => RemoteStatus::Ahead(a),
        (0, b) if b > 0 => RemoteStatus::Behind(b),
        (a, b) if a > 0 && b > 0 => RemoteStatus::Diverged(a, b),
        _ => RemoteStatus::UpToDate,
    }
}

/// Get remote SHA using git ls-remote (non-destructive)
fn get_remote_sha_ls_remote(repo: &Repo, branch: &str) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo.path.to_string_lossy(),
            "ls-remote",
            "origin",
            branch,
        ])
        .output()
        .context("Failed to run git ls-remote")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git ls-remote failed: {}", stderr));
    }

    let output_str =
        String::from_utf8(output.stdout).context("Invalid UTF-8 in ls-remote output")?;

    // Parse: "SHA\trefs/heads/branch"
    if let Some(line) = output_str.lines().next() {
        if let Some(sha) = line.split('\t').next() {
            return Ok(sha.to_string());
        }
    }

    Err(eyre::eyre!("Could not parse ls-remote output"))
}

/// Count commits between two SHAs
fn count_commits_between(from_sha: &str, to_sha: &str, repo: &Repo) -> Result<u32> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo.path.to_string_lossy(),
            "rev-list",
            "--count",
            &format!("{from_sha}..{to_sha}"),
        ])
        .output()
        .context("Failed to count commits")?;

    if output.status.success() {
        let count_str =
            String::from_utf8(output.stdout).context("Invalid UTF-8 in rev-list output")?;
        let count = count_str
            .trim()
            .parse::<u32>()
            .context("Failed to parse commit count")?;
        Ok(count)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If the SHA doesn't exist locally, we can't count commits
        // This happens when remote has commits we haven't fetched
        if stderr.contains("unknown revision") || stderr.contains("ambiguous argument") {
            debug!("Cannot count commits between {from_sha} and {to_sha} - remote SHA not in local repo");
            Err(eyre::eyre!("Remote SHA not found in local repository"))
        } else {
            Err(eyre::eyre!("git rev-list failed: {}", stderr))
        }
    }
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

/// Create a new branch from current HEAD
pub fn create_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
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
            "Created branch '{}' in '{}'",
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

/// Stash uncommitted changes
#[allow(dead_code)]
pub fn stash_changes(repo_path: &std::path::Path, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "stash",
            "push",
            "-m",
            message,
        ])
        .output()
        .context("Failed to execute git stash push")?;

    if output.status.success() {
        debug!(
            "Stashed changes in '{}' with message: {}",
            repo_path.display(),
            message
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to stash changes: {}", error))
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
#[allow(dead_code)]
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

/// Reset repository to HEAD (discard uncommitted changes)
#[allow(dead_code)]
pub fn reset_to_head(repo_path: &std::path::Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "reset",
            "--hard",
            "HEAD",
        ])
        .output()
        .context("Failed to execute git reset --hard")?;

    if output.status.success() {
        debug!("Reset repository to HEAD in '{}'", repo_path.display());
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to reset repository: {}", error))
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
        let mut changes = StatusChanges::default();
        changes.modified = 1;
        changes.untracked = 1;
        changes.added = 1;

        assert!(!changes.is_empty());
        assert_eq!(changes.modified, 1);
        assert_eq!(changes.untracked, 1);
        assert_eq!(changes.added, 1);
    }
}
