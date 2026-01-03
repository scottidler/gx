//! Branch cleanup after PR merge
//!
//! Provides functionality to clean up local and remote branches
//! after PRs have been merged.

use crate::cli::Cli;
use crate::config::Config;
use crate::git;
use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus, StateManager};
use eyre::Result;
use log::{info, warn};

/// Result of a cleanup operation
#[derive(Debug)]
pub struct CleanupResult {
    #[allow(dead_code)]
    pub change_id: String,
    pub repos_cleaned: usize,
    pub repos_skipped: usize,
    pub repos_failed: usize,
    pub errors: Vec<String>,
}

/// Process cleanup command
pub fn process_cleanup_command(
    _cli: &Cli,
    _config: &Config,
    change_id: Option<&str>,
    all: bool,
    list: bool,
    include_remote: bool,
    force: bool,
) -> Result<()> {
    let state_manager = StateManager::new()?;

    if list {
        return list_cleanable_changes(&state_manager);
    }

    if all {
        return cleanup_all_merged(&state_manager, include_remote, force);
    }

    let change_id = change_id
        .ok_or_else(|| eyre::eyre!("Change ID required unless --all or --list is specified"))?;

    cleanup_single_change(&state_manager, change_id, include_remote, force)
}

/// List changes that can be cleaned up
fn list_cleanable_changes(state_manager: &StateManager) -> Result<()> {
    let states = state_manager.list()?;

    let cleanable: Vec<_> = states
        .iter()
        .filter(|s| {
            s.status == ChangeStatus::FullyMerged || s.status == ChangeStatus::PartiallyMerged
        })
        .collect();

    if cleanable.is_empty() {
        println!("No changes need cleanup.");
        return Ok(());
    }

    println!("Changes available for cleanup:\n");
    for state in cleanable {
        let repos_needing_cleanup = state.get_repos_needing_cleanup().len();
        let open_prs = state.get_open_prs().len();
        let total_repos = state.repositories.len();
        let merged = state
            .repositories
            .values()
            .filter(|r| r.status == RepoChangeStatus::PrMerged)
            .count();

        println!(
            "  📦 {} ({} repos, {} merged, {} open, {} need cleanup)",
            state.change_id, total_repos, merged, open_prs, repos_needing_cleanup
        );

        if let Some(desc) = &state.description {
            println!("     {}", desc);
        }
    }

    println!("\nRun `gx cleanup <change-id>` to clean up a specific change.");
    println!("Run `gx cleanup --all` to clean up all merged changes.");

    Ok(())
}

/// Clean up all merged changes
fn cleanup_all_merged(
    state_manager: &StateManager,
    include_remote: bool,
    force: bool,
) -> Result<()> {
    let states = state_manager.list()?;

    let cleanable: Vec<_> = states
        .into_iter()
        .filter(|s| {
            s.status == ChangeStatus::FullyMerged
                || (force && s.status == ChangeStatus::PartiallyMerged)
        })
        .collect();

    if cleanable.is_empty() {
        println!("No changes to clean up.");
        return Ok(());
    }

    println!("Cleaning up {} change(s)...\n", cleanable.len());

    let mut total_cleaned = 0;
    let mut total_skipped = 0;
    let mut total_failed = 0;

    for mut state in cleanable {
        match cleanup_change(&mut state, include_remote, force) {
            Ok(result) => {
                total_cleaned += result.repos_cleaned;
                total_skipped += result.repos_skipped;
                total_failed += result.repos_failed;

                // Update state
                state_manager.save(&state)?;

                // Delete state if fully cleaned
                if state.get_repos_needing_cleanup().is_empty() {
                    state_manager.delete(&state.change_id)?;
                }
            }
            Err(e) => {
                warn!("Failed to cleanup {}: {}", state.change_id, e);
                total_failed += 1;
            }
        }
    }

    println!("\n📊 Cleanup summary:");
    println!("   🧹 {} branches cleaned", total_cleaned);
    println!("   ⏭️  {} skipped", total_skipped);
    if total_failed > 0 {
        println!("   ❌ {} failed", total_failed);
    }

    Ok(())
}

/// Clean up a single change
fn cleanup_single_change(
    state_manager: &StateManager,
    change_id: &str,
    include_remote: bool,
    force: bool,
) -> Result<()> {
    let mut state = state_manager
        .load(change_id)?
        .ok_or_else(|| eyre::eyre!("Change not found: {}", change_id))?;

    let result = cleanup_change(&mut state, include_remote, force)?;

    // Update state
    state_manager.save(&state)?;

    // Print summary
    println!("\n📊 Cleanup for {}:", change_id);
    println!("   🧹 {} branches cleaned", result.repos_cleaned);
    println!("   ⏭️  {} skipped", result.repos_skipped);
    if result.repos_failed > 0 {
        println!("   ❌ {} failed", result.repos_failed);
        for error in &result.errors {
            println!("      - {}", error);
        }
    }

    // Delete state if fully cleaned
    if state.get_repos_needing_cleanup().is_empty() && result.repos_failed == 0 {
        state_manager.delete(change_id)?;
        println!("   ✅ Change state removed");
    }

    Ok(())
}

