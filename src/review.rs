use crate::cli::Cli;
use crate::config::Config;
use crate::git;
use crate::github::{self, PrInfo};
use crate::output::{display_review_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use crate::ssh::SshUrlBuilder;
use crate::state::StateManager;
use eyre::{Context, Result};
use log::{debug, info, trace, warn};
use rayon::prelude::*;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub repo: Repo,
    pub change_id: String,
    pub pr_number: Option<u64>,
    pub action: ReviewAction,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ReviewAction {
    Listed,   // PR information displayed
    Cloned,   // Repository cloned/updated
    Approved, // PR approved and merged
    Deleted,  // PR closed and branch deleted
    Purged,   // All GX branches cleaned up
}

/// Process review ls command - list PRs by change ID
pub fn process_review_ls_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    _patterns: &[String],
    change_ids: &[String],
) -> Result<()> {
    // Discover repositories for auto-detection
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = crate::repo::discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    // Determine user/org(s) with precedence
    let user_org_contexts =
        crate::user_org::determine_user_orgs(org, cli.user_org.as_deref(), &repos, config)?;

    if user_org_contexts.is_empty() {
        eprintln!("Error: No organization detected. Use --org <org> to specify one.");
        eprintln!("Example: gx review --org tatari-tv ls");
        return Ok(());
    }

    info!(
        "Using {} org(s): {}",
        user_org_contexts.len(),
        user_org_contexts
            .iter()
            .map(|ctx| format!(
                "{} ({})",
                ctx.user_or_org,
                format!("{:?}", ctx.detection_method).to_lowercase()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // If no change IDs provided, search for all GX- prefixed PRs
    let search_patterns: Vec<String> = if change_ids.is_empty() {
        vec!["GX-".to_string()]
    } else {
        change_ids.to_vec()
    };

    info!("Listing PRs for patterns: {search_patterns:?}");

    let mut all_results = Vec::new();

    // Process each org and pattern combination
    for context in &user_org_contexts {
        for pattern in &search_patterns {
            match github::list_prs_by_change_id(&context.user_or_org, pattern, config) {
                Ok(prs) => {
                    info!(
                        "Found {} PRs for pattern '{}' in org '{}'",
                        prs.len(),
                        pattern,
                        context.user_or_org
                    );

                    for pr in prs {
                        // Create a pseudo-repo for display purposes
                        let repo = create_repo_from_slug(&pr.repo_slug);

                        let result = ReviewResult {
                            repo,
                            change_id: pr.branch.clone(),
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
                Err(e) => {
                    log::warn!(
                        "Failed to get PRs from org '{}' for pattern '{}': {}",
                        context.user_or_org,
                        pattern,
                        e
                    );
                }
            }
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

    display_review_results(&all_results, &opts);
    display_review_summary(&all_results, &opts);

    Ok(())
}

/// Process review clone command - clone repositories with PRs
pub fn process_review_clone_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    _patterns: &[String],
    change_id: &str,
    include_closed: bool,
) -> Result<()> {
    info!("Cloning repositories for change ID: {change_id}");

    // Discover repositories for auto-detection
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = crate::repo::discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    // Determine user/org(s) with precedence
    let user_org_contexts =
        crate::user_org::determine_user_orgs(org, cli.user_org.as_deref(), &repos, config)?;

    info!(
        "Using {} org(s): {}",
        user_org_contexts.len(),
        user_org_contexts
            .iter()
            .map(|ctx| format!(
                "{} ({})",
                ctx.user_or_org,
                format!("{:?}", ctx.detection_method).to_lowercase()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Collect all PRs from all orgs
    let mut all_prs = Vec::new();
    for context in &user_org_contexts {
        match github::list_prs_by_change_id(&context.user_or_org, change_id, config) {
            Ok(mut prs) => {
                info!(
                    "Found {} PRs for change ID '{}' in org '{}'",
                    prs.len(),
                    change_id,
                    context.user_or_org
                );
                all_prs.append(&mut prs);
            }
            Err(e) => {
                log::warn!(
                    "Failed to get PRs from org '{}' for change ID '{}': {}",
                    context.user_or_org,
                    change_id,
                    e
                );
            }
        }
    }

    if all_prs.is_empty() {
        println!("No PRs found for change ID: {change_id}");
        return Ok(());
    }

    let current_dir = std::env::current_dir()?;
    let base_dir = cli.cwd.as_deref().unwrap_or(&current_dir);

    // Determine parallelism
    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process repositories in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        all_prs
            .par_iter()
            // The search no longer filters to open-only (Phase 4 [F11]), so
            // treat Merged the same as Closed here to preserve prior behavior:
            // `--all`/`include_closed` is required to clone a repo whose PR is
            // no longer open, whether it landed or was abandoned.
            .filter(|pr| {
                include_closed
                    || !matches!(pr.state, github::PrState::Closed | github::PrState::Merged)
            })
            .map(|pr| {
                // Extract org from repo slug for directory structure
                let org_name = pr.repo_slug.split('/').next().unwrap_or("unknown");
                let org_dir = base_dir.join(org_name);
                clone_repo_for_pr(&org_dir, pr, change_id)
            })
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

    display_review_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Process review approve command - approve and merge PRs
pub fn process_review_approve_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    _patterns: &[String],
    change_id: &str,
    admin_override: bool,
    auto_merge: bool,
) -> Result<()> {
    info!("Approving PRs for change ID: {change_id}");

    // Discover repositories for org auto-detection
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = crate::repo::discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    let user_org_contexts =
        crate::user_org::determine_user_orgs(org, cli.user_org.as_deref(), &repos, config)?;

    if user_org_contexts.is_empty() {
        eprintln!("Error: No organization detected. Use --org <org> to specify one.");
        return Ok(());
    }

    // Collect PRs from all detected orgs
    let mut all_prs = Vec::new();
    for context in &user_org_contexts {
        match github::list_prs_by_change_id(&context.user_or_org, change_id, config) {
            Ok(prs) => all_prs.extend(prs),
            Err(e) => {
                log::warn!(
                    "Failed to get PRs from org '{}': {}",
                    context.user_or_org,
                    e
                );
            }
        }
    }
    let prs = all_prs;

    if prs.is_empty() {
        println!("No PRs found for change ID: {change_id}");
        return Ok(());
    }

    // Filter to only open PRs
    let open_prs: Vec<_> = prs
        .iter()
        .filter(|pr| pr.state == github::PrState::Open)
        .collect();

    if open_prs.is_empty() {
        println!("No open PRs found for change ID: {change_id}");
        return Ok(());
    }

    println!("Found {} open PRs to approve and merge:", open_prs.len());
    for pr in &open_prs {
        println!("  PR #{}: {} ({})", pr.number, pr.title, pr.repo_slug);
    }

    // Determine parallelism
    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process PRs in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        open_prs
            .par_iter()
            .map(|pr| approve_and_merge_pr(pr, change_id, admin_override, auto_merge, config))
            .collect()
    });

    // Single race-free state update: load once, apply all outcomes, save once ([A10]).
    if let Ok(manager) = StateManager::new() {
        if let Ok(Some(mut state)) = manager.load(change_id) {
            for result in &results {
                match &result.error {
                    None => state.mark_merged(&result.repo.slug),
                    Some(e) => state.mark_failed(&result.repo.slug, e.clone()),
                }
            }
            if let Err(e) = manager.save(&state) {
                warn!("Failed to save change state after approve: {e}");
            }
        }
    }

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

    display_review_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Process review delete command - close PRs and delete branches
pub fn process_review_delete_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    _patterns: &[String],
    change_id: &str,
) -> Result<()> {
    info!("Deleting PRs for change ID: {change_id}");

    // Discover repositories for org auto-detection
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = crate::repo::discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    let user_org_contexts =
        crate::user_org::determine_user_orgs(org, cli.user_org.as_deref(), &repos, config)?;

    if user_org_contexts.is_empty() {
        eprintln!("Error: No organization detected. Use --org <org> to specify one.");
        return Ok(());
    }

    // Collect PRs from all detected orgs
    let mut all_prs = Vec::new();
    for context in &user_org_contexts {
        match github::list_prs_by_change_id(&context.user_or_org, change_id, config) {
            Ok(prs) => all_prs.extend(prs),
            Err(e) => {
                log::warn!(
                    "Failed to get PRs from org '{}': {}",
                    context.user_or_org,
                    e
                );
            }
        }
    }
    let prs = all_prs;

    if prs.is_empty() {
        println!("No PRs found for change ID: {change_id}");
        return Ok(());
    }

    // Filter to only open PRs
    let open_prs: Vec<_> = prs
        .iter()
        .filter(|pr| pr.state == github::PrState::Open)
        .collect();

    if open_prs.is_empty() {
        println!("No open PRs found for change ID: {change_id}");
        return Ok(());
    }

    println!("Found {} open PRs to delete:", open_prs.len());
    for pr in &open_prs {
        println!("  PR #{}: {} ({})", pr.number, pr.title, pr.repo_slug);
    }

    // Determine parallelism
    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process PRs in parallel
    let results: Vec<ReviewResult> = pool.install(|| {
        open_prs
            .par_iter()
            .map(|pr| delete_pr_and_branch(pr, change_id, config))
            .collect()
    });

    // Single race-free state update: load once, mark closed, save once ([A10]).
    if let Ok(manager) = StateManager::new() {
        if let Ok(Some(mut state)) = manager.load(change_id) {
            for result in &results {
                if result.error.is_none() {
                    state.mark_closed(&result.repo.slug);
                }
            }
            if let Err(e) = manager.save(&state) {
                warn!("Failed to save change state after delete: {e}");
            }
        }
    }

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

    display_review_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// Process review sync command - true-up recorded change state against
/// GitHub PR reality (Phase 4 [F11], F14). Reconciles merged/closed PRs into
/// `mark_merged`/`mark_closed` so `gx cleanup`/`gx rollback cleanup` see the
/// current state instead of whatever was last recorded by a `create`/`approve`
/// run that may itself have crashed before updating state.
pub fn process_review_sync_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    _patterns: &[String],
    change_id: &str,
) -> Result<()> {
    info!("Syncing change state for change ID: {change_id}");

    // Discover repositories for org auto-detection.
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = crate::repo::discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    let user_org_contexts =
        crate::user_org::determine_user_orgs(org, cli.user_org.as_deref(), &repos, config)?;

    if user_org_contexts.is_empty() {
        eprintln!("Error: No organization detected. Use --org <org> to specify one.");
        return Ok(());
    }

    // Collect PRs (every state, not just open - Phase 4 broadened the search)
    // from all detected orgs.
    let mut all_prs = Vec::new();
    for context in &user_org_contexts {
        match github::list_prs_by_change_id(&context.user_or_org, change_id, config) {
            Ok(prs) => all_prs.extend(prs),
            Err(e) => {
                warn!(
                    "Failed to get PRs from org '{}': {}",
                    context.user_or_org, e
                );
            }
        }
    }

    if all_prs.is_empty() {
        println!("No PRs found for change ID: {change_id}");
        return Ok(());
    }

    let (merged, closed, status) = sync_change_state(&all_prs, change_id)?;

    println!("Synced {change_id}: {merged} merged, {closed} closed (aggregate status: {status:?})");
    Ok(())
}

/// Core of `gx review sync`: reconcile already-fetched PR info into the
/// recorded change state and save once. Split from the command shell above so
/// tests can exercise the reconciliation logic directly with a `gh`-shimmed
/// [`github::list_prs_by_change_id`] result, without needing repo discovery or
/// org auto-detection.
fn sync_change_state(
    prs: &[PrInfo],
    change_id: &str,
) -> Result<(usize, usize, crate::state::ChangeStatus)> {
    debug!("sync_change_state: change_id={change_id} prs={}", prs.len());
    let manager = StateManager::new()?;
    let mut state = manager
        .load(change_id)?
        .ok_or_else(|| eyre::eyre!("No change state recorded for {change_id}"))?;

    let mut merged = 0;
    let mut closed = 0;
    for pr in prs {
        trace!(
            "sync_change_state: repo={} pr=#{} state={:?} base={} merged_at={:?} merge_commit={:?}",
            pr.repo_slug,
            pr.number,
            pr.state,
            pr.base_ref_name,
            pr.merged_at,
            pr.merge_commit_oid
        );
        match pr.state {
            github::PrState::Merged => {
                state.mark_merged(&pr.repo_slug);
                merged += 1;
            }
            github::PrState::Closed => {
                state.mark_closed(&pr.repo_slug);
                closed += 1;
            }
            github::PrState::Open => {}
        }
    }

    manager.save(&state)?;
    debug!(
        "sync_change_state: change_id={change_id} merged={merged} closed={closed} status={:?}",
        state.status
    );
    Ok((merged, closed, state.status.clone()))
}

/// Process review purge command - clean up all GX branches and PRs
pub fn process_review_purge_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    patterns: &[String],
    yes: bool,
) -> Result<()> {
    info!("Purging gx branches for org: {org:?}");

    // Discover repositories
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    let filtered_repos = filter_repos(repos, patterns);

    if filtered_repos.is_empty() {
        println!("No repositories found matching the specified patterns.");
        return Ok(());
    }

    // Determine parallelism
    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Build the purge plan: per repo, the gx-created (GX-) branches with NO open
    // PR are deletable; branches that still have an open PR are refused ([A12], Q3).
    let plan: Vec<PurgePlan> = pool.install(|| {
        filtered_repos
            .par_iter()
            .map(|repo| build_purge_plan(repo, config))
            .collect()
    });

    let total_deletable: usize = plan.iter().map(|p| p.to_delete.len()).sum();
    let total_blocked: usize = plan.iter().map(|p| p.blocked.len()).sum();

    // Show the resolved plan.
    println!("Purge plan:");
    for p in &plan {
        for b in &p.to_delete {
            println!("  delete  {} {}", p.repo.slug, b);
        }
        for b in &p.blocked {
            println!(
                "  skip    {} {} (open PR; run `gx review delete` first)",
                p.repo.slug, b
            );
        }
        if let Some(err) = &p.error {
            println!("  error   {}: {}", p.repo.slug, err);
        }
    }
    println!("{total_deletable} branch(es) to delete, {total_blocked} skipped (open PR).");

    if total_deletable == 0 {
        return Ok(());
    }

    if !yes && !confirm_purge(total_deletable)? {
        println!("Aborted; no branches deleted.");
        return Ok(());
    }

    // Execute deletions in parallel.
    let results: Vec<ReviewResult> = pool.install(|| {
        plan.par_iter()
            .map(|p| purge_repo_branches(p, config))
            .collect()
    });

    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_review_results(&results, &opts);
    display_review_summary(&results, &opts);

    Ok(())
}

/// A per-repo purge plan: which gx branches can be deleted, which are blocked by
/// an open PR, and any error gathering the lists.
struct PurgePlan {
    repo: Repo,
    to_delete: Vec<String>,
    blocked: Vec<String>,
    error: Option<String>,
}

/// Compute the purge plan for one repo: gx-created (`GX-`) branches partitioned
/// into deletable (no open PR) vs. blocked (open PR).
fn build_purge_plan(repo: &Repo, config: &Config) -> PurgePlan {
    let slug = &repo.slug;
    let branches = match github::list_branches_with_prefix(slug, "GX-", config) {
        Ok(b) => b,
        Err(e) => {
            return PurgePlan {
                repo: repo.clone(),
                to_delete: Vec::new(),
                blocked: Vec::new(),
                error: Some(format!("Failed to list branches: {e}")),
            };
        }
    };
    let open_pr_branches = match github::list_open_pr_branches(slug, config) {
        Ok(b) => b,
        Err(e) => {
            return PurgePlan {
                repo: repo.clone(),
                to_delete: Vec::new(),
                blocked: Vec::new(),
                error: Some(format!("Failed to list open PRs: {e}")),
            };
        }
    };

    let (blocked, to_delete): (Vec<String>, Vec<String>) = branches
        .into_iter()
        .partition(|b| open_pr_branches.contains(b));

    PurgePlan {
        repo: repo.clone(),
        to_delete,
        blocked,
        error: None,
    }
}

/// Prompt for confirmation before purging. Fails closed on non-interactive stdin.
fn confirm_purge(count: usize) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(eyre::eyre!(
            "Refusing to purge {count} branches without confirmation on non-interactive stdin; pass --yes to proceed"
        ));
    }
    print!("Delete {count} gx branch(es)? (y/N): ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read confirmation from stdin")?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Delete the deletable branches in one repo's purge plan.
fn purge_repo_branches(plan: &PurgePlan, config: &Config) -> ReviewResult {
    let repo = plan.repo.clone();
    if let Some(err) = &plan.error {
        return ReviewResult {
            repo,
            change_id: "PURGE".to_string(),
            pr_number: None,
            action: ReviewAction::Purged,
            error: Some(err.clone()),
        };
    }

    let mut errors = Vec::new();
    let mut deleted = 0;
    for branch in &plan.to_delete {
        match github::delete_remote_branch(&plan.repo.slug, branch, config) {
            Ok(()) => deleted += 1,
            Err(e) => errors.push(format!("{branch}: {e}")),
        }
    }
    info!("Purged {} gx branches from {}", deleted, plan.repo.slug);

    ReviewResult {
        repo,
        change_id: "PURGE".to_string(),
        pr_number: None,
        action: ReviewAction::Purged,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        },
    }
}

/// Clone a repository for a specific PR
fn clone_repo_for_pr(org_dir: &Path, pr: &PrInfo, change_id: &str) -> ReviewResult {
    let repo_name = extract_repo_name(&pr.repo_slug);
    let repo_dir = org_dir.join(&repo_name);
    // Create repo object - use slug fallback if the directory isn't a valid repo yet
    let repo =
        Repo::new(repo_dir.clone()).unwrap_or_else(|_| Repo::from_slug(pr.repo_slug.clone()));

    if repo_dir.exists() {
        // Repository already exists, pull latest
        match git::pull_latest(&repo_dir) {
            Ok(()) => {
                info!("Updated existing repository: {repo_name}");
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: None,
                }
            }
            Err(e) => {
                warn!("Failed to update repository {repo_name}: {e}");
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: Some(format!("Failed to update: {e}")),
                }
            }
        }
    } else {
        // Clone the repository using SSH
        let clone_url = match SshUrlBuilder::build_ssh_url(&pr.repo_slug) {
            Ok(url) => url,
            Err(e) => {
                return ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: Some(format!("Invalid repository slug: {e}")),
                };
            }
        };
        match git::clone_repository(&clone_url, &repo_dir) {
            Ok(()) => {
                info!("Cloned repository: {repo_name}");
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: None,
                }
            }
            Err(e) => {
                warn!("Failed to clone repository {repo_name}: {e}");
                ReviewResult {
                    repo,
                    change_id: change_id.to_string(),
                    pr_number: Some(pr.number),
                    action: ReviewAction::Cloned,
                    error: Some(format!("Failed to clone: {e}")),
                }
            }
        }
    }
}

/// Approve and merge a PR
fn approve_and_merge_pr(
    pr: &PrInfo,
    change_id: &str,
    admin_override: bool,
    auto_merge: bool,
    config: &Config,
) -> ReviewResult {
    let repo = create_repo_from_slug(&pr.repo_slug);

    // State is updated once, after the parallel section completes (the caller),
    // to avoid a read-modify-write race across rayon workers ([A10]).
    match github::approve_and_merge_pr(&pr.repo_slug, pr.number, admin_override, auto_merge, config)
    {
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
                error: Some(format!("Failed to approve/merge: {e}")),
            }
        }
    }
}

