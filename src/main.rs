use clap::Parser;
use eyre::{Context, Result};
use log::info;
use std::env;
use std::fs;
use std::path::PathBuf;


mod cli;
mod config;
mod git;
mod github;
mod output;
mod repo;
mod status;
mod checkout;
mod clone;
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

        fs::create_dir_all(&log_dir)
            .context("Failed to create log directory")?;

        log_dir.join("gx.log")
    };

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
            status::process_status_command(cli, config, *detailed, !no_emoji, !no_color, patterns)
        }
        Commands::Checkout {
            create_branch,
            from_branch,
            branch_name,
            stash,
            patterns,
        } => {
            checkout::process_checkout_command(cli, config, *create_branch, from_branch.as_deref(), branch_name, *stash, patterns)
        }
        Commands::Clone {
            user_or_org,
            include_archived,
            patterns,
        } => {
            clone::process_clone_command(cli, config, user_or_org, *include_archived, patterns)
        }
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
