//! Status subcommand implementation
//!
//! Shows git status across multiple repositories with unified output formatting.

use crate::cli::Cli;
use crate::config::{Config, OutputVerbosity};
use crate::output::StatusOptions;
use crate::utils::{get_jobs_from_config, get_max_depth_from_config, get_nproc};
use crate::{git, output, repo};
use eyre::{Context, Result};
use log::{debug, info};
use rayon::prelude::*;
use std::env;
use std::sync::Mutex;

/// Status command options
pub struct StatusCommandOptions<'a> {
    pub detailed: bool,
    pub use_emoji: bool,
    pub use_colors: bool,
    pub patterns: &'a [String],
    pub fetch_first: bool,
    pub no_remote: bool,
}

/// Process the status subcommand
pub fn process_status_command(
    cli: &Cli,
    config: &Config,
    options: StatusCommandOptions,
) -> Result<()> {
    info!(
        "Processing status command with {} patterns (fetch_first: {}, no_remote: {})",
        options.patterns.len(),
        options.fetch_first,
        options.no_remote
    );

    // Apply config defaults if CLI flags not provided
    let effective_fetch_first = options.fetch_first
        || config
            .remote_status
            .as_ref()
            .and_then(|rs| rs.fetch_first)
            .unwrap_or(false);

    let effective_no_remote = options.no_remote
        || !config
            .remote_status
            .as_ref()
            .and_then(|rs| rs.enabled)
            .unwrap_or(true);

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
        .unwrap_or(2);

    debug!("Using max depth: {max_depth}");

    // 1. Discover repositories
    let start_dir = env::current_dir().context("Failed to get current directory")?;
    let repos =
        repo::discover_repos(&start_dir, max_depth).context("Failed to discover repositories")?;

    info!("Discovered {} repositories", repos.len());

    // 2. Filter repositories
    let filtered_repos = repo::filter_repos(repos, options.patterns);
    info!("Filtered to {} repositories", filtered_repos.len());

    if filtered_repos.is_empty() {
        println!("🔍 No repositories found matching the criteria");
        return Ok(());
    }

    // 3. Use the fast calculation that now properly handles all possible emoji patterns
    let widths = output::calculate_alignment_widths_fast(&filtered_repos);

    // 4. Create status options
    let verbosity = if options.detailed {
        // CLI --detailed flag overrides config
        OutputVerbosity::Detailed
    } else {
        // Use config verbosity or default
        config
            .output
            .as_ref()
            .and_then(|o| o.verbosity)
            .unwrap_or_default()
    };

    let status_opts = StatusOptions {
        verbosity,
        use_emoji: options.use_emoji,
        use_colors: options.use_colors,
    };

    // 5. Process repositories in parallel with streaming output
    let results = Mutex::new(Vec::new());

    filtered_repos.par_iter().for_each(|repo| {
        let result =
            git::get_repo_status_with_options(repo, effective_fetch_first, effective_no_remote);

        // Store for final summary
        if let Ok(mut results_vec) = results.lock() {
            results_vec.push(result.clone());
        }

        // Display immediately with pre-calculated alignment
        if let Err(e) = output::display_status_result_immediate(&result, &status_opts, &widths) {
            log::error!("Failed to display status result: {e}");
        }
    });

    // 6. Final summary
    let results_vec = results.into_inner().unwrap_or_default();
    let (clean_count, dirty_count, error_count) = categorize_status_results(&results_vec);
    output::display_unified_summary(clean_count, dirty_count, error_count, &status_opts);

    // 7. Exit with error count
    if error_count > 0 {
        std::process::exit(error_count.min(255) as i32);
    }

    Ok(())
}

/// Categorize status results into clean/dirty/error counts
fn categorize_status_results(results: &[git::RepoStatus]) -> (usize, usize, usize) {
    let mut clean_count = 0;
    let mut dirty_count = 0;
    let mut error_count = 0;

    for result in results {
        if result.error.is_some() {
            error_count += 1;
        } else if result.is_clean {
            clean_count += 1;
        } else {
            dirty_count += 1;
        }
    }

    (clean_count, dirty_count, error_count)
}
