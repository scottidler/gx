use crate::config::Config;
use crate::git;
use crate::github::{self, PrInfo};
use crate::output::{display_unified_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use crate::cli::Cli;
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub repo: Repo,
    pub change_id: String,
    #[allow(dead_code)]
    pub pr_number: Option<u64>,
    pub action: ReviewAction,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ReviewAction {
    Listed,           // PR information displayed
    Cloned,           // Repository cloned/updated
    Approved,         // PR approved and merged
    Deleted,          // PR closed and branch deleted
    Purged,           // All GX branches cleaned up
}

/// Process review ls command - list PRs by change ID
pub fn process_review_ls_command(
    cli: &Cli,
    _config: &Config,
    org: &str,
    _patterns: &[String],
    change_ids: &[String],
) -> Result<()> {
    info!("Listing PRs for org: {}, change IDs: {:?}", org, change_ids);

    let mut all_results = Vec::new();

    for change_id in change_ids {
        let prs = github::list_prs_by_change_id(org, change_id)
            .with_context(|| format!("Failed to list PRs for change ID: {}", change_id))?;

        info!("Found {} PRs for change ID: {}", prs.len(), change_id);

        for pr in prs {
            // Create a pseudo-repo for display purposes
            let repo = create_repo_from_slug(&pr.repo_slug);

            let result = ReviewResult {
                repo,
                change_id: change_id.clone(),
                pr_number: Some(pr.number),
                action: ReviewAction::Listed,
                error: None,
            };

            all_results.push(result);

            // Display PR info
            println!("PR #{}: {} ({})", pr.number, pr.title, pr.state_string());
            println!("  Repository: {}", pr.repo_slug);
            println!("  Branch: {}", pr.branch);
            println!("  Author: {}", pr.author);
            println!("  URL: {}", pr.url);
            println!();
        }
    }

    // Display unified results
    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_unified_results(&all_results, &opts);
    display_review_summary(&all_results, &opts);

    Ok(())
}

/// Process review clone command - clone repositories with PRs
pub fn process_review_clone_command(
    cli: &Cli,
    config: &Config,
    org: &str,
    _patterns: &[String],
    change_id: &str,
    include_closed: bool,
) -> Result<()> {
    info!("Cloning repositories for change ID: {}", change_id);

    let prs = github::list_prs_by_change_id(org, change_id)
        .with_context(|| format!("Failed to list PRs for change ID: {}", change_id))?;

    if prs.is_empty() {
        println!("No PRs found for change ID: {}", change_id);
        return Ok(());
    }

    let current_dir = std::env::current_dir()?;
    let base_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let org_dir = base_dir.join(org);

    // Determine parallelism
    let parallel_jobs = cli.parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process repositories in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        prs.par_iter()
            .filter(|pr| include_closed || pr.state != github::PrState::Closed)
            .map(|pr| clone_repo_for_pr(&org_dir, pr, change_id))
            .collect()
    });

    // Display results
    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_unified_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Process review approve command - approve and merge PRs
pub fn process_review_approve_command(
    cli: &Cli,
    config: &Config,
    org: &str,
    _patterns: &[String],
    change_id: &str,
    admin_override: bool,
) -> Result<()> {
    info!("Approving PRs for change ID: {}", change_id);

    let prs = github::list_prs_by_change_id(org, change_id)
        .with_context(|| format!("Failed to list PRs for change ID: {}", change_id))?;

    if prs.is_empty() {
        println!("No PRs found for change ID: {}", change_id);
        return Ok(());
    }

    // Filter to only open PRs
    let open_prs: Vec<_> = prs.iter()
        .filter(|pr| pr.state == github::PrState::Open)
        .collect();

    if open_prs.is_empty() {
        println!("No open PRs found for change ID: {}", change_id);
        return Ok(());
    }

    println!("Found {} open PRs to approve and merge:", open_prs.len());
    for pr in &open_prs {
        println!("  PR #{}: {} ({})", pr.number, pr.title, pr.repo_slug);
    }

    // Determine parallelism
    let parallel_jobs = cli.parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process PRs in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        open_prs.par_iter()
            .map(|pr| approve_and_merge_pr(pr, change_id, admin_override))
            .collect()
    });

    // Display results
    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_unified_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Process review delete command - close PRs and delete branches
