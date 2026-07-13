use crate::cli::Cli;
use crate::config::Config;
use crate::confirm::{confirm_destructive, DestructiveOp};
use crate::git;
use crate::github::{self, PrInfo};
use crate::output::{display_review_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use crate::ssh::SshUrlBuilder;
use crate::state::StateManager;
use crate::user_org::UserOrgContext;
use eyre::{Context, Result};
use log::{debug, info, trace, warn};
use rayon::prelude::*;
use std::path::Path;

/// Preflight-complete-or-abort PR discovery for a finish-line batch (design doc
/// `2026-07-12-gx-production-hardening.md`, Phase 3). Resolves PR discovery for
/// EVERY targeted org up front; if ANY org's `list_prs_by_change_id` errors,
/// the whole batch aborts with a loud `Err` naming that org - it NEVER
/// warn-and-continues over a partial set, which would let a token/network blip
/// on one org yield a partial merge/delete reported as success. Because the
/// caller binds this with `?` before the parallel mutation section, an aborted
/// discovery guarantees ZERO GitHub writes on the other orgs.
fn discover_all_prs(
    user_org_contexts: &[UserOrgContext],
    change_id: &str,
    config: &Config,
) -> Result<Vec<PrInfo>> {
    debug!(
        "discover_all_prs: change_id={change_id} orgs={}",
        user_org_contexts.len()
    );
    let mut all_prs = Vec::new();
    for context in user_org_contexts {
        let prs = github::list_prs_by_change_id(&context.user_or_org, change_id, config)
            .with_context(|| {
                format!(
                    "Aborting batch for {change_id}: PR discovery failed for org '{}' -- \
                     refusing to run a partial batch (no partial merge/delete)",
                    context.user_or_org
                )
            })?;
        all_prs.extend(prs);
    }
    debug!(
        "discover_all_prs: change_id={change_id} total_prs={}",
        all_prs.len()
    );
    Ok(all_prs)
}

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
#[allow(clippy::too_many_arguments)]
pub fn process_review_approve_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,
    _patterns: &[String],
    change_id: &str,
    admin_override: bool,
    auto_merge: bool,
    yes: bool,
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

    // Preflight-complete-or-abort (Phase 3): resolve discovery for EVERY org
    // BEFORE any mutation; any org error aborts the whole batch loudly (no
    // warn-and-continue over a partial set).
    let prs = discover_all_prs(&user_org_contexts, change_id, config)?;

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

    // Confirm gate (Phase 3): prompt only once the count reaches the threshold
    // (mirrors `create`'s confirm-threshold), fail closed on non-interactive
    // stdin without `--yes`. Runs AFTER discovery is proven complete, so the
    // count shown is the true blast radius.
    let threshold = config.review_confirm_threshold();
    if open_prs.len() >= threshold
        && !confirm_destructive(DestructiveOp::ReviewApprove, open_prs.len(), yes)?
    {
        println!("Aborted; no PRs approved or merged.");
        return Ok(());
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

    // Single race-free state update: load once, apply all outcomes, save once
    // ([A10]), under the change-level lock (Phase 7 [F6]) so a concurrent
    // `review sync`/`cleanup`/`undo` on the same change-id can't interleave.
    match crate::lock::ChangeLock::acquire(change_id) {
        Ok(_change_lock) => {
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
        }
        Err(e) => warn!("Failed to acquire change lock for {change_id}: {e}"),
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
    yes: bool,
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

    // Preflight-complete-or-abort (Phase 3): resolve discovery for EVERY org
    // BEFORE any mutation; any org error aborts the whole batch loudly (no
    // warn-and-continue over a partial set).
    let prs = discover_all_prs(&user_org_contexts, change_id, config)?;

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

    // Confirm gate (Phase 3): `review delete` CLOSES open (unmerged) PRs and
    // deletes their branches - the prompt states that destruction truthfully.
    // Prompt only once the count reaches the threshold; fail closed on
    // non-interactive stdin without `--yes`. Runs AFTER discovery is complete.
    let threshold = config.review_confirm_threshold();
    if open_prs.len() >= threshold
        && !confirm_destructive(DestructiveOp::ReviewDelete, open_prs.len(), yes)?
    {
        println!("Aborted; no PRs deleted.");
        return Ok(());
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

    // Single race-free state update: load once, mark closed, save once ([A10]),
    // under the change-level lock (Phase 7 [F6]).
    match crate::lock::ChangeLock::acquire(change_id) {
        Ok(_change_lock) => {
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
        }
        Err(e) => warn!("Failed to acquire change lock for {change_id}: {e}"),
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
/// org auto-detection. `pub(crate)` so `gx undo` reuses the exact same
/// reconciliation before building its plan (Phase 5 [F4]).
pub(crate) fn sync_change_state(
    prs: &[PrInfo],
    change_id: &str,
) -> Result<(usize, usize, crate::state::ChangeStatus)> {
    debug!("sync_change_state: change_id={change_id} prs={}", prs.len());
    // Change-level lock (Phase 7 [F6]): held for the whole load-mutate-save
    // cycle so a concurrent `undo`/`cleanup`/`approve`/`delete` on the same
    // change-id can never interleave and lose an update.
    let _change_lock = crate::lock::ChangeLock::acquire(change_id)
        .with_context(|| format!("Failed to acquire change lock for {change_id}"))?;
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

    // Per-repo lock (Phase 7 [F6]): a second concurrent gx invocation must not
    // interleave a clone/pull with any other mutating command on this repo.
    let _lock = match crate::lock::RepoLock::acquire(&repo_dir) {
        Ok(lock) => lock,
        Err(e) => {
            return ReviewResult {
                repo,
                change_id: change_id.to_string(),
                pr_number: Some(pr.number),
                action: ReviewAction::Cloned,
                error: Some(format!("Repository is locked: {e}")),
            };
        }
    };

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
        let guard = crate::test_utils::env_lock();
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

    #[test]
    fn test_sync_change_state_fails_fast_under_concurrent_change_lock() {
        // Phase 7 [F6] success criterion: concurrent `review sync` + `undo` on
        // one change-id lose no updates. `undo`'s final load-mutate-save holds
        // the SAME `ChangeLock` `sync_change_state` acquires; simulate "undo
        // mid-save" by holding the lock directly and confirm `sync_change_state`
        // fails fast -- it never reads-mutates-saves while someone else holds
        // the lock, so there is no window where one save can race and clobber
        // the other's update.
        let guard = crate::test_utils::env_lock();
        let prior_data_home = std::env::var("XDG_DATA_HOME").ok();

        let data_home = TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

        let change_id = "GX-lock-contend";
        let manager = StateManager::new().unwrap();
        let mut state = ChangeState::new(change_id.to_string(), None);
        state.add_repository("org/repo".to_string(), change_id.to_string());
        manager.save(&state).unwrap();

        // Simulate "undo" mid-save: hold the change-level lock directly.
        let held = crate::lock::ChangeLock::acquire(change_id).unwrap();

        let result = sync_change_state(&[], change_id);
        assert!(
            result.is_err(),
            "sync_change_state must fail fast while the change lock is held, not race the save"
        );

        drop(held);

        // Once released, the state on disk is exactly what it was before the
        // failed attempt -- nothing was torn or partially applied.
        let reloaded = manager.load(change_id).unwrap().expect("state intact");
        assert_eq!(
            reloaded.repositories.len(),
            1,
            "state must be unchanged by the failed sync attempt"
        );

        // And a sync AFTER release proceeds normally (the lock isn't stuck).
        assert!(sync_change_state(&[], change_id).is_ok());

        match prior_data_home {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        drop(guard);
    }

    /// A gh shim for the preflight test: succeeds (one open PR) for any org
    /// EXCEPT `badorg`, for which it exits non-zero to simulate a token/network
    /// blip. Any non-`api graphql` (i.e. mutating) invocation is also an error.
    const GH_PREFLIGHT_SHIM: &str = r#"#!/bin/sh
if [ "$1" != "api" ] || [ "$2" != "graphql" ]; then
  echo "gh preflight shim: unexpected mutating invocation: $@" >&2
  exit 1
fi
for arg in "$@"; do
  case "$arg" in
    *org:badorg*)
      echo "gh preflight shim: simulated discovery failure for badorg" >&2
      exit 1 ;;
  esac
done
cat <<'JSON'
{"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{
  "number": 7,
  "title": "GX-preflight: change",
  "headRefName": "GX-preflight",
  "author": {"login": "tester"},
  "state": "OPEN",
  "url": "https://github.com/goodorg/repo/pull/7",
  "repository": {"nameWithOwner": "goodorg/repo"},
  "mergedAt": null,
  "mergeCommit": null,
  "baseRefName": "main"
}]}}}
JSON
exit 0
"#;

    fn install_shim(dir: &std::path::Path, script: &str) {
        let gh_path = dir.join("gh");
        std::fs::write(&gh_path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&gh_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&gh_path, perms).unwrap();
    }

    /// Preflight-complete-or-abort bite (Phase 3): if ANY targeted org's PR
    /// discovery errors, `discover_all_prs` aborts the WHOLE batch with a loud
    /// `Err` naming the failed org - it never warn-and-continues over a partial
    /// set. The caller binds this with `?` before the mutation section, so an
    /// aborted discovery is a structural guarantee of ZERO GitHub writes.
    /// Reverting to warn-and-continue would make this return `Ok` and the test
    /// fails.
    #[test]
    fn test_discover_all_prs_aborts_whole_batch_when_one_org_errors() {
        let guard = crate::test_utils::env_lock();
        let prior_path = std::env::var("PATH").ok();
        let prior_tok = std::env::var("GITHUB_PAT_HOME").ok();

        let shim_dir = TempDir::new().unwrap();
        install_shim(shim_dir.path(), GH_PREFLIGHT_SHIM);
        let new_path = format!(
            "{}:{}",
            shim_dir.path().display(),
            prior_path.clone().unwrap_or_default()
        );
        unsafe { std::env::set_var("PATH", &new_path) };
        unsafe { std::env::set_var("GITHUB_PAT_HOME", "dummy-token-not-a-secret") };

        let contexts = vec![
            UserOrgContext {
                user_or_org: "goodorg".to_string(),
                detection_method: crate::user_org::DetectionMethod::Explicit,
            },
            UserOrgContext {
                user_or_org: "badorg".to_string(),
                detection_method: crate::user_org::DetectionMethod::Explicit,
            },
        ];
        let config = Config::default();
        let err = discover_all_prs(&contexts, "GX-preflight", &config)
            .expect_err("a failing org must abort the whole batch, not warn-and-continue");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("badorg"),
            "abort error must name the failed org: {msg}"
        );

        match prior_path {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        match prior_tok {
            Some(v) => unsafe { std::env::set_var("GITHUB_PAT_HOME", v) },
            None => unsafe { std::env::remove_var("GITHUB_PAT_HOME") },
        }
        drop(guard);
    }

    /// A gh spy shim for the command-level approve test: returns ONE open PR on
    /// the discovery (`api graphql`) path; ANY other (mutating) invocation
    /// appends to `$GX_TEST_MUTATION_LOG` so the test can assert ZERO mutations.
    const GH_APPROVE_SPY_SHIM: &str = r#"#!/bin/sh
if [ "$1" = "api" ] && [ "$2" = "graphql" ]; then
cat <<'JSON'
{"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{
  "number": 11,
  "title": "GX-approve-shim: change",
  "headRefName": "GX-approve-shim",
  "author": {"login": "tester"},
  "state": "OPEN",
  "url": "https://github.com/gx-testing/repo/pull/11",
  "repository": {"nameWithOwner": "gx-testing/repo"},
  "mergedAt": null,
  "mergeCommit": null,
  "baseRefName": "main"
}]}}}
JSON
  exit 0