/// Delete PR and its branch
fn delete_pr_and_branch(pr: &PrInfo, change_id: &str, config: &Config) -> ReviewResult {
    let repo = create_repo_from_slug(&pr.repo_slug);

    // State is updated once after the parallel section (the caller) to avoid a
    // read-modify-write race across rayon workers ([A10]).
    match github::close_pr(&pr.repo_slug, pr.number, config) {
        Ok(()) => {
            // Then delete the remote branch
            match github::delete_remote_branch(&pr.repo_slug, &pr.branch, config) {
                Ok(()) => {
                    info!(
                        "Successfully deleted PR #{} and branch {}",
                        pr.number, pr.branch
                    );
                    ReviewResult {
                        repo,
                        change_id: change_id.to_string(),
                        pr_number: Some(pr.number),
                        action: ReviewAction::Deleted,
                        error: None,
                    }
                }
                Err(e) => {
                    warn!(
                        "Closed PR #{} but failed to delete branch {}: {}",
                        pr.number, pr.branch, e
                    );
                    ReviewResult {
                        repo,
                        change_id: change_id.to_string(),
                        pr_number: Some(pr.number),
                        action: ReviewAction::Deleted,
                        error: Some(format!("Failed to delete branch: {e}")),
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
                error: Some(format!("Failed to close PR: {e}")),
            }
        }
    }
}

/// Create a pseudo-repo from a repository slug
fn create_repo_from_slug(repo_slug: &str) -> Repo {
    Repo::from_slug(repo_slug.to_string())
}

/// Extract repository name from a slug like "owner/repo"
fn extract_repo_name(repo_slug: &str) -> String {
    repo_slug
        .split('/')
        .next_back()
        .unwrap_or(repo_slug)
        .to_string()
}

/// Display summary of review results
fn display_review_summary(results: &[ReviewResult], opts: &StatusOptions) {
    let total = results.len();
    let successful = results.iter().filter(|r| r.error.is_none()).count();
    let errors = total - successful;

    let listed = results
        .iter()
        .filter(|r| matches!(r.action, ReviewAction::Listed))
        .count();
    let cloned = results
        .iter()
        .filter(|r| matches!(r.action, ReviewAction::Cloned))
        .count();
    let approved = results
        .iter()
        .filter(|r| matches!(r.action, ReviewAction::Approved))
        .count();
    let deleted = results
        .iter()
        .filter(|r| matches!(r.action, ReviewAction::Deleted))
        .count();
    let purged = results
        .iter()
        .filter(|r| matches!(r.action, ReviewAction::Purged))
        .count();

    if opts.use_emoji {
        println!("\n📊 {total} repositories processed:");
        if listed > 0 {
            println!("   📋 {listed} PRs listed");
        }
        if cloned > 0 {
            println!("   📥 {cloned} repositories cloned/updated");
        }
        if approved > 0 {
            println!("   ✅ {approved} PRs approved and merged");
        }
        if deleted > 0 {
            println!("   ❌ {deleted} PRs deleted");
        }
        if purged > 0 {
            println!("   🧹 {purged} repositories purged");
        }
        if errors > 0 {
            println!("   ❌ {errors} errors");
        }
    } else {
        println!("\nSummary: {total} repositories processed:");
        if listed > 0 {
            println!("   {listed} PRs listed");
        }
        if cloned > 0 {
            println!("   {cloned} repositories cloned/updated");
        }
        if approved > 0 {
            println!("   {approved} PRs approved and merged");
        }
        if deleted > 0 {
            println!("   {deleted} PRs deleted");
        }
        if purged > 0 {
            println!("   {purged} repositories purged");
        }
        if errors > 0 {
            println!("   {errors} errors");
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

impl ReviewResult {
    /// Get a formatted PR reference for display
    pub fn pr_reference(&self) -> String {
        match self.pr_number {
            Some(num) => format!("PR#{num}"),
            None => "-------".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus};
    use tempfile::TempDir;

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
        assert_eq!(repo.slug, "owner/test-repo".to_string());
    }

    /// A stub `gh` on PATH: asserts the invocation is `api graphql` carrying
    /// our search pattern (bite-proof - a wrong query fails the test loudly),
    /// then returns one canned MERGED PR as GraphQL JSON. Offline and
    /// deterministic, per the 2026-06-11 gh-shim precedent.
    const GH_SHIM_SCRIPT: &str = r#"#!/bin/sh
if [ "$1" != "api" ] || [ "$2" != "graphql" ]; then
  echo "gh shim: unexpected invocation: $@" >&2
  exit 1
fi
found_q=0
for arg in "$@"; do
  case "$arg" in
    q=*GX-sync-shim*) found_q=1 ;;
  esac
done
if [ "$found_q" != "1" ]; then
  echo "gh shim: expected q= arg containing GX-sync-shim, got: $@" >&2
  exit 1
fi
cat <<'JSON'
{"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{
  "number": 42,
  "title": "GX-sync-shim: change",
  "headRefName": "GX-sync-shim",
  "author": {"login": "tester"},
  "state": "MERGED",
  "url": "https://github.com/gx-testing/repo/pull/42",
  "repository": {"nameWithOwner": "gx-testing/repo"},
  "mergedAt": "2026-07-11T00:00:00Z",
  "mergeCommit": {"oid": "deadbeef"},
  "baseRefName": "main"
}]}}}
JSON
exit 0
"#;

    #[test]
    fn test_review_sync_marks_merged_pr_via_gh_shim() {
        // Phase 4 [F11] success criterion: a PR merged via gh (shimmed) shows
        // Merged after `review sync`. Exercises the REAL
        // github::list_prs_by_change_id (hitting a PATH-shimmed `gh`) piped
        // into `sync_change_state` - the exact path `gx review sync` runs.
        let guard = crate::test_utils::ENV_LOCK.lock().unwrap();
        let prior_path = std::env::var("PATH").ok();
        let prior_data_home = std::env::var("XDG_DATA_HOME").ok();

        let shim_dir = TempDir::new().unwrap();
        let gh_path = shim_dir.path().join("gh");
        std::fs::write(&gh_path, GH_SHIM_SCRIPT).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&gh_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&gh_path, perms).unwrap();
        }
        let new_path = format!(
            "{}:{}",
            shim_dir.path().display(),
            prior_path.clone().unwrap_or_default()
        );
        unsafe { std::env::set_var("PATH", &new_path) };

        let data_home = TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

        let change_id = "GX-sync-shim";
        let manager = StateManager::new().unwrap();
        let mut state = ChangeState::new(change_id.to_string(), None);
        state.add_repository("gx-testing/repo".to_string(), change_id.to_string());
        state.set_pr_info(
            "gx-testing/repo",
            42,
            "https://github.com/gx-testing/repo/pull/42".to_string(),
            false,
        );
        manager.save(&state).unwrap();

        let config = Config::default();
        let prs = github::list_prs_by_change_id("gx-testing", change_id, &config)
            .expect("shimmed gh call should succeed");
        assert_eq!(prs.len(), 1, "shim must return exactly one PR");
        assert_eq!(prs[0].state, github::PrState::Merged);

        let (merged, closed, status) =
            sync_change_state(&prs, change_id).expect("sync_change_state should succeed");
        assert_eq!(merged, 1);
        assert_eq!(closed, 0);
        assert_eq!(status, ChangeStatus::FullyMerged);

        let synced = manager
            .load(change_id)
            .unwrap()
            .expect("state must still exist");
        assert_eq!(
            synced.repositories.get("gx-testing/repo").unwrap().status,
            RepoChangeStatus::PrMerged
        );

        match prior_path {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        match prior_data_home {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        drop(guard);
    }
}
