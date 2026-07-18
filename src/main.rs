use eyre::{Context, Result};
use log::info;
use std::env;
use std::fs;
use std::path::PathBuf;

mod bare;
mod checkout;
mod cleanup;
mod cli;
mod clone;
mod config;
mod confirm;
mod crash;
mod create;
mod diff;
mod doctor;
mod file;
mod git;
mod github;
mod hash;
mod lock;
mod output;
mod persona;
mod repo;
mod review;
mod rollback;
mod ssh;
mod state;
mod status;
mod subprocess;
mod transaction;
mod undo;
mod user_org;
mod utils;

#[cfg(test)]
pub mod test_utils;

use cli::{Cli, Commands};
use config::{xdg_data_dir, Config};

fn setup_logging(level: cli::LogLevel) -> Result<()> {
    // During tests, use a temp directory to avoid polluting production logs
    let log_file = if cfg!(test) {
        // Create a temp file for test logging
        let temp_dir = std::env::temp_dir();
        temp_dir.join(format!("gx-test-{}.log", std::process::id()))
    } else {
        // Production logging location
        let log_dir = xdg_data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gx")
            .join("logs");

        fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

        log_dir.join("gx.log")
    };

    // Setup env_logger with file output. The level comes from --log-level only;
    // RUST_LOG is no longer consulted ([A24]).
    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .context("Failed to open log file")?,
    );

    env_logger::Builder::new()
        .filter_level(level.to_filter())
        .target(env_logger::Target::Pipe(target))
        .init();

    info!("Logging initialized, writing to: {}", log_file.display());
    Ok(())
}

/// Install a panic hook that logs the panic (thread, location, message) at
/// ERROR before the process unwinds. rayon already re-raises a worker panic
/// out of `par_iter` (an uncaught panic still exits the process), so this
/// hook does not change that outcome - it guarantees a diagnostic line lands
/// in the log instead of the panic being a bare, undiagnosable abort. The
/// prior hook (Rust's default `thread '<name>' panicked at ...` to stderr) is
/// preserved by chaining through it, so nothing is lost, only added.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        default_hook(panic_info);

        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };

        log::error!("panic on thread '{thread_name}' at {location}: {message}");
    }));
}

fn run_application(cli: &Cli, config: &Config) -> Result<()> {
    info!("Starting gx with command: {:?}", cli.command);

    match &cli.command {
        Commands::Status {
            detailed,
            no_emoji,
            no_color,
            patterns,
            fetch_first,
            no_remote,
        } => {
            let options = status::StatusCommandOptions {
                detailed: *detailed,
                use_emoji: !no_emoji,
                use_colors: !no_color,
                patterns,
                fetch_first: *fetch_first,
                no_remote: *no_remote,
            };
            status::process_status_command(cli, config, options)
        }
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
            draft,
            yes,
            report,
            action,
        } => match action {
            None => create::show_matches(cli, config, files, patterns),
            Some(action) => {
                let propose_only =
                    matches!(action, cli::CreateAction::Llm { propose, .. } if *propose);
                let change = match action {
                    cli::CreateAction::Add { path, content } => {
                        create::Change::Add(path.clone(), content.clone())
                    }
                    cli::CreateAction::Delete => create::Change::Delete,
                    cli::CreateAction::Sub {
                        pattern,
                        replacement,
                    } => create::Change::Sub(pattern.clone(), replacement.clone()),
                    cli::CreateAction::Regex {
                        pattern,
                        replacement,
                    } => create::Change::Regex(pattern.clone(), replacement.clone()),
                    cli::CreateAction::Llm { prompt, .. } => create::Change::Llm(prompt.clone()),
                };
                create::process_create_command(
                    cli,
                    config,
                    files,
                    change_id.clone(),
                    patterns,
                    commit.clone(),
                    *pr,
                    *draft,
                    *yes,
                    change,
                    propose_only,
                    report.as_deref(),
                )
            }
        },
        Commands::Apply {
            change_id,
            pr,
            draft,
            yes,
        } => create::process_apply_command(cli, config, change_id, *pr, *draft, *yes),
        Commands::Review {
            org,
            patterns,
            action,
        } => match action {
            cli::ReviewAction::Ls { change_ids } => {
                review::process_review_ls_command(cli, config, org.as_deref(), patterns, change_ids)
            }
            cli::ReviewAction::Clone { change_id, all } => review::process_review_clone_command(
                cli,
                config,
                org.as_deref(),
                patterns,
                change_id,
                *all,
            ),
            cli::ReviewAction::Approve {
                change_id,
                admin,
                auto,
                yes,
            } => review::process_review_approve_command(
                cli,
                config,
                org.as_deref(),
                patterns,
                change_id,
                *admin,
                *auto,
                *yes,
            ),
            cli::ReviewAction::Delete { change_id, yes } => review::process_review_delete_command(
                cli,
                config,
                org.as_deref(),
                patterns,
                change_id,
                *yes,
            ),
            cli::ReviewAction::Sync { change_id } => review::process_review_sync_command(
                cli,
                config,
                org.as_deref(),
                patterns,
                change_id,
            ),
            cli::ReviewAction::Purge { yes } => {
                review::process_review_purge_command(cli, config, org.as_deref(), patterns, *yes)
            }
        },
        Commands::Rollback { action } => rollback::handle_rollback(action.clone()),
        Commands::Undo {
            change_id,
            org,
            yes,
        } => undo::process_undo_command(cli, config, change_id, org.as_deref(), *yes),
        Commands::Cleanup {
            change_id,
            all,
            list,
            include_remote,
            force,
            yes,
        } => cleanup::process_cleanup_command(
            cli,
            config,
            change_id.as_deref(),
            *all,
            *list,
            *include_remote,
            *force,
            *yes,
        ),
        Commands::Doctor { purge } => doctor::run_doctor(*purge),
        // Intercepted in `run()` before `run_application` is ever called, so it
        // never reaches this dispatch.
        Commands::Mcp(_) => unreachable!("mcp is handled in run() before run_application"),
    }
}

