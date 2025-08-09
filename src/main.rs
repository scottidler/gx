use clap::Parser;
// use colored::*; // Not used in main.rs
use eyre::{Context, Result};
use log::{debug, info};
use rayon::prelude::*;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod cli;
mod config;
mod git;
mod github;
mod output;
mod repo;

#[cfg(test)]
pub mod test_utils;

use cli::{Cli, Commands};
use config::Config;
use output::StatusOptions;

fn setup_logging() -> Result<()> {
    // Create log directory
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gx")
        .join("logs");

    fs::create_dir_all(&log_dir)
        .context("Failed to create log directory")?;

    let log_file = log_dir.join("gx.log");

    // Setup env_logger with file output
    let target = Box::new(fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .context("Failed to open log file")?);

    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .init();

    info!("Logging initialized, writing to: {}", log_file.display());
    Ok(())
}

fn run_application(cli: &Cli, config: &Config) -> Result<()> {
    info!("Starting gx with command: {:?}", cli.command);

    match &cli.command {
        Commands::Status {
            detailed,
            no_emoji,
            no_color,
            patterns,
        } => {
            process_status_command(cli, config, *detailed, !no_emoji, !no_color, patterns)
        }
        Commands::Checkout {
            create_branch,
            from_branch,
            branch_name,
            stash,
            patterns,
        } => {
            process_checkout_command(cli, config, *create_branch, from_branch.as_deref(), branch_name, *stash, patterns)
        }
        Commands::Clone {
            user_or_org,
            include_archived,
            patterns,
        } => {
            process_clone_command(cli, config, user_or_org, *include_archived, patterns)
        }
    }
}

fn process_status_command(
    cli: &Cli,
    config: &Config,
    detailed: bool,
    use_emoji: bool,
    use_colors: bool,
    patterns: &[String],
) -> Result<()> {
    info!("Processing status command with {} patterns", patterns.len());

    // Determine parallelism
    let parallelism = cli.parallel
        .or_else(|| get_parallelism_from_config(config))
        .unwrap_or_else(|| get_nproc().unwrap_or(4));

    debug!("Using parallelism: {}", parallelism);

    // Set rayon thread pool size
    rayon::ThreadPoolBuilder::new()
        .num_threads(parallelism)
        .build_global()
        .context("Failed to initialize thread pool")?;

    // Determine max depth
    let max_depth = cli.max_depth
        .or_else(|| get_max_depth_from_config(config))
        .unwrap_or(10);

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

    // 4. Display results
    let status_opts = StatusOptions {
        show_all: true, // Always show all repositories
        detailed,
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

fn process_checkout_command(
    cli: &Cli,
    config: &Config,
    create_branch: bool,
    from_branch: Option<&str>,
    branch_name: &str,
    stash: bool,
    patterns: &[String],
) -> Result<()> {
    info!("Processing checkout command for branch '{}' with {} patterns", branch_name, patterns.len());

    // Determine parallelism
    let parallelism = cli.parallel
        .or_else(|| get_parallelism_from_config(config))
        .unwrap_or_else(|| get_nproc().unwrap_or(4));

    debug!("Using parallelism: {}", parallelism);

    // Set rayon thread pool size
    rayon::ThreadPoolBuilder::new()
        .num_threads(parallelism)
        .build_global()
        .context("Failed to initialize thread pool")?;

    // Determine max depth
    let max_depth = cli.max_depth
        .or_else(|| get_max_depth_from_config(config))
        .unwrap_or(10);

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
    let results: Vec<git::CheckoutResult> = filtered_repos
        .par_iter()
        .map(|repo| git::checkout_branch(repo, branch_name, create_branch, from_branch, stash))
        .collect();

    // 4. Display results
    output::display_checkout_results(results.clone());

    // 5. Exit with error count
    let error_count = results.iter().filter(|r| r.error.is_some()).count();
    if error_count > 0 {
        std::process::exit(error_count.min(255) as i32);
    }

    Ok(())
}

fn process_clone_command(
    cli: &Cli,
    config: &Config,
    user_or_org: &str,
    include_archived: bool,
    patterns: &[String],
) -> Result<()> {
    info!("Processing clone command for user/org '{}' with {} patterns", user_or_org, patterns.len());

    // Determine parallelism
    let parallelism = cli.parallel
        .or_else(|| get_parallelism_from_config(config))
        .unwrap_or_else(|| get_nproc().unwrap_or(4));

    debug!("Using parallelism: {}", parallelism);

    // Set rayon thread pool size
    rayon::ThreadPoolBuilder::new()
        .num_threads(parallelism)
        .build_global()
        .context("Failed to initialize thread pool")?;

    // 1. Get repositories from GitHub
    let all_repos = github::get_user_repos(user_or_org, include_archived)
        .context("Failed to get repositories from GitHub")?;

    info!("Found {} repositories for {}", all_repos.len(), user_or_org);

    if all_repos.is_empty() {
        println!("🔍 No repositories found for {}", user_or_org);
        return Ok(());
    }

    // 2. Filter repositories using existing repo filtering logic
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
    let filtered_slugs: Vec<String> = filtered_repos
        .iter()
        .filter_map(|r| r.slug.clone())
        .collect();

    info!("Filtered to {} repositories", filtered_slugs.len());

    if filtered_slugs.is_empty() {
        println!("🔍 No repositories found matching the patterns");
        return Ok(());
    }

    // 3. Read GitHub token
    let token = github::read_token(user_or_org)
        .context("Failed to read GitHub token")?;

    // 4. Process repositories in parallel
    let results: Vec<git::CloneResult> = filtered_slugs
        .par_iter()
        .map(|repo_slug| git::clone_or_update_repo(repo_slug, user_or_org, &token))
        .collect();

    // 5. Display results
    output::display_clone_results(results.clone(), false); // TODO: Add detailed flag

    // 6. Exit with error count
    let error_count = results.iter().filter(|r| r.error.is_some()).count();
    if error_count > 0 {
        std::process::exit(error_count.min(255) as i32);
    }

    Ok(())
}

/// Get parallelism from config, handling "nproc" string
fn get_parallelism_from_config(_config: &Config) -> Option<usize> {
    // This would need to be implemented once we have the config structure
    // For now, return None to use nproc
    None
}

/// Get max depth from config
fn get_max_depth_from_config(_config: &Config) -> Option<usize> {
    // This would need to be implemented once we have the config structure
    // For now, return None to use default
    None
}

/// Get number of processors by running nproc command
fn get_nproc() -> Option<usize> {
    let output = Command::new("nproc").output().ok()?;

    if output.status.success() {
        let nproc_str = String::from_utf8(output.stdout).ok()?;
        nproc_str.trim().parse().ok()
    } else {
        None
    }
}

fn main() -> Result<()> {
    // Setup logging first
    setup_logging()
        .context("Failed to setup logging")?;

    // Parse CLI arguments
    let cli = Cli::parse();

    // ONLY change directory if user explicitly provided --cwd
    if let Some(cwd) = &cli.cwd {
        env::set_current_dir(cwd)
            .context(format!("Failed to change to directory: {}", cwd.display()))?;
        info!("Changed working directory to: {}", cwd.display());
    }

    // Load configuration
    let config = Config::load(cli.config.as_ref())
        .context("Failed to load configuration")?;

    info!("Starting with config from: {:?}", cli.config);

    // Run the main application logic
    run_application(&cli, &config)
        .context("Application failed")?;

    Ok(())
}
