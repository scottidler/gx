use crate::repo::Repo;
use crate::ssh::{SshCommandDetector, SshUrlBuilder};
use crate::subprocess::{run_checked, subprocess_timeout};
use eyre::{Context, Result};
use log::{debug, warn};
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
    Ahead(u32),         // ↑  Local is ahead by N commits
    Behind(u32),        // ↓  Local is behind by N commits
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
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo.path.to_string_lossy(),
            "rev-parse",
            "--short=7",
            "HEAD",
        ]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .arg("branch")
            .arg("--show-current"),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&repo.path)
            .arg("rev-parse")
            .arg("--short")
            .arg("HEAD"),
        subprocess_timeout(),
    )
    .ok()?;

    if output.status.success() {
        let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
        Some(format!("HEAD@{commit}"))
    } else {
        None
    }
}

/// Parse `git status --porcelain=v1` output text into change counts.
///
/// The single counting rule used everywhere ([A20]). Porcelain v1 lines are
/// `XY <path>` where `X` is the index (staged) status and `Y` the worktree
/// status:
/// - `??` -> untracked
/// - index `A` -> added; index `M`/`D`/`C` -> staged; index `R` -> renamed
/// - worktree `M` -> modified; worktree `D` -> deleted
pub fn parse_porcelain_status(text: &str) -> StatusChanges {
    let mut changes = StatusChanges::default();

    for line in text.lines() {
        if line.len() < 2 {
            continue;
        }
        let mut chars = line.chars();
        let index_status = chars.next().unwrap_or(' ');
        let worktree_status = chars.next().unwrap_or(' ');

        if index_status == '?' && worktree_status == '?' {
            changes.untracked += 1;
            continue;
        }

        match index_status {
            'A' => changes.added += 1,
            'M' | 'D' | 'C' => changes.staged += 1,
            'R' => changes.renamed += 1,
            _ => {}
        }
        match worktree_status {
            'M' => changes.modified += 1,
            'D' => changes.deleted += 1,
            _ => {}
        }
    }

    changes
}

/// Run `git status --porcelain=v1` in `repo_path` and return the output text.
fn run_status_porcelain(repo_path: &std::path::Path) -> Result<String> {
    let output = run_checked(
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .arg("status")
            .arg("--porcelain=v1"),
        subprocess_timeout(),
    )
    .context("Failed to run git status")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("git status failed: {}", stderr));
    }

    // We only parse the leading `XY` status columns, never the path, so a lossy
    // conversion is safe and avoids aborting on a non-UTF-8 filename ([A21]).
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Get status changes by parsing git status --porcelain output
fn get_status_changes(repo: &Repo) -> Result<StatusChanges> {
    let changes = parse_porcelain_status(&run_status_porcelain(&repo.path)?);
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
    let output = match run_checked(
        Command::new("git").args([
            "-C",
            &repo.path.to_string_lossy(),
            "status",
            "--porcelain",
            "--branch",
        ]),
        subprocess_timeout(),
    ) {
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
        let fetch_result = run_checked(
            Command::new("git").args(["-C", &repo.path.to_string_lossy(), "fetch", "--quiet"]),
            subprocess_timeout(),
        );

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
                let stash_result = run_checked(
                    Command::new("git").args([
                        "-C",
                        &repo.path.to_string_lossy(),
                        "stash",
                        "push",
                        "-m",
                        &format!("gx auto-stash for {branch_name}"),
                    ]),
                    subprocess_timeout(),
                );

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

        run_checked(&mut cmd, subprocess_timeout())
    } else {
        // Checkout existing branch
        run_checked(
            Command::new("git").args(["-C", &repo.path.to_string_lossy(), "checkout", branch_name]),
            subprocess_timeout(),
        )
    };

    // Handle checkout result
    match checkout_result {
        Ok(output) if output.status.success() => {
            // Try to pull/sync with remote if not creating a new branch
            if !create_branch {
                let _ = run_checked(
                    Command::new("git").args([
                        "-C",
                        &repo.path.to_string_lossy(),
                        "pull",
                        "--ff-only",
                    ]),
                    subprocess_timeout(),
                );
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
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo.path.to_string_lossy(),
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
        ]),
        subprocess_timeout(),
    );

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
        let output = run_checked(
            Command::new("git").args([
                "-C",
                &repo.path.to_string_lossy(),
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}"),
            ]),
            subprocess_timeout(),
        );

        if let Ok(output) = output {
            if output.status.success() {
                return Ok(branch.to_string());
            }
        }
    }

    // Try to find the initial branch (the one created by git init)
    let output = run_checked(
        Command::new("git").args(["-C", &repo.path.to_string_lossy(), "branch", "--list"]),
        subprocess_timeout(),
    );

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
            // Update existing repo: get default branch, checkout, pull.
            debug!("Updating existing repo: {repo_slug}");
            let update_path = match resolve_update_work_tree(&target_dir) {
                Ok(path) => path,
                Err(e) => {
                    return CloneResult {
                        repo_slug: repo_slug.to_string(),
                        action: CloneAction::Updated,
                        error: Some(format!(
                            "bare container has no usable default worktree: {e}"
                        )),
                    };
                }
            };
            update_existing_repo(&update_path, repo_slug, token)
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

    let output = run_checked(
        Command::new("git")
            .env("GIT_SSH_COMMAND", ssh_command)
            .args([
                "clone",
                "--quiet",
                &clone_url,
                &target_dir.to_string_lossy(),
            ]),
        subprocess_timeout(),
    );

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
/// The work tree that `git status`/`checkout`/`pull` must run in when updating an
/// existing repo. A bare container is ONE logical repo == its default worktree;
/// the container root is NOT a work tree, so those commands would fail there
/// (`fatal: this operation must be run in a work tree`). A flat checkout is
/// itself. `get_remote_origin` resolves fine at a container root, but the update
/// path must be routed to the worktree - mirroring what discovery already does.
fn resolve_update_work_tree(target_dir: &std::path::Path) -> Result<std::path::PathBuf> {
    if crate::bare::is_bare_container(target_dir) {
        crate::bare::default_worktree(target_dir)
    } else {
        Ok(target_dir.to_path_buf())
    }
}

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
            let stash_result = run_checked(
                Command::new("git").args([
                    "-C",
                    &repo_path.to_string_lossy(),
                    "stash",
                    "push",
                    "-m",
                    "gx auto-stash for clone update",
                ]),
                subprocess_timeout(),
            );

            if let Ok(output) = stash_result {
                if output.status.success() {
                    stashed = true;
                    debug!("Successfully stashed changes");
                }
            }
        }
    }

    // Fetch latest changes from remote
    let fetch_result = run_checked(
        Command::new("git").args(["-C", &repo_path.to_string_lossy(), "fetch", "origin"]),
        subprocess_timeout(),
    );

    if let Err(e) = fetch_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to fetch from remote: {e}")),
        };
    }

    // Checkout default branch
    let checkout_result = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "checkout",
            &default_branch,
        ]),
        subprocess_timeout(),
    );

    if let Err(e) = checkout_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to checkout default branch: {e}")),
        };
    }

    // Pull latest (same as checkout: --ff-only)
    let pull_result = run_checked(
        Command::new("git").args(["-C", &repo_path.to_string_lossy(), "pull", "--ff-only"]),
        subprocess_timeout(),
    );

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
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ]),
        subprocess_timeout(),
    )
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
    Ok(parse_porcelain_status(&run_status_porcelain(repo_path)?))
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
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "checkout",
            "-b",
            branch_name,
        ]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git").args(["-C", &repo_path.to_string_lossy(), "checkout", branch_name]),
        subprocess_timeout(),
    )
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