pub fn process_review_delete_command(
    cli: &Cli,
    config: &Config,
    org: &str,
    _patterns: &[String],
    change_id: &str,
) -> Result<()> {
    info!("Deleting PRs for change ID: {}", change_id);

    let prs = github::list_prs_by_change_id(org, change_id)
        .with_context(|| format!("Failed to list PRs for change ID: {}", change_id))?;

    if prs.is_empty() {
        println!("No PRs found for change ID: {}", change_id);
        return Ok(());
    }

    // Filter to only open PRs
    let open_prs: Vec<_> = prs.iter()
        .filter(|pr| pr.state == github::PrState::Open)
        .collect();

    if open_prs.is_empty() {
        println!("No open PRs found for change ID: {}", change_id);
        return Ok(());
    }

    println!("Found {} open PRs to delete:", open_prs.len());
    for pr in &open_prs {
        println!("  PR #{}: {} ({})", pr.number, pr.title, pr.repo_slug);
    }

    // Determine parallelism
    let parallel_jobs = cli.parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process PRs in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        open_prs.par_iter()
            .map(|pr| delete_pr_and_branch(pr, change_id))
            .collect()
    });

    // Display results
    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_unified_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Process review purge command - clean up all GX branches and PRs
pub fn process_review_purge_command(
    cli: &Cli,
    config: &Config,
    org: &str,
    patterns: &[String],
) -> Result<()> {
    info!("Purging all GX branches and PRs for org: {}", org);

    // Discover repositories
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli.max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = discover_repos(start_dir, max_depth)
        .context("Failed to discover repositories")?;

    let filtered_repos = filter_repos(repos, patterns);

    if filtered_repos.is_empty() {
        println!("No repositories found matching the specified patterns.");
        return Ok(());
    }

    // Determine parallelism
    let parallel_jobs = cli.parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process repositories in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        filtered_repos.par_iter()
            .map(|repo| purge_gx_branches(repo))
            .collect()
    });

    // Display results
    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_unified_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Clone a repository for a specific PR
fn clone_repo_for_pr(org_dir: &Path, pr: &PrInfo, change_id: &str) -> ReviewResult {
    let repo_name = extract_repo_name(&pr.repo_slug);
    let repo_dir = org_dir.join(&repo_name);
    let repo = Repo::new(repo_dir.clone());

    if repo_dir.exists() {
        // Repository already exists, pull latest
        match git::pull_latest(&repo_dir) {
            Ok(()) => {
                info!("Updated existing repository: {}", repo_name);
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: None,
                }
            }
            Err(e) => {
                warn!("Failed to update repository {}: {}", repo_name, e);
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: Some(format!("Failed to update: {}", e)),
                }
            }
        }
    } else {
        // Clone the repository
        let clone_url = format!("https://github.com/{}.git", pr.repo_slug);
        match git::clone_repository(&clone_url, &repo_dir) {
            Ok(()) => {
                info!("Cloned repository: {}", repo_name);
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: None,
                }
            }
            Err(e) => {
                warn!("Failed to clone repository {}: {}", repo_name, e);
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: Some(format!("Failed to clone: {}", e)),
                }
            }
        }
    }
}