/// Clean up branches for a change
fn cleanup_change(
    state: &mut ChangeState,
    include_remote: bool,
    force: bool,
) -> Result<CleanupResult> {
    // Get repos needing cleanup - collect into owned data
    let repos_to_clean: Vec<_> = state
        .get_repos_needing_cleanup()
        .iter()
        .map(|r| (r.repo_slug.clone(), r.branch_name.clone(), r.status.clone()))
        .collect();

    let mut cleaned = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for (repo_slug, branch_name, status) in repos_to_clean {
        // Check if we should clean this repo
        if !force && status != RepoChangeStatus::PrMerged {
            info!("Skipping {} - PR not merged", repo_slug);
            skipped += 1;
            continue;
        }

        // Try to find local path
        let local_path = match find_repo_locally(&repo_slug) {
            Some(p) => p,
            None => {
                info!("Skipping {} - local repo not found", repo_slug);
                skipped += 1;
                continue;
            }
        };

        // Delete local branch
        match git::delete_local_branch(&local_path, &branch_name) {
            Ok(()) => {
                info!("🧹 Deleted local branch {} in {}", branch_name, repo_slug);
                cleaned += 1;

                // Mark as cleaned up in state
                state.mark_cleaned_up(&repo_slug);
            }
            Err(e) => {
                // Check if branch doesn't exist (already deleted)
                let err_str = e.to_string();
                if err_str.contains("not found") || err_str.contains("does not exist") {
                    info!("Branch {} already deleted in {}", branch_name, repo_slug);
                    state.mark_cleaned_up(&repo_slug);
                    skipped += 1;
                } else {
                    warn!(
                        "Failed to delete branch {} in {}: {}",
                        branch_name, repo_slug, e
                    );
                    errors.push(format!("{}: {}", repo_slug, e));
                    failed += 1;
                }
            }
        }

        // Optionally delete remote branch
        if include_remote {
            if let Err(e) = git::delete_remote_branch(&local_path, &branch_name) {
                // Remote branch might already be deleted by GitHub
                let err_str = e.to_string();
                if !err_str.contains("not found") && !err_str.contains("does not exist") {
                    warn!(
                        "Failed to delete remote branch {} in {}: {}",
                        branch_name, repo_slug, e
                    );
                }
            }
        }
    }

    Ok(CleanupResult {
        change_id: state.change_id.clone(),
        repos_cleaned: cleaned,
        repos_skipped: skipped,
        repos_failed: failed,
        errors,
    })
}

/// Try to find a repository locally by slug
fn find_repo_locally(repo_slug: &str) -> Option<std::path::PathBuf> {
    // Extract repo name from slug
    let repo_name = repo_slug.split('/').next_back()?;

    // Check current directory and parent directories
    let current = std::env::current_dir().ok()?;

    // Try: ./repo_name
    let direct = current.join(repo_name);
    if direct.join(".git").exists() {
        return Some(direct);
    }

    // Try: ./org/repo_name (full slug path)
    let with_org = current.join(repo_slug);
    if with_org.join(".git").exists() {
        return Some(with_org);
    }

    // Try: look in subdirectories matching org name
    if let Some(org) = repo_slug.split('/').next() {
        let org_dir = current.join(org);
        if org_dir.is_dir() {
            let repo_in_org = org_dir.join(repo_name);
            if repo_in_org.join(".git").exists() {
                return Some(repo_in_org);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_repo_locally_not_found() {
        let result = find_repo_locally("nonexistent/repo");
        assert!(result.is_none());
    }

    #[test]
    fn test_cleanup_result_debug() {
        let result = CleanupResult {
            change_id: "GX-test".to_string(),
            repos_cleaned: 2,
            repos_skipped: 1,
            repos_failed: 0,
            errors: vec![],
        };
        assert!(!format!("{:?}", result).is_empty());
    }

    #[test]
    fn test_cleanup_change_empty_state() {
        let mut state = ChangeState::new("test".to_string(), None);
        let result = cleanup_change(&mut state, false, false).unwrap();

        assert_eq!(result.repos_cleaned, 0);
        assert_eq!(result.repos_skipped, 0);
        assert_eq!(result.repos_failed, 0);
    }

    #[test]
    fn test_cleanup_change_with_repos_not_found() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("nonexistent/repo".to_string(), "GX-test".to_string());
        // Set status to merged so it would be eligible for cleanup
        state
            .repositories
            .get_mut("nonexistent/repo")
            .unwrap()
            .status = RepoChangeStatus::PrMerged;

        let result = cleanup_change(&mut state, false, false).unwrap();

        // Should skip because local repo not found
        assert_eq!(result.repos_cleaned, 0);
        assert_eq!(result.repos_skipped, 1);
        assert_eq!(result.repos_failed, 0);
    }

    #[test]
    fn test_list_cleanable_changes_empty() {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::with_dir(temp_dir.path().to_path_buf());

        // Should not error on empty state
        let result = list_cleanable_changes(&manager);
        assert!(result.is_ok());
    }
}
