use crate::repo::Repo;
use crate::subprocess::{run_checked, subprocess_timeout};
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

/// Compute a repository's status using ONLY local refs -- current branch,
/// commit SHA, dirty state, and ahead/behind from the local tracking ref -- with
/// ZERO network access (never fetches). This is the credential-free status entry
/// the intel-catalog walk (Track B1) consumes; the fetch-capable
/// `get_repo_status_with_options` lives in the remote half. The Phase 2 boundary
/// grep over `local/src` enforces the zero-fetch guarantee structurally: this
/// function's call graph reaches `get_remote_status_native` (which reads the
/// LOCAL tracking ref), never `get_remote_status_with_fetch`.
pub fn get_repo_status_local(repo: &Repo) -> RepoStatus {
    debug!("get_repo_status_local: repo={}", repo.name);

    let branch = get_current_branch(repo);
    let commit_sha = get_current_commit_sha(repo);
    let remote_status = get_remote_status_native(repo);

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
pub fn get_current_commit_sha(repo: &Repo) -> Option<String> {
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
pub fn get_current_branch(repo: &Repo) -> Option<String> {
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
pub fn get_status_changes(repo: &Repo) -> Result<StatusChanges> {
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

/// Get remote tracking status using git status --porcelain --branch.
///
/// Reads the LOCAL tracking ref only (`refs/remotes/origin/*` as it stands on
/// disk); it NEVER fetches, so it is credential-free and belongs in `local`. The
/// fetch-capable wrapper (`get_remote_status_with_fetch`) lives in the remote
/// half and calls this after an optional `git fetch`.
pub fn get_remote_status_native(repo: &Repo) -> RemoteStatus {
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

/// The work tree that `git status`/`checkout`/`pull` must run in when updating an
/// existing repo. A bare container is ONE logical repo == its default worktree;
/// the container root is NOT a work tree, so those commands would fail there
/// (`fatal: this operation must be run in a work tree`). A flat checkout is
/// itself. `get_remote_origin` resolves fine at a container root, but the update
/// path must be routed to the worktree - mirroring what discovery already does.
pub fn resolve_update_work_tree(target_dir: &std::path::Path) -> Result<std::path::PathBuf> {
    if crate::bare::is_bare_container(target_dir) {
        crate::bare::default_worktree(target_dir)
    } else {
        Ok(target_dir.to_path_buf())
    }
}

/// Get remote origin URL for a repository
pub fn get_remote_origin(repo_path: &std::path::Path) -> Result<String> {
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
pub fn is_same_repo(remote_url: &str, expected_slug: &str) -> bool {
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
pub fn get_status_changes_for_path(repo_path: &std::path::Path) -> Result<StatusChanges> {
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
/// This is a purely local `git cherry` patch-identity proof, so it lives in
/// `local`; it is `pub` because the remote-half `branch_merged_into_base` (which
/// fetches `origin` first) delegates the containment proof to it across the
/// crate boundary.
///
/// - exit 0, zero `+` lines -> `Ok(true)` (all changes in base; safe to delete)
/// - exit 0, one+ `+` lines  -> `Ok(false)` (unmerged change present; preserve)
/// - any other exit / spawn failure -> `Err` (cannot verify; caller fails closed)
pub fn branch_changes_in_base(
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

/// Check if a branch exists in the LOCAL tracking ref (`refs/remotes/origin/*`).
///
/// This reads the on-disk tracking ref only (`git rev-parse --verify`); it never
/// contacts the remote, so it is credential-free. Contrast the remote-half
/// `branch_exists_remotely`, which really runs `git ls-remote`.
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

/// Checkout a branch that exists in the local tracking ref (creates a local
/// tracking branch from `origin/<branch>` already on disk; no network).
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
}