/// Prove a local branch's commits are all contained in the repo's
/// freshly-fetched default base ref, the real guard `gx cleanup` runs before
/// `git branch -D` (production-hardening doc, Phase 4).
///
/// Resolves the repo's default base branch NAME via [`get_head_branch`]
/// (origin/HEAD, then main/master) - it does NOT assume `main`, falling back to
/// `main` (with a warning) only when nothing resolves. Then FETCHES `origin`
/// first (a containment test against a stale local base ref is worse than
/// useless) and delegates the proof to [`branch_changes_in_base`], a
/// PATCH-identity check (`git cherry origin/<base> <branch>`) that handles
/// gx's own squash merges -- unlike the old commit-identity
/// `git merge-base --is-ancestor`, which returned false for every
/// squash-merged branch (Bug 1).
///
/// - all branch patches present in base -> `Ok(true)` (safe to delete)
/// - a branch patch absent from base    -> `Ok(false)` (preserve)
/// - fetch failure / unverifiable cherry -> `Err` (cannot verify; caller fails
///   closed and preserves the branch)
pub fn branch_merged_into_base(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    debug!(
        "branch_merged_into_base: repo_path={} branch={branch_name}",
        repo_path.display()
    );
    let base = match get_head_branch(repo_path) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "Could not resolve default base branch for {}; falling back to main: {e}",
                repo_path.display()
            );
            "main".to_string()
        }
    };

    // Fetch first: the ancestry test MUST run against the up-to-date remote base
    // ref, not a stale local one.
    fetch_origin(repo_path).with_context(|| {
        format!(
            "Failed to fetch origin before ancestry check in {}",
            repo_path.display()
        )
    })?;

    let base_ref = format!("origin/{base}");
    branch_changes_in_base(repo_path, &base_ref, branch_name)
}

/// Prove -- by PATCH identity, not commit identity -- that every commit on
/// `branch_name` already has its diff present in `base_ref`. This is the
/// squash-merge-aware replacement for `git merge-base --is-ancestor` (a
/// commit-identity test that returns false for every squash-merged branch;
/// gx's own `review approve` merges with `--squash`, so the old guard skipped
/// every branch gx itself merged).
///
/// Runs `git cherry <base_ref> <branch_name>`, which lists each branch commit
/// with `-` (its patch is already in base, matched by patch-id) or `+` (its
/// patch is NOT in base). The branch's changes are all in base iff there are
/// ZERO `+` lines.
///
/// FAIL CLOSED (review finding #3): `run_checked` returns `Ok` on a non-zero
/// exit, and `git cherry` exits 0 whether or not `+` lines exist -- so a fatal
/// cherry (bad ref) yields empty stdout that would naively read as "no `+`
/// lines -> merged -> delete". This primitive therefore requires a SUCCESS
/// exit before interpreting stdout; any non-zero/error status is an `Err` that
/// propagates so cleanup PRESERVES the branch (never deletes on an ambiguous
/// cherry). Mirrors the exit-code mapping the old `--is-ancestor` guard did.
///
/// - exit 0, zero `+` lines -> `Ok(true)` (all changes in base; safe to delete)
/// - exit 0, one+ `+` lines  -> `Ok(false)` (unmerged change present; preserve)
/// - any other exit / spawn failure -> `Err` (cannot verify; caller fails closed)
fn branch_changes_in_base(
    repo_path: &std::path::Path,
    base_ref: &str,
    branch_name: &str,
) -> Result<bool> {
    debug!(
        "branch_changes_in_base: repo_path={} base_ref={base_ref} branch={branch_name}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "cherry",
            base_ref,
            branch_name,
        ]),
        subprocess_timeout(),
    )
    .context("Failed to execute git cherry")?;

    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let plus_lines = stdout.lines().filter(|line| line.starts_with('+')).count();
            debug!(
                "branch_changes_in_base: {branch_name} vs {base_ref} -> {plus_lines} unmerged (+) line(s)"
            );
            Ok(plus_lines == 0)
        }
        other => Err(eyre::eyre!(
            "git cherry {base_ref} {branch_name} failed (exit {other:?}): {}",
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Delete a local branch
pub fn delete_local_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "branch",
            "-D",
            branch_name,
        ]),
        subprocess_timeout(),
    )
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

