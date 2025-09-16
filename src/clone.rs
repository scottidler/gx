//! Clone subcommand implementation
//!
//! Clone repositories from GitHub user/org with streaming output.

use crate::cli::Cli;
use crate::config::Config;
use crate::output::StatusOptions;
use crate::utils::{get_jobs_from_config, get_nproc};
use crate::{git, github, output, repo};
use eyre::{Context, Result};
use log::{debug, info};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Mutex;

/// Process the clone subcommand
pub fn process_clone_command(
    cli: &Cli,
    config: &Config,
    user_or_org: &str,
    include_archived: bool,
    patterns: &[String],
) -> Result<()> {
    info!(
        "Processing clone command for user/org '{}' with {} patterns",
        user_or_org,
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

    // 1. Get repositories from GitHub
    let all_repos = github::get_user_repos(user_or_org, include_archived, config)
        .context("Failed to get repositories from GitHub")?;

    info!("Found {} repositories for {}", all_repos.len(), user_or_org);

    if all_repos.is_empty() {
        println!("🔍 No repositories found for {user_or_org}");
        return Ok(());
    }

    // 2. Filter repositories using existing repo filtering logic
    let filtered_slugs = filter_repository_slugs(&all_repos, patterns);

    info!("Filtered to {} repositories", filtered_slugs.len());

    if filtered_slugs.is_empty() {
        println!("🔍 No repositories found matching the patterns");
        return Ok(());
    }

    // 3. Read GitHub token
    let token = github::read_token(user_or_org, config).context("Failed to read GitHub token")?;

    // 4. Process repositories in parallel with streaming output
    let results = Mutex::new(Vec::new());

    filtered_slugs.par_iter().for_each(|repo_slug| {
        let result = git::clone_or_update_repo(repo_slug, user_or_org, &token);

        // Store result and display immediately
        if let Ok(mut results_vec) = results.lock() {
            results_vec.push(result.clone());
        }
        if let Err(e) = output::display_clone_result_immediate(&result) {
            log::error!("Failed to display clone result: {e}");
        }
    });

    // 5. Categorize results and show unified summary
    let results_vec = results.into_inner().unwrap_or_default();
    let (clean_count, dirty_count, error_count) = categorize_clone_results(&results_vec);

    let status_opts = StatusOptions::default();
    output::display_unified_summary(clean_count, dirty_count, error_count, &status_opts);

    // 6. Exit with error count
    if error_count > 0 {
        std::process::exit(error_count.min(255) as i32);
    }

    Ok(())
}

/// Filter repository slugs using the existing repo filtering logic
fn filter_repository_slugs(all_repos: &[String], patterns: &[String]) -> Vec<String> {
    // Convert repo slugs to fake Repo objects for filtering
    let fake_repos: Vec<repo::Repo> = all_repos
        .iter()
        .map(|slug| {
            let parts: Vec<&str> = slug.split('/').collect();
            let name = if parts.len() == 2 { parts[1] } else { slug };
            repo::Repo {
                path: PathBuf::from(name), // Not used for filtering
                name: name.to_string(),
                slug: Some(slug.clone()),
            }
        })
        .collect();

    let filtered_repos = repo::filter_repos(fake_repos, patterns);
    filtered_repos.iter().filter_map(|r| r.slug.clone()).collect()
}

/// Categorize clone results into clean/dirty/error counts
fn categorize_clone_results(results: &[git::CloneResult]) -> (usize, usize, usize) {
    let mut clean_count = 0;
    let mut dirty_count = 0;
    let mut error_count = 0;

    for result in results {
        if result.error.is_some() {
            error_count += 1;
        } else {
            match result.action {
                git::CloneAction::Cloned => clean_count += 1,
                git::CloneAction::Updated => clean_count += 1,
                git::CloneAction::Stashed => dirty_count += 1, // Had uncommitted changes during update
                git::CloneAction::DirectoryNotGitRepo => error_count += 1, // Directory exists but not git
                git::CloneAction::DifferentRemote => dirty_count += 1, // Different remote URL detected
            }
        }
    }

    (clean_count, dirty_count, error_count)
}