fn run() -> Result<()> {
    use clap::{CommandFactory, FromArgMatches};

    // Render the log path at runtime from the same XDG source the logger uses,
    // so --help never drifts and we don't spawn subprocesses during parsing ([A24]).
    let after_help = format!(
        "Logs are written to: {}\nRun `gx doctor` to check required tools.",
        doctor::log_path().display()
    );
    let matches = Cli::command().after_help(after_help).get_matches();
    let cli = Cli::from_arg_matches(&matches)?;

    // ONLY change directory if user explicitly provided --cwd. Done before any
    // path resolution (config load, the mcp handoff below, repo discovery).
    if let Some(cwd) = &cli.cwd {
        env::set_current_dir(cwd)
            .context(format!("Failed to change to directory: {}", cwd.display()))?;
    }

    // The `mcp` arm hands off to mcp-io, which owns its OWN file logging, tokio
    // runtime, and stdio discipline, and never returns. It MUST intercept here,
    // BEFORE gx's env_logger init (a second env_logger init would panic). It
    // uses the gx *library* Config/handler types: main.rs's own `config` module
    // is a separate compilation unit whose `Config` is a distinct type, so the
    // handler (which holds `gx::config::Config`) must be fed the library one.
    if let Commands::Mcp(cmd) = &cli.command {
        let config = gx::config::Config::load(cli.config.as_ref())
            .context("Failed to load configuration")?;
        let io = mcp_io::mcp_io!();
        std::process::exit(cmd.run(&io, || {
            Ok::<_, std::convert::Infallible>(gx::mcp::server::GxMcpServer::new(config))
        }));
    }

    // Set up logging from the parsed --log-level.
    setup_logging(cli.log_level).context("Failed to setup logging")?;

    // Install the panic hook now that logging is live, so a worker panic in
    // any parallel command (rayon `par_iter`, e.g. `create`/`status`/
    // `checkout`/`clone`) surfaces an ERROR log line rather than a bare abort.
    install_panic_hook();

    if let Some(cwd) = &cli.cwd {
        info!("Changed working directory to: {}", cwd.display());
    }

    // Load configuration
    let config = Config::load(cli.config.as_ref()).context("Failed to load configuration")?;

    // Install the configured git/gh subprocess timeout before any command spins
    // up a rayon pool: the deep git/gh call sites read it via
    // `subprocess::subprocess_timeout()` (Phase 2).
    subprocess::init_subprocess_timeout(config.subprocess_timeout());

    info!("Starting with config from: {:?}", cli.config);

    // Run the main application logic
    run_application(&cli, &config).context("Application failed")?;

    Ok(())
}

fn main() {
    // `Result`-returning `main` prints eyre's `Debug` impl on error, which
    // appends a `Location: src/....rs:NN` (and backtrace) trailer to every
    // user-facing error. Print just the Display chain (`.context()` chain,
    // no Location) instead; full detail still reaches the log file.
    if let Err(err) = run() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}
