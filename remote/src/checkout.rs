//! Checkout subcommand implementation
//!
//! Checkout branches across multiple repositories with streaming output.

use crate::cli::Cli;
use crate::output::StatusOptions;
use crate::{git, output};
use eyre::{Context, Result};
use local::config::Config;
use local::repo;
use local::utils::{get_jobs_from_config, get_max_depth_from_config, get_nproc};
use log::{debug, info};
use rayon::prelude::*;
use std::env;
use std::sync::Mutex;

/// Process the checkout subcommand
pub fn process_checkout_command(
    cli: &Cli,
    config: &Config,
    create_branch: bool,
    from_branch: Option<&str>,
    branch_name: &str,
    stash: bool,
    patterns: &[String],
) -> Result<()> {
    info!(
        "Processing checkout command for branch '{}' with {} patterns",
        branch_name,
        patterns.len()
    );

    // Determine jobs
    let jobs = cli
        .parallel
        .or_else(|| get_jobs_from_config(config))
        .unwrap_or_else(|| get_nproc().unwrap_or(4));

    debug!("Using jobs: {jobs}");

    // Set rayon thread pool size
    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build_global()
        .context("Failed to initialize thread pool")?;

    // Determine max depth
    let max_depth = cli
        .max_depth
        .or_else(|| get_max_depth_from_config(config))
        .unwrap_or(3);

    debug!("Using max depth: {max_depth}");

    // 1. Discover repositories
    let start_dir = env::current_dir().context("Failed to get current directory")?;
    let repos = repo::discover_repos(&start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    info!("Discovered {} repositories", repos.len());

    // 2. Filter repositories
    let filtered_repos = repo::filter_repos(repos, patterns);
    info!("Filtered to {} repositories", filtered_repos.len());

    if filtered_repos.is_empty() {
        println!("🔍 No repositories found matching the criteria");
        return Ok(());
    }

    // 3. Process repositories in parallel with streaming output
    let results = Mutex::new(Vec::new());

    filtered_repos.par_iter().for_each(|repo| {
        // Resolve branch name per repo (handle 'default' keyword)
        let resolved_branch = match local::git::resolve_branch_name(repo, branch_name) {
            Ok(branch) => branch,
            Err(e) => {
                // Handle resolution error
                let result = git::CheckoutResult {
                    repo: repo.clone(),
                    branch_name: branch_name.to_string(),
                    commit_sha: None,
                    action: git::CheckoutAction::CheckedOutSynced,
                    error: Some(format!("Failed to resolve branch name: {e}")),
                };

                // Store result and display immediately. Poison-recovery
                // belt-and-suspenders (the panic hook in `main` is the
                // primary fix): recover partial results rather than blank.
                results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(result.clone());
                if let Err(e) = output::display_checkout_result_immediate(&result) {
                    log::error!("Failed to display checkout result: {e}");
                }
                return;
            }
        };

        // Resolve from_branch if provided and it's 'default'
        let resolved_from_branch = if let Some(from) = from_branch {
            match local::git::resolve_branch_name(repo, from) {
                Ok(branch) => Some(branch),
                Err(e) => {
                    // Handle from_branch resolution error
                    let result = git::CheckoutResult {
                        repo: repo.clone(),
                        branch_name: branch_name.to_string(),
                        commit_sha: None,
                        action: git::CheckoutAction::CheckedOutSynced,
                        error: Some(format!("Failed to resolve from branch '{from}': {e}")),
                    };

                    // Store result and display immediately. Poison-recovery
                    // belt-and-suspenders (the panic hook in `main` is the
                    // primary fix).
                    results
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(result.clone());
                    if let Err(e) = output::display_checkout_result_immediate(&result) {
                        log::error!("Failed to display checkout result: {e}");
                    }
                    return;
                }
            }
        } else {
            None
        };

        let result = git::checkout_branch(
            repo,
            &resolved_branch,
            create_branch,
            resolved_from_branch.as_deref(),
            stash,
        );

        // Store result and display immediately. Poison-recovery
        // belt-and-suspenders (the panic hook in `main` is the primary fix).
        results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(result.clone());
        if let Err(e) = output::display_checkout_result_immediate(&result) {
            log::error!("Failed to display checkout result: {e}");
        }
    });

    // 4. Categorize results and show unified summary
    let results_vec = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let (clean_count, dirty_count, error_count) = categorize_checkout_results(&results_vec);

    let status_opts = StatusOptions::default();
    output::display_unified_summary(clean_count, dirty_count, error_count, &status_opts);

    // 5. Exit with error count
    if error_count > 0 {
        std::process::exit(error_count.min(255) as i32);
    }

    Ok(())
}

/// Categorize checkout results into clean/dirty/error counts
fn categorize_checkout_results(results: &[git::CheckoutResult]) -> (usize, usize, usize) {
    let mut clean_count = 0;
    let mut dirty_count = 0;
    let mut error_count = 0;

    for result in results {
        if result.error.is_some() {
            error_count += 1;
        } else {
            match result.action {
                git::CheckoutAction::CheckedOutSynced => clean_count += 1,
                git::CheckoutAction::CreatedFromRemote => clean_count += 1,
                git::CheckoutAction::Stashed => dirty_count += 1, // Had uncommitted changes
                git::CheckoutAction::HasUntracked => dirty_count += 1, // Has untracked files
            }
        }
    }

    (clean_count, dirty_count, error_count)
}
