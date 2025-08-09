use crate::repo::Repo;
use eyre::{Context, Result};
use log::debug;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct RepoStatus {
    pub repo: Repo,
    pub branch: Option<String>,
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
    UpToDate,      // ✅ Local and remote are in sync
    Ahead(u32),    // ⬆️  Local is ahead by N commits
    Behind(u32),   // ⬇️  Local is behind by N commits
    Diverged(u32, u32), // 🔀 Local ahead by N, behind by M
    NoRemote,      // 📍 No remote tracking branch
    Error(String), // ❌ Error checking remote status
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
    let remote_status = get_remote_status(repo, &branch);

    match get_status_changes(repo) {
        Ok(changes) => {
            let is_clean = changes.is_empty();
            RepoStatus {
                repo: repo.clone(),
                branch,
                is_clean,
                changes,
                remote_status,
                error: None,
            }
        }
        Err(e) => {
            RepoStatus {
                repo: repo.clone(),
                branch,
                is_clean: false,
                changes: StatusChanges::default(),
                remote_status,
                error: Some(e.to_string()),
            }
        }
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
        let branch = String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .to_string();

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
        let commit = String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .to_string();
        Some(format!("HEAD@{}", commit))
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

    let status_output = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in git status output")?;

    let mut changes = StatusChanges::default();

    for line in status_output.lines() {
        if line.len() < 2 {
            continue;
        }

        let index_status = line.chars().nth(0).unwrap_or(' ');
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

    // First check if there's a remote tracking branch
    let upstream_output = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg(&format!("{}@{{upstream}}", branch))
        .output();

    let upstream_branch = match upstream_output {
        Ok(output) if output.status.success() => {
            match String::from_utf8(output.stdout) {
                Ok(s) => s.trim().to_string(),
                Err(_) => return RemoteStatus::Error("Invalid UTF-8 in upstream branch".to_string()),
            }
        }
        _ => return RemoteStatus::NoRemote,
    };

    debug!("Checking remote status for {}: {} -> {}", repo.name, branch, upstream_branch);

    // Get ahead/behind counts
    let status_output = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .arg("rev-list")
        .arg("--left-right")
        .arg("--count")
        .arg(&format!("{}...{}", branch, upstream_branch))
        .output();

    match status_output {
        Ok(output) if output.status.success() => {
            let counts = match String::from_utf8(output.stdout) {
                Ok(s) => s,
                Err(_) => return RemoteStatus::Error("Invalid UTF-8 in rev-list output".to_string()),
            };
            let parts: Vec<&str> = counts.trim().split('\t').collect();

            if parts.len() == 2 {
                let ahead = parts[0].parse::<u32>().unwrap_or(0);
                let behind = parts[1].parse::<u32>().unwrap_or(0);

                debug!("Remote status for {}: ahead={}, behind={}", repo.name, ahead, behind);

                match (ahead, behind) {
                    (0, 0) => RemoteStatus::UpToDate,
                    (a, 0) if a > 0 => RemoteStatus::Ahead(a),
                    (0, b) if b > 0 => RemoteStatus::Behind(b),
                    (a, b) if a > 0 && b > 0 => RemoteStatus::Diverged(a, b),
                    _ => RemoteStatus::UpToDate,
                }
            } else {
                RemoteStatus::Error("Invalid rev-list output".to_string())
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            RemoteStatus::Error(format!("git rev-list failed: {}", stderr))
        }
        Err(e) => RemoteStatus::Error(format!("Failed to run git rev-list: {}", e)),
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