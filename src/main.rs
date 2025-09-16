use clap::Parser;
use eyre::{Context, Result};
use log::info;
use std::env;
use std::fs;
use std::path::PathBuf;

mod checkout;
mod cli;
mod clone;
mod config;
mod create;
mod diff;
mod file;
mod git;
mod github;
mod output;
mod repo;
mod review;
mod ssh;
mod status;
mod transaction;
mod user_org;
mod utils;

#[cfg(test)]
pub mod test_utils;

use cli::{Cli, Commands};
use config::Config;

fn setup_logging() -> Result<()> {
    // During tests, use a temp directory to avoid polluting production logs
    let log_file = if cfg!(test) {
        // Create a temp file for test logging
        let temp_dir = std::env::temp_dir();
        temp_dir.join(format!("gx-test-{}.log", std::process::id()))
    } else {
        // Production logging location
        let log_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gx")
            .join("logs");

        fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

        log_dir.join("gx.log")
    };

    // Setup env_logger with file output
    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .context("Failed to open log file")?,
    );

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
        } => status::process_status_command(cli, config, *detailed, !no_emoji, !no_color, patterns),
        Commands::Checkout {
            create_branch,
            from_branch,
            branch_name,
            stash,
            patterns,
        } => checkout::process_checkout_command(
            cli,
            config,
            *create_branch,
            from_branch.as_deref(),
            branch_name,
            *stash,
            patterns,
        ),
        Commands::Clone {
            user_or_org,
            include_archived,
            patterns,
        } => clone::process_clone_command(cli, config, user_or_org, *include_archived, patterns),
        Commands::Create {
            files,
            change_id,
            patterns,
            commit,
            pr,
            action,
        } => match action {
            None => create::show_matches(cli, config, files, patterns),
            Some(action) => {
                let change = match action {
                    cli::CreateAction::Add { path, content } => create::Change::Add(path.clone(), content.clone()),
                    cli::CreateAction::Delete => create::Change::Delete,
                    cli::CreateAction::Sub { pattern, replacement } => {
                        create::Change::Sub(pattern.clone(), replacement.clone())
                    }
                    cli::CreateAction::Regex { pattern, replacement } => {
                        create::Change::Regex(pattern.clone(), replacement.clone())
                    }
                };
                create::process_create_command(
                    cli,
                    config,
                    files,
                    change_id.clone(),
                    patterns,
                    commit.clone(),
                    *pr,
                    change,
                )
            }
        },
        Commands::Review { org, patterns, action } => match action {
            cli::ReviewAction::Ls { change_ids } => {
                review::process_review_ls_command(cli, config, org.as_deref(), patterns, change_ids)
            }
            cli::ReviewAction::Clone { change_id, all } => {
                review::process_review_clone_command(cli, config, org.as_deref(), patterns, change_id, *all)
            }
            cli::ReviewAction::Approve { change_id, admin } => {
                review::process_review_approve_command(cli, config, org.as_deref(), patterns, change_id, *admin)
            }
            cli::ReviewAction::Delete { change_id } => {
                review::process_review_delete_command(cli, config, org.as_deref(), patterns, change_id)
            }
            cli::ReviewAction::Purge => review::process_review_purge_command(cli, config, org.as_deref(), patterns),
        },
    }
}

fn main() -> Result<()> {
    // Setup logging first
    setup_logging().context("Failed to setup logging")?;

    // Parse CLI arguments
    let cli = Cli::parse();

    // ONLY change directory if user explicitly provided --cwd
    if let Some(cwd) = &cli.cwd {
        env::set_current_dir(cwd).context(format!("Failed to change to directory: {}", cwd.display()))?;
        info!("Changed working directory to: {}", cwd.display());
    }

    // Load configuration
    let config = Config::load(cli.config.as_ref()).context("Failed to load configuration")?;

    info!("Starting with config from: {:?}", cli.config);

    // Run the main application logic
    run_application(&cli, &config).context("Application failed")?;

    Ok(())
}
