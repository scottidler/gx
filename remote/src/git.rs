use crate::ssh::{SshCommandDetector, SshUrlBuilder};
use eyre::{Context, Result};
use local::git::{
    branch_changes_in_base, get_current_branch, get_current_commit_sha, get_remote_origin,
    get_remote_status_native, get_status_changes, get_status_changes_for_path, is_same_repo,
    resolve_update_work_tree, RemoteStatus, RepoStatus, StatusChanges,
};
use local::repo::Repo;
use local::subprocess::{run_checked, subprocess_timeout};
use log::{debug, warn};
use std::process::Command;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delete_remote_branch_absent_is_no_op() {
        // F13: an already-absent remote branch is a no-op (explicit
        // `ls-remote --exit-code` probe), not something the caller has to
        // sniff an error string for.
        use local::test_utils::run_git_command;
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
        use local::test_utils::run_git_command;
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