/// Stage specific files (handles add, modify, and delete)
/// Uses "git add -A --" which stages all changes for the specified files:
/// - New files are added
/// - Modified files are staged
/// - Deleted files are staged for removal
pub fn add_files(repo_path: &std::path::Path, files: &[String]) -> Result<()> {
    if files.is_empty() {
        debug!("No files to stage in '{}'", repo_path.display());
        return Ok(());
    }

    // Use literal pathspecs (`:(literal)<path>`) so a tracked filename
    // containing glob metacharacters cannot be re-expanded by git ([A26]).
    let repo_path_str = repo_path.to_string_lossy().to_string();
    let literal_specs: Vec<String> = files.iter().map(|f| format!(":(literal){f}")).collect();
    let mut args: Vec<&str> = vec!["-C", &repo_path_str, "add", "-A", "--"];
    for spec in &literal_specs {
        args.push(spec.as_str());
    }

    let output = run_checked(Command::new("git").args(&args), subprocess_timeout())
        .context("Failed to execute git add")?;

    if output.status.success() {
        debug!(
            "Staged {} files in '{}': {:?}",
            files.len(),
            repo_path.display(),
            files
        );
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to stage files: {}", error))
    }
}

/// Commit staged changes with a message
pub fn commit_changes(repo_path: &std::path::Path, message: &str) -> Result<()> {
    let output = run_checked(
        Command::new("git").args(["-C", &repo_path.to_string_lossy(), "commit", "-m", message]),
        subprocess_timeout(),
    )
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

    let output = run_checked(
        Command::new("git")
            .env("GIT_SSH_COMMAND", ssh_command)
            .args([
                "-C",
                &repo_path.to_string_lossy(),
                "push",
                "--set-upstream",
                "origin",
                branch_name,
            ]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git").args(["-C", &repo_path.to_string_lossy(), "status", "--porcelain"]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "branch",
            "--show-current",
        ]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch_name}"),
        ]),
        subprocess_timeout(),
    )
    .context("Failed to execute git rev-parse")?;

    Ok(output.status.success())
}

/// Check if a branch exists on remote
pub fn branch_exists_on_remote(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{branch_name}"),
        ]),
        subprocess_timeout(),
    )
    .context("Failed to execute git rev-parse for remote branch")?;

    Ok(output.status.success())
}

/// Checkout a branch that exists on remote (creates local tracking branch)
pub fn checkout_remote_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "checkout",
            "-b",
            branch_name,
            &format!("origin/{branch_name}"),
        ]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git").args(["-C", &repo_path.to_string_lossy(), "pull"]),
        subprocess_timeout(),
    )
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

    let output = run_checked(
        Command::new("git").args(["clone", clone_url, &target_dir.to_string_lossy()]),
        subprocess_timeout(),
    )
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