/// Approve and merge a PR
fn approve_and_merge_pr(pr: &PrInfo, change_id: &str, admin_override: bool) -> ReviewResult {
    let repo = create_repo_from_slug(&pr.repo_slug);

    match github::approve_and_merge_pr(&pr.repo_slug, pr.number, admin_override) {
        Ok(()) => {
            info!("Successfully approved and merged PR #{}", pr.number);
            ReviewResult {
                repo,
                change_id: change_id.to_string(),
                pr_number: Some(pr.number),
                action: ReviewAction::Approved,
                error: None,
            }
        }
        Err(e) => {
            warn!("Failed to approve and merge PR #{}: {}", pr.number, e);
            ReviewResult {
                repo,
                change_id: change_id.to_string(),
                pr_number: Some(pr.number),
                action: ReviewAction::Approved,
                error: Some(format!("Failed to approve/merge: {}", e)),
            }
        }
    }
}

/// Delete PR and its branch
fn delete_pr_and_branch(pr: &PrInfo, change_id: &str) -> ReviewResult {
    let repo = create_repo_from_slug(&pr.repo_slug);

    // First close the PR
    match github::close_pr(&pr.repo_slug, pr.number) {
        Ok(()) => {
            // Then delete the remote branch
            match github::delete_remote_branch(&pr.repo_slug, &pr.branch) {
                Ok(()) => {
                    info!("Successfully deleted PR #{} and branch {}", pr.number, pr.branch);
                    ReviewResult {
                        repo,
                        change_id: change_id.to_string(),
                        pr_number: Some(pr.number),
                        action: ReviewAction::Deleted,
                        error: None,
                    }
                }
                Err(e) => {
                    warn!("Closed PR #{} but failed to delete branch {}: {}", pr.number, pr.branch, e);
                    ReviewResult {
                        repo,
                        change_id: change_id.to_string(),
                        pr_number: Some(pr.number),
                        action: ReviewAction::Deleted,
                        error: Some(format!("Failed to delete branch: {}", e)),
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to close PR #{}: {}", pr.number, e);
            ReviewResult {
                repo,
                change_id: change_id.to_string(),
                pr_number: Some(pr.number),
                action: ReviewAction::Deleted,
                error: Some(format!("Failed to close PR: {}", e)),
            }
        }
    }
}

/// Purge all GX branches from a repository
fn purge_gx_branches(repo: &Repo) -> ReviewResult {
    debug!("Purging GX branches from repository: {}", repo.name);

    if let Some(repo_slug) = &repo.slug {
        // List all GX branches (assuming they start with "GX-")
        match github::list_branches_with_prefix(repo_slug, "GX-") {
            Ok(branches) => {
                let mut errors = Vec::new();
                let mut deleted_count = 0;

                for branch in branches {
                    match github::delete_remote_branch(repo_slug, &branch) {
                        Ok(()) => {
                            deleted_count += 1;
                            debug!("Deleted branch: {}", branch);
                        }
                        Err(e) => {
                            errors.push(format!("Failed to delete {}: {}", branch, e));
                        }
                    }
                }

                if errors.is_empty() {
                    info!("Purged {} GX branches from {}", deleted_count, repo.name);
                    ReviewResult {
                        repo: repo.clone(),
                        change_id: "PURGE".to_string(),
                        pr_number: None,
                        action: ReviewAction::Purged,
                        error: None,
                    }
                } else {
                    warn!("Purged {} branches but had {} errors in {}", deleted_count, errors.len(), repo.name);
                    ReviewResult {
                        repo: repo.clone(),
                        change_id: "PURGE".to_string(),
                        pr_number: None,
                        action: ReviewAction::Purged,
                        error: Some(format!("Partial success: {}", errors.join("; "))),
                    }
                }
            }
            Err(e) => {
                warn!("Failed to list branches for {}: {}", repo.name, e);
                ReviewResult {
                    repo: repo.clone(),
                    change_id: "PURGE".to_string(),
                    pr_number: None,
                    action: ReviewAction::Purged,
                    error: Some(format!("Failed to list branches: {}", e)),
                }
            }
        }
    } else {
        ReviewResult {
            repo: repo.clone(),
            change_id: "PURGE".to_string(),
            pr_number: None,
            action: ReviewAction::Purged,
            error: Some("Repository has no slug, cannot purge remote branches".to_string()),
        }
    }
}

/// Create a pseudo-repo from a repository slug
fn create_repo_from_slug(repo_slug: &str) -> Repo {
    let repo_name = extract_repo_name(repo_slug);
    let mut repo = Repo::new(std::path::PathBuf::from(&repo_name));
    repo.slug = Some(repo_slug.to_string());
    repo
}

/// Extract repository name from a slug like "owner/repo"
fn extract_repo_name(repo_slug: &str) -> String {
    repo_slug.split('/').last().unwrap_or(repo_slug).to_string()
}

/// Display summary of review results
fn display_review_summary(results: &[ReviewResult], opts: &StatusOptions) {
    let total = results.len();
    let successful = results.iter().filter(|r| r.error.is_none()).count();
    let errors = total - successful;

    let listed = results.iter().filter(|r| matches!(r.action, ReviewAction::Listed)).count();
    let cloned = results.iter().filter(|r| matches!(r.action, ReviewAction::Cloned)).count();
    let approved = results.iter().filter(|r| matches!(r.action, ReviewAction::Approved)).count();
    let deleted = results.iter().filter(|r| matches!(r.action, ReviewAction::Deleted)).count();
    let purged = results.iter().filter(|r| matches!(r.action, ReviewAction::Purged)).count();

    if opts.use_emoji {
        println!("\n📊 {} repositories processed:", total);
        if listed > 0 {
            println!("   📋 {} PRs listed", listed);
        }
        if cloned > 0 {
            println!("   📥 {} repositories cloned/updated", cloned);
        }
        if approved > 0 {
            println!("   ✅ {} PRs approved and merged", approved);
        }
        if deleted > 0 {
            println!("   ❌ {} PRs deleted", deleted);
        }
        if purged > 0 {
            println!("   🧹 {} repositories purged", purged);
        }
        if errors > 0 {
            println!("   ❌ {} errors", errors);
        }
    } else {
        println!("\nSummary: {} repositories processed:", total);
        if listed > 0 {
            println!("   {} PRs listed", listed);
        }
        if cloned > 0 {
            println!("   {} repositories cloned/updated", cloned);
        }
        if approved > 0 {
            println!("   {} PRs approved and merged", approved);
        }
        if deleted > 0 {
            println!("   {} PRs deleted", deleted);
        }
        if purged > 0 {
            println!("   {} repositories purged", purged);
        }
        if errors > 0 {
            println!("   {} errors", errors);
        }
    }
}

/// Implement state_string for PrInfo
impl PrInfo {
    pub fn state_string(&self) -> &str {
        match self.state {
            github::PrState::Open => "Open",
            github::PrState::Closed => "Closed",
            github::PrState::Merged => "Merged",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_repo_name() {
        assert_eq!(extract_repo_name("owner/repo"), "repo");
        assert_eq!(extract_repo_name("tatari-tv/frontend"), "frontend");
        assert_eq!(extract_repo_name("single"), "single");
        assert_eq!(extract_repo_name(""), "");
    }

    #[test]
    fn test_create_repo_from_slug() {
        let repo = create_repo_from_slug("owner/test-repo");
        assert_eq!(repo.name, "test-repo");
        assert_eq!(repo.slug, Some("owner/test-repo".to_string()));
    }

    #[test]
    fn test_review_result_debug() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo = Repo::new(temp_dir.path().to_path_buf());

        let result = ReviewResult {
            repo,
            change_id: "test-change".to_string(),
            pr_number: Some(123),
            action: ReviewAction::Listed,
            error: None,
        };

        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("test-change"));
        assert!(debug_str.contains("Listed"));
        assert!(debug_str.contains("123"));
    }

    #[test]
    fn test_review_action_debug() {
        let actions = vec![
            ReviewAction::Listed,
            ReviewAction::Cloned,
            ReviewAction::Approved,
            ReviewAction::Deleted,
            ReviewAction::Purged,
        ];

        for action in actions {
            assert!(!format!("{:?}", action).is_empty());
        }
    }
}
