//! Status subcommand implementation
//!
//! Shows git status across multiple repositories with unified output formatting.

use crate::cli::Cli;
use crate::config::{Config, OutputVerbosity};
use crate::{git, output, repo};
use crate::output::StatusOptions;
use crate::utils::{get_jobs_from_config, get_max_depth_from_config, get_nproc};
use eyre::{Context, Result};
use log::{debug, info};
use rayon::prelude::*;
use std::env;

/// Process the status subcommand
pub fn process_status_command(
    cli: &Cli,
    config: &Config,
    detailed: bool,
    use_emoji: bool,
    use_colors: bool,
    patterns: &[String],
) -> Result<()> {
    info!("Processing status command with {} patterns", patterns.len());

    // Determine jobs
    let jobs = cli.parallel
        .or_else(|| get_jobs_from_config(config))
        .unwrap_or_else(|| get_nproc().unwrap_or(4));

    debug!("Using jobs: {}", jobs);

    // Set rayon thread pool size
    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build_global()
        .context("Failed to initialize thread pool")?;

    // Determine max depth
    let max_depth = cli.max_depth
        .or_else(|| get_max_depth_from_config(config))
        .unwrap_or(2);

    debug!("Using max depth: {}", max_depth);

    // 1. Discover repositories
    let start_dir = env::current_dir().context("Failed to get current directory")?;
    let repos = repo::discover_repos(&start_dir, max_depth)
        .context("Failed to discover repositories")?;

    info!("Discovered {} repositories", repos.len());

    // 2. Filter repositories
    let filtered_repos = repo::filter_repos(repos, patterns);
    info!("Filtered to {} repositories", filtered_repos.len());

    if filtered_repos.is_empty() {
        println!("🔍 No repositories found matching the criteria");
        return Ok(());
    }

    // 3. Process repositories in parallel
    let results: Vec<git::RepoStatus> = filtered_repos
        .par_iter()
        .map(|repo| git::get_repo_status(repo))
        .collect();

    // 4. Display results using output.rs
    let verbosity = if detailed {
        // CLI --detailed flag overrides config
        OutputVerbosity::Detailed
    } else {
        // Use config verbosity or default
        config.output
            .as_ref()
            .and_then(|o| o.verbosity)
            .unwrap_or_default()
    };

    let status_opts = StatusOptions {
        verbosity,
        use_emoji,
        use_colors,
    };

    output::display_status_results(results.clone(), &status_opts);

    // 5. Exit with error count
    let error_count = results.iter().filter(|r| r.error.is_some()).count();
    if error_count > 0 {
        std::process::exit(error_count.min(255) as i32);
    }

    Ok(())
}