/// Get the default/head branch name for the repository
pub fn get_head_branch(repo_path: &std::path::Path) -> Result<String> {
    // First try to get the default branch from remote
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["symbolic-ref", "refs/remotes/origin/HEAD"]),
        subprocess_timeout(),
    )
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
    let output = run_checked(
        Command::new("git").current_dir(repo_path).args([
            "ls-remote",
            "--heads",
            "origin",
            branch_name,
        ]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to check remote branch: {}", e))?;

    Ok(output.status.success() && !output.stdout.is_empty())
}

/// Read-only probe for whether `branch_name` exists on `origin`, using
/// `ls-remote --exit-code` so an offline/error condition is distinguishable
/// from a genuine absence. The `pushing`-phase recovery must fail closed when
/// it cannot reach the remote rather than assume the branch never landed.
///
/// - exit 0 (matching ref found) -> `Ok(true)`
/// - exit 2 (no matching ref) -> `Ok(false)`
/// - any other exit / spawn failure -> `Err` (cannot classify)
pub fn remote_branch_exists_probe(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    debug!(
        "remote_branch_exists_probe: repo_path={} branch={branch_name}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git").current_dir(repo_path).args([
            "ls-remote",
            "--exit-code",
            "--heads",
            "origin",
            branch_name,
        ]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to probe remote branch: {}", e))?;

    match output.status.code() {
        Some(0) => Ok(true),
        Some(2) => Ok(false),
        other => Err(eyre::eyre!(
            "Could not probe remote for branch {branch_name} (git ls-remote exit {:?}): {}",
            other,
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Delete a remote branch (for `gx cleanup`; rollback never deletes remote
/// branches -- that is `gx undo`'s job). Existence is checked explicitly
/// FIRST via [`remote_branch_exists_probe`] (F13) so an already-absent branch
/// is a no-op, never a caller sniffing the delete's stderr for
/// "remote ref does not exist".
pub fn delete_remote_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    if !remote_branch_exists_probe(repo_path, branch_name)? {
        debug!(
            "Remote branch '{branch_name}' already absent in '{}'; no-op",
            repo_path.display()
        );
        return Ok(());
    }

    let output = run_checked(
        Command::new("git").current_dir(repo_path).args([
            "push",
            "origin",
            "--delete",
            branch_name,
        ]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to delete remote branch: {}", e))?;

    if output.status.success() {
        debug!(
            "Deleted remote branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to delete remote branch: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Count the parents of a commit. `git rev-list --parents -n 1 <oid>` prints the
/// commit's own oid followed by each parent oid on one line; the parent count is
/// the number of tokens minus one. One parent -> squash/rebase (plain
/// `git revert`); two -> a true merge commit (`git revert -m 1`). This is the
/// authoritative dispatch for the Phase 6 revert path -- never inferred from the
/// PR's merge method.
pub fn commit_parent_count(repo_path: &std::path::Path, oid: &str) -> Result<usize> {
    debug!(
        "commit_parent_count: repo_path={} oid={oid}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["rev-list", "--parents", "-n", "1", oid]),
        subprocess_timeout(),
    )
    .context("Failed to execute git rev-list --parents")?;

    if !output.status.success() {
        return Err(eyre::eyre!(
            "Failed to list parents of {oid}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let tokens = line.split_whitespace().count();
    if tokens == 0 {
        return Err(eyre::eyre!("git rev-list returned no output for {oid}"));
    }
    let parents = tokens - 1;
    debug!("commit_parent_count: oid={oid} parents={parents}");
    Ok(parents)
}

/// Fetch all refs from `origin`. Read-only with respect to the remote; used by
/// the Phase 6 revert path so the revert branch is cut from the up-to-date base
/// head that the merge actually landed on.
pub fn fetch_origin(repo_path: &std::path::Path) -> Result<()> {
    debug!("fetch_origin: repo_path={}", repo_path.display());
    let ssh_command =
        SshCommandDetector::get_ssh_command().context("Failed to get SSH command for fetch")?;
    let output = run_checked(
        Command::new("git")
            .env("GIT_SSH_COMMAND", ssh_command)
            .current_dir(repo_path)
            .args(["fetch", "origin"]),
        subprocess_timeout(),
    )
    .context("Failed to execute git fetch origin")?;

    if output.status.success() {
        debug!("Fetched origin in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to fetch origin: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Create and check out `branch_name` at `start_point` (e.g. `origin/main`).
/// Fails if the branch already exists -- the caller detects collisions BEFORE
/// calling this so it can report and refuse rather than force (Phase 6).
pub fn create_branch_at(
    repo_path: &std::path::Path,
    branch_name: &str,
    start_point: &str,
) -> Result<()> {
    debug!(
        "create_branch_at: repo_path={} branch={branch_name} start_point={start_point}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git").current_dir(repo_path).args([
            "checkout",
            "-b",
            branch_name,
            start_point,
        ]),
        subprocess_timeout(),
    )
    .context("Failed to execute git checkout -b")?;

    if output.status.success() {
        debug!(
            "Created branch '{branch_name}' at '{start_point}' in '{}'",
            repo_path.display()
        );
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to create branch '{}' at '{}': {}",
            branch_name,
            start_point,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Revert `oid` on the current branch (`--no-edit`). `mainline` selects the
/// parent for a true merge commit (`Some(1)`), `None` for a single-parent
/// (squash/rebase) commit. A conflicting revert returns `Err` and the working
/// tree is left mid-revert: the caller reports the conflict per repo and leaves
/// the revert branch for manual resolution (undo NEVER force-resolves).
pub fn revert_commit(repo_path: &std::path::Path, oid: &str, mainline: Option<u32>) -> Result<()> {
    debug!(
        "revert_commit: repo_path={} oid={oid} mainline={mainline:?}",
        repo_path.display()
    );
    let mut args: Vec<String> = vec!["revert".to_string(), "--no-edit".to_string()];
    if let Some(m) = mainline {
        args.push("-m".to_string());
        args.push(m.to_string());
    }
    args.push(oid.to_string());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    let output = run_checked(
        Command::new("git").current_dir(repo_path).args(&arg_refs),
        subprocess_timeout(),
    )
    .context("Failed to execute git revert")?;

    if output.status.success() {
        debug!("Reverted {oid} in '{}'", repo_path.display());
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(eyre::eyre!(
            "git revert of {oid} failed: {} {}",
            stderr.trim(),
            stdout.trim()
        ))
    }
}

/// Pull latest changes from the remote repository, fast-forward only.
///
/// A non-fast-forward result is a per-repo error rather than a surprise merge
/// commit ([A14]). The dead `Already up to date` stderr sniff is gone - that
/// message goes to stdout on a zero exit ([A28]).
pub fn pull_latest_changes(repo_path: &std::path::Path) -> Result<()> {
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["pull", "--ff-only"]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to run git pull --ff-only: {}", e))?;

    if output.status.success() {
        debug!("Successfully pulled (ff-only) in '{}'", repo_path.display());
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to fast-forward pull (run `git pull` manually to resolve): {}",
            stderr
        ))
    }
}

/// Get the full SHA of HEAD.
pub fn get_head_sha(repo_path: &std::path::Path) -> Result<String> {
    debug!("get_head_sha: repo_path={}", repo_path.display());
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "HEAD"]),
        subprocess_timeout(),
    )
    .context("Failed to execute git rev-parse HEAD")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(eyre::eyre!(
            "Failed to get HEAD sha: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Stash current changes (including untracked, `-u`) and return the stash commit
/// SHA so it can later be re-applied by SHA rather than by a positional
/// `stash@{n}` that a concurrent stash could shift ([A15], design Q6).
pub fn stash_save_with_untracked(repo_path: &std::path::Path, message: &str) -> Result<String> {
    debug!(
        "stash_save_with_untracked: repo_path={} message={message}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["stash", "push", "-u", "-m", message]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to run git stash push -u: {}", e))?;

    if !output.status.success() {
        return Err(eyre::eyre!(
            "Failed to stash changes: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Resolve the SHA of the stash we just created.
    let sha_output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "stash@{0}"]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to resolve stash SHA: {}", e))?;

    if !sha_output.status.success() {
        return Err(eyre::eyre!(
            "Failed to resolve stash SHA after push: {}",
            String::from_utf8_lossy(&sha_output.stderr)
        ));
    }

    let sha = String::from_utf8_lossy(&sha_output.stdout)
        .trim()
        .to_string();
    debug!("stash_save_with_untracked: created stash {sha}");
    Ok(sha)
}

/// Resolve the commit SHA of the stash whose reflog subject contains `message`.
/// Returns `Ok(None)` when no stash matches (never created, or already popped).
/// Used by the message-keyed `PopStashByMessage` recovery step (F5): before the
/// stash's SHA is known, the message is the only handle recovery has.
pub fn stash_sha_by_message(repo_path: &std::path::Path, message: &str) -> Result<Option<String>> {
    debug!(
        "stash_sha_by_message: repo_path={} message={message}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["stash", "list", "--format=%H %gs"]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to list stashes: {}", e))?;

    if !output.status.success() {
        return Err(eyre::eyre!(
            "Failed to list stashes: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some((sha, subject)) = line.split_once(' ') {
            if subject.contains(message) {
                debug!("stash_sha_by_message: matched stash {sha} for {message:?}");
                return Ok(Some(sha.trim().to_string()));
            }
        }
    }
    Ok(None)
}

/// Apply a stash by its commit SHA. `git stash apply` accepts any stash-shaped
/// commit (unlike `pop`/`drop`, which need a positional ref). Returns an error
/// if the apply fails or conflicts; the caller decides whether to drop.
pub fn stash_apply_sha(repo_path: &std::path::Path, stash_sha: &str) -> Result<()> {
    debug!(
        "stash_apply_sha: repo_path={} stash_sha={stash_sha}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["stash", "apply", stash_sha]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to run git stash apply: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "git stash apply {} failed: {}",
            stash_sha,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Drop the stash entry whose commit SHA equals `stash_sha`.
///
/// `git stash drop` only accepts a positional `stash@{n}`, so we resolve `n`
/// from `git reflog show stash` by matching the SHA immediately before dropping
/// and re-verify the SHA at that index, so a concurrent stash mutation cannot
/// shift the entry and drop the wrong stash ([A15]).
pub fn stash_drop_by_sha(repo_path: &std::path::Path, stash_sha: &str) -> Result<()> {
    debug!(
        "stash_drop_by_sha: repo_path={} stash_sha={stash_sha}",
        repo_path.display()
    );
    let reflog = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["reflog", "show", "stash", "--format=%H"]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to read stash reflog: {}", e))?;

    if !reflog.status.success() {
        return Err(eyre::eyre!(
            "Failed to read stash reflog: {}",
            String::from_utf8_lossy(&reflog.stderr)
        ));
    }

    let reflog_text = String::from_utf8_lossy(&reflog.stdout);
    let index = reflog_text
        .lines()
        .position(|line| line.trim() == stash_sha)
        .ok_or_else(|| eyre::eyre!("Stash {} no longer present in reflog", stash_sha))?;

    let stash_ref = format!("stash@{{{index}}}");

    // Re-verify the SHA at that index before dropping.
    let verify = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", &stash_ref]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to verify stash ref {}: {}", stash_ref, e))?;
    let verified_sha = String::from_utf8_lossy(&verify.stdout).trim().to_string();
    if verified_sha != stash_sha {
        return Err(eyre::eyre!(
            "Stash index shifted: {} now resolves to {}, expected {}",
            stash_ref,
            verified_sha,
            stash_sha
        ));
    }

    let drop = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["stash", "drop", &stash_ref]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to drop stash: {}", e))?;

    if drop.status.success() {
        debug!("stash_drop_by_sha: dropped {stash_ref} ({stash_sha})");
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to drop stash {}: {}",
            stash_ref,
            String::from_utf8_lossy(&drop.stderr)
        ))
    }
}

/// Hard-reset to a specific SHA. Used during rollback to undo a commit back to a
/// recorded pre-commit HEAD (idempotent if HEAD is already there) ([A2]).
pub fn reset_hard_to_sha(repo_path: &std::path::Path, sha: &str) -> Result<()> {
    debug!(
        "reset_hard_to_sha: repo_path={} sha={sha}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["reset", "--hard", sha]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to run git reset --hard {}: {}", sha, e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to reset to {}: {}",
            sha,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Force-checkout a branch, discarding any uncommitted worktree changes. Used
/// during rollback to get off a gx branch before deleting it.
pub fn force_switch_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    debug!(
        "force_switch_branch: repo_path={} branch={branch_name}",
        repo_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["checkout", "-f", branch_name]),
        subprocess_timeout(),
    )
    .map_err(|e| eyre::eyre!("Failed to run git checkout -f {}: {}", branch_name, e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to force-switch to {}: {}",
            branch_name,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Convert raw bytes from git output into a `PathBuf`.
///
/// On Unix, git paths are arbitrary byte sequences; we preserve them exactly via
/// `OsStr::from_bytes`. On other platforms we fall back to a lossy conversion.
fn bytes_to_path(bytes: &[u8]) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        std::path::PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// List the files in the git index along with their octal mode strings.
///
/// Uses `git ls-files --stage -z` and parses the NUL-delimited records so that
/// non-UTF-8 filenames and paths containing newlines are handled correctly. Each
/// returned entry is `(mode, relative_path)` where `mode` is the raw octal mode
/// string git reports (e.g. `"100644"`, `"120000"` for symlinks, `"160000"` for
/// submodule gitlinks).
pub fn list_index_files(repo_path: &std::path::Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    debug!("list_index_files: repo_path={}", repo_path.display());
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["ls-files", "--stage", "-z"]),
        subprocess_timeout(),
    )
    .context("Failed to execute git ls-files --stage -z")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to list index files: {}", error));
    }

    let mut entries = Vec::new();
    for record in output.stdout.split(|&b| b == 0) {
        if record.is_empty() {
            continue;
        }
        // Each record is `<mode> <object> <stage>\t<path>`.
        let Some(tab) = record.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let (meta, path_with_tab) = record.split_at(tab);
        let path_bytes = &path_with_tab[1..]; // drop the leading tab
        let meta_str = String::from_utf8_lossy(meta);
        let mode = meta_str.split(' ').next().unwrap_or("").to_string();
        entries.push((mode, bytes_to_path(path_bytes)));
    }

    debug!("list_index_files: found {} entries", entries.len());
    Ok(entries)
}

/// Add a DETACHED worktree of `base_sha` at `worktree_path`, checked out from
/// the repo at `repo_path`. Used by the `llm` propose pass to give the agent a
/// throwaway checkout that shares the object store but is OUTSIDE the real
/// worktree, so nothing the agent does can touch tracked files (design
/// `2026-07-12-llm-propose-apply-and-mcp-server.md`, Chunk A propose step 2).
pub fn worktree_add_detached(
    repo_path: &std::path::Path,
    worktree_path: &std::path::Path,
    base_sha: &str,
) -> Result<()> {
    debug!(
        "worktree_add_detached: repo_path={} worktree_path={} base_sha={base_sha}",
        repo_path.display(),
        worktree_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["worktree", "add", "--detach"])
            .arg(worktree_path)
            .arg(base_sha),
        subprocess_timeout(),
    )
    .context("Failed to execute git worktree add")?;
    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to add temp worktree at {}: {}",
            worktree_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Remove a worktree previously added by [`worktree_add_detached`]. Best-effort
/// and forceful (the agent may have left the tree dirty): the propose pass
/// calls this on EVERY path, including errors, so a leaked worktree registration
/// never accumulates. Errors are returned for the caller to log, not to abort.
pub fn worktree_remove(repo_path: &std::path::Path, worktree_path: &std::path::Path) -> Result<()> {
    debug!(
        "worktree_remove: repo_path={} worktree_path={}",
        repo_path.display(),
        worktree_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(repo_path)
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path),
        subprocess_timeout(),
    )
    .context("Failed to execute git worktree remove")?;
    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to remove temp worktree at {}: {}",
            worktree_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// `git add -A` in `worktree_path`: stage adds, modifications, and deletions so
/// a subsequent `git diff --cached` sees the full set of changes the agent made
/// (design Chunk A propose step 4). This runs ONLY in a throwaway worktree,
/// never the real one, so the "never `git add .` in the real tree" rule does
/// not apply here.
pub fn stage_all(worktree_path: &std::path::Path) -> Result<()> {
    debug!("stage_all: worktree_path={}", worktree_path.display());
    let output = run_checked(
        Command::new("git")
            .current_dir(worktree_path)
            .args(["add", "-A"]),
        subprocess_timeout(),
    )
    .context("Failed to execute git add -A")?;
    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to stage worktree changes: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Resolve the real repo root that owns a (possibly leftover) linked
/// worktree, via the worktree's OWN `.git` back-pointer
/// (`git rev-parse --git-common-dir`, which always names the MAIN repo's
/// `.git` for a linked worktree). Used by the propose pass's startup prune
/// (ringer addendum #7, design Risks: "worktrees under a gx-owned tmp root")
/// to self-heal a crashed prior run's leftover worktree: the git metadata
/// survives the crash even though gx's own in-process mapping does not.
/// Returns `None` if the path is gone or is no longer a valid git worktree
/// (already pruned by something else, or never one).
pub fn resolve_worktree_repo(
    worktree_path: &std::path::Path,
) -> Result<Option<std::path::PathBuf>> {
    debug!(
        "resolve_worktree_repo: worktree_path={}",
        worktree_path.display()
    );
    if !worktree_path.exists() {
        return Ok(None);
    }
    let output = run_checked(
        Command::new("git")
            .current_dir(worktree_path)
            .args(["rev-parse", "--git-common-dir"]),
        subprocess_timeout(),
    )
    .context("Failed to execute git rev-parse --git-common-dir")?;
    if !output.status.success() {
        debug!(
            "resolve_worktree_repo: {} is not a valid git worktree",
            worktree_path.display()
        );
        return Ok(None);
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return Ok(None);
    }
    // `--git-common-dir` is relative to CWD unless the process runs outside
    // the repo; joining then canonicalizing resolves either form.
    let common_dir = worktree_path.join(&raw);
    let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
    Ok(common_dir.parent().map(|p| p.to_path_buf()))
}

/// Unified `git diff --cached <base_sha>` in `worktree_path`, as a display
/// string. This is the DISPLAY patch for the proposal (design "diff for
/// display, blobs for apply"); apply never consumes it. Decoded lossily since
/// it is only ever shown to a human.
pub fn diff_cached_patch(worktree_path: &std::path::Path, base_sha: &str) -> Result<String> {
    debug!(
        "diff_cached_patch: worktree_path={} base_sha={base_sha}",
        worktree_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(worktree_path)
            .args(["diff", "--cached", base_sha]),
        subprocess_timeout(),
    )
    .context("Failed to execute git diff --cached")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(eyre::eyre!(
            "Failed to compute display diff: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Raw, NUL-terminated `git diff --cached --raw -z <base_sha>` in
/// `worktree_path`, returned as bytes so a non-UTF-8 path is preserved for the
/// payload-fidelity check (design payload matrix: non-UTF-8 paths are rejected).
/// The `--raw` format carries the src/dst file modes, which is how the propose
/// pass detects symlinks (`120000`), gitlinks/submodules (`160000`), and
/// executable-bit changes without a special case.
pub fn diff_cached_raw_z(worktree_path: &std::path::Path, base_sha: &str) -> Result<Vec<u8>> {
    debug!(
        "diff_cached_raw_z: worktree_path={} base_sha={base_sha}",
        worktree_path.display()
    );
    let output = run_checked(
        Command::new("git")
            .current_dir(worktree_path)
            .args(["diff", "--cached", "--raw", "-z", base_sha]),
        subprocess_timeout(),
    )
    .context("Failed to execute git diff --cached --raw -z")?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(eyre::eyre!(
            "Failed to compute raw diff: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_update_work_tree_routes_bare_container_to_worktree() {
        let temp = tempfile::TempDir::new().unwrap();

        // A bare container: the update path must resolve to the default (main)
        // worktree, NOT the container root - the root is not a work tree, so
        // git status/checkout/pull there fail with exit 128 (the shipped bug).
        let container =
            crate::test_utils::create_bare_container(temp.path(), "gx", "scottidler/gx");

        // Document the bug this guards against: git status at the ROOT fails.
        let at_root = crate::test_utils::run_git_command(&["status", "--porcelain"], &container);
        assert!(
            !at_root.status.success(),
            "container root is not a work tree - proves why the update path must resolve the worktree"
        );

        let resolved = resolve_update_work_tree(&container).unwrap();
        assert!(
            resolved.ends_with("main"),
            "bare container must resolve to its default worktree, got {resolved:?}"
        );
        // And git status SUCCEEDS in the resolved worktree (the fix).
        let at_worktree = crate::test_utils::run_git_command(&["status", "--porcelain"], &resolved);
        assert!(
            at_worktree.status.success(),
            "git status must succeed in the resolved default worktree"
        );

        // A flat checkout resolves to itself.
        let flat = crate::test_utils::create_minimal_test_repo(temp.path(), "flat");
        assert_eq!(resolve_update_work_tree(&flat).unwrap(), flat);
    }

    #[test]
    fn test_branch_changes_in_base_fails_closed_on_bad_base_ref() {
        // FAIL-CLOSED (review finding #3, correctness-critical): a fatal
        // `git cherry` (bad/non-existent base ref -> exit 128) MUST map to
        // Err, never to "no + lines -> merged -> delete". `run_checked`
        // returns Ok on a non-zero exit and `git cherry` exits 0 regardless of
        // + lines, so without the success-exit gate the empty stdout of a
        // fatal cherry would read as "merged" and cleanup would delete an
        // unverified branch. If this returned Ok, that hole would be open.
        use crate::test_utils::run_git_command;
        let temp = tempfile::TempDir::new().unwrap();
        let repo = crate::test_utils::create_minimal_test_repo(temp.path(), "gx");

        // A real feature branch with one commit, so the BRANCH ref resolves;
        // only the BASE ref is bad -- isolating the fatal-cherry path.
        run_git_command(&["checkout", "--quiet", "-b", "feature"], &repo);
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        run_git_command(&["add", "-A"], &repo);
        run_git_command(&["commit", "--quiet", "-m", "feature commit"], &repo);

        let result = branch_changes_in_base(&repo, "origin/does-not-exist", "feature");
        assert!(
            result.is_err(),
            "a fatal git cherry (bad base ref) must be Err (fail closed), got {result:?}"
        );
    }

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
    fn test_parse_porcelain_status_table() {
        // (input, [modified, added, deleted, renamed, untracked, staged])
        let cases: &[(&str, [u32; 6])] = &[
            ("", [0, 0, 0, 0, 0, 0]),
            ("?? new.txt", [0, 0, 0, 0, 1, 0]),
            (" M mod.txt", [1, 0, 0, 0, 0, 0]),
            ("M  staged.txt", [0, 0, 0, 0, 0, 1]),
            ("A  added.txt", [0, 1, 0, 0, 0, 0]),
            (" D del.txt", [0, 0, 1, 0, 0, 0]),
            ("R  old.txt -> new.txt", [0, 0, 0, 1, 0, 0]),
            ("MM both.txt", [1, 0, 0, 0, 0, 1]),
            ("?? a\n M b\nA  c\n D d", [1, 1, 1, 0, 1, 0]),
        ];
        for (input, expected) in cases {
            let c = parse_porcelain_status(input);
            assert_eq!(
                [
                    c.modified,
                    c.added,
                    c.deleted,
                    c.renamed,
                    c.untracked,
                    c.staged
                ],
                *expected,
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn test_get_current_branch_name_empty_on_detached_head() {
        // The detached-HEAD guard ([A30]) keys off an empty branch name.
        use crate::test_utils::run_git_command;
        let temp = tempfile::TempDir::new().unwrap();
        let p = temp.path();
        run_git_command(&["init", "--quiet"], p);
        run_git_command(&["config", "user.email", "t@e.com"], p);
        run_git_command(&["config", "user.name", "T"], p);
        run_git_command(&["config", "commit.gpgsign", "false"], p);
        std::fs::write(p.join("a.txt"), "a").unwrap();
        run_git_command(&["add", "-A"], p);
        run_git_command(&["commit", "--quiet", "-m", "one"], p);
        std::fs::write(p.join("b.txt"), "b").unwrap();
        run_git_command(&["add", "-A"], p);
        run_git_command(&["commit", "--quiet", "-m", "two"], p);

        // Detach HEAD onto the first commit.
        run_git_command(&["checkout", "--quiet", "HEAD~1"], p);
        assert_eq!(get_current_branch_name(p).unwrap(), "");
    }

    #[test]
    fn test_add_files_literal_pathspec() {
        use crate::test_utils::run_git_command;
        let temp = tempfile::TempDir::new().unwrap();
        let p = temp.path();
        run_git_command(&["init", "--quiet"], p);
        run_git_command(&["config", "user.email", "t@e.com"], p);
        run_git_command(&["config", "user.name", "T"], p);
        run_git_command(&["config", "commit.gpgsign", "false"], p);

        // A tracked file whose name contains glob metacharacters.
        std::fs::write(p.join("f[1].txt"), "orig").unwrap();
        run_git_command(&["add", "-A"], p);
        run_git_command(&["commit", "--quiet", "-m", "init"], p);
        std::fs::write(p.join("f[1].txt"), "changed").unwrap();

        // Literal pathspec stages the file whose name IS `f[1].txt` (a glob
        // pathspec would instead try to match `f1.txt` and stage nothing).
        add_files(p, &["f[1].txt".to_string()]).unwrap();

        let staged = run_git_command(&["diff", "--cached", "--name-only"], p);
        let names = String::from_utf8_lossy(&staged.stdout);
        assert!(names.contains("f[1].txt"), "staged: {names:?}");
    }

    #[test]
    fn test_delete_remote_branch_absent_is_no_op() {
        // F13: an already-absent remote branch is a no-op (explicit
        // `ls-remote --exit-code` probe), not something the caller has to
        // sniff an error string for.
        use crate::test_utils::run_git_command;
        let bare_dir = tempfile::TempDir::new().unwrap();
        let bare = bare_dir.path();
        run_git_command(&["init", "--quiet", "--bare"], bare);

        let repo_dir = tempfile::TempDir::new().unwrap();
        let repo = repo_dir.path();
        run_git_command(&["init", "--quiet", "-b", "main"], repo);
        run_git_command(&["config", "user.email", "t@e.com"], repo);
        run_git_command(&["config", "user.name", "T"], repo);
        run_git_command(&["config", "commit.gpgsign", "false"], repo);
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        run_git_command(&["add", "-A"], repo);
        run_git_command(&["commit", "--quiet", "-m", "init"], repo);
        run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], repo);
        run_git_command(&["push", "--quiet", "-u", "origin", "main"], repo);

        // "GX-never-pushed" exists nowhere on the remote.
        assert!(delete_remote_branch(repo, "GX-never-pushed").is_ok());
    }

    #[test]
    fn test_delete_remote_branch_deletes_when_present() {
        use crate::test_utils::run_git_command;
        let bare_dir = tempfile::TempDir::new().unwrap();
        let bare = bare_dir.path();
        run_git_command(&["init", "--quiet", "--bare"], bare);

        let repo_dir = tempfile::TempDir::new().unwrap();
        let repo = repo_dir.path();
        run_git_command(&["init", "--quiet", "-b", "main"], repo);
        run_git_command(&["config", "user.email", "t@e.com"], repo);
        run_git_command(&["config", "user.name", "T"], repo);
        run_git_command(&["config", "commit.gpgsign", "false"], repo);
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        run_git_command(&["add", "-A"], repo);
        run_git_command(&["commit", "--quiet", "-m", "init"], repo);
        run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], repo);
        run_git_command(&["push", "--quiet", "-u", "origin", "main"], repo);
        run_git_command(&["branch", "GX-pushed"], repo);
        run_git_command(&["push", "--quiet", "origin", "GX-pushed"], repo);
        // `remote_branch_exists_probe` asks the remote directly (`ls-remote`),
        // never a possibly-stale local `refs/remotes/origin/*` cache.
        assert!(remote_branch_exists_probe(repo, "GX-pushed").unwrap());

        delete_remote_branch(repo, "GX-pushed").unwrap();
        assert!(!remote_branch_exists_probe(repo, "GX-pushed").unwrap());
    }

    // Rollback tests - these would need a real git repository to test properly
    // For now, we'll add basic structure tests
    mod rollback_tests {
        use super::*;
        use std::fs;
        use tempfile::TempDir;

        /// Set up a git repo for tests. git is a declared requirement (CI has
        /// it), so this fails loud rather than silently `return`ing ([A31]).
        fn setup_test_repo() -> (TempDir, std::path::PathBuf) {
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let repo_path = temp_dir.path().to_path_buf();

            let output = run_checked(
                Command::new("git").current_dir(&repo_path).args(["init"]),
                subprocess_timeout(),
            )
            .expect("Failed to run git init");
            assert!(
                output.status.success(),
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            run_checked(
                Command::new("git").current_dir(&repo_path).args([
                    "config",
                    "user.email",
                    "test@example.com",
                ]),
                subprocess_timeout(),
            )
            .expect("Failed to set git email");
            run_checked(
                Command::new("git").current_dir(&repo_path).args([
                    "config",
                    "user.name",
                    "Test User",
                ]),
                subprocess_timeout(),
            )
            .expect("Failed to set git name");

            (temp_dir, repo_path)
        }

        #[test]
        fn test_has_uncommitted_changes() {
            let (_temp_dir, repo_path) = setup_test_repo();

            // Empty repo: no uncommitted changes.
            assert!(!has_uncommitted_changes(&repo_path).unwrap());

            // After writing an untracked file, status reports dirty.
            fs::write(repo_path.join("test.txt"), "test content").unwrap();
            assert!(has_uncommitted_changes(&repo_path).unwrap());
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