fi
echo "MUTATION: $@" >> "$GX_TEST_MUTATION_LOG"
exit 0
"#;

    /// Command-level bite (Phase 3): the confirm gate is actually WIRED into
    /// `review approve`. With the open-PR count at/above the threshold and
    /// non-interactive stdin (as under `cargo test`) without `--yes`, the
    /// command FAILS CLOSED (loud error naming `--yes`) and performs ZERO
    /// GitHub mutations - the spy shim records any non-discovery invocation and
    /// the test asserts none happened. Remove the gate and this test fails
    /// (either the command returns `Ok`, or a merge mutation is logged).
    #[test]
    fn test_review_approve_fails_closed_and_makes_zero_mutations() {
        use clap::Parser;
        let guard = crate::test_utils::env_lock();
        let prior_path = std::env::var("PATH").ok();
        let prior_tok = std::env::var("GITHUB_PAT_HOME").ok();
        let prior_mut = std::env::var("GX_TEST_MUTATION_LOG").ok();
        let prior_data_home = std::env::var("XDG_DATA_HOME").ok();

        let shim_dir = TempDir::new().unwrap();
        install_shim(shim_dir.path(), GH_APPROVE_SPY_SHIM);
        let new_path = format!(
            "{}:{}",
            shim_dir.path().display(),
            prior_path.clone().unwrap_or_default()
        );
        unsafe { std::env::set_var("PATH", &new_path) };
        unsafe { std::env::set_var("GITHUB_PAT_HOME", "dummy-token-not-a-secret") };
        let mut_log = shim_dir.path().join("mutations.log");
        unsafe { std::env::set_var("GX_TEST_MUTATION_LOG", &mut_log) };
        let data_home = TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

        let work = TempDir::new().unwrap();
        let cwd = work.path().to_string_lossy().to_string();
        let cli = Cli::parse_from(["gx", "--cwd", &cwd, "review", "approve", "GX-approve-shim"]);
        // Threshold 1 so a single open PR trips the gate deterministically.
        let config = Config {
            review: Some(crate::config::ReviewConfig {
                confirm_threshold: Some(1),
            }),
            ..Config::default()
        };

        let result = process_review_approve_command(
            &cli,
            &config,
            Some("gx-testing"),
            &[],
            "GX-approve-shim",
            false,
            false,
            false, // yes = false -> must fail closed
        );

        assert!(
            result.is_err(),
            "review approve must fail closed on non-interactive stdin without --yes"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("--yes"), "error must name --yes: {msg}");
        assert!(
            !mut_log.exists(),
            "ZERO mutations: no merge/close gh call may have run, but the spy logged one"
        );

        match prior_path {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        match prior_tok {
            Some(v) => unsafe { std::env::set_var("GITHUB_PAT_HOME", v) },
            None => unsafe { std::env::remove_var("GITHUB_PAT_HOME") },
        }
        match prior_mut {
            Some(v) => unsafe { std::env::set_var("GX_TEST_MUTATION_LOG", v) },
            None => unsafe { std::env::remove_var("GX_TEST_MUTATION_LOG") },
        }
        match prior_data_home {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        drop(guard);
    }
}
