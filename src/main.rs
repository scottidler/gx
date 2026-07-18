use eyre::{Context, Result};
use log::info;
use std::env;
use std::fs;
use std::path::PathBuf;

use local::config::{xdg_data_dir, Config};
use remote::cli::{Cli, Commands};

fn setup_logging(level: remote::cli::LogLevel) -> Result<()> {
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

fn run() -> Result<()> {
    use clap::{CommandFactory, FromArgMatches};

    // Render the log path at runtime from the same XDG source the logger uses,
    // so --help never drifts and we don't spawn subprocesses during parsing ([A24]).
    let after_help = format!(
        "Logs are written to: {}\nRun `gx doctor` to check required tools.",
        remote::doctor::log_path().display()
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
    // BEFORE gx's env_logger init (a second env_logger init would panic).
    // `Config` lives in the `local` crate and is shared by both the bin (here)
    // and `remote`'s mcp handler - one type, not two.
    if let Commands::Mcp(cmd) = &cli.command {
        let config = local::config::Config::load(cli.config.as_ref())
            .context("Failed to load configuration")?;
        let io = mcp_io::mcp_io!();
        std::process::exit(cmd.run(&io, || {
            Ok::<_, std::convert::Infallible>(remote::mcp::server::GxMcpServer::new(config))
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
    local::subprocess::init_subprocess_timeout(config.subprocess_timeout());

    info!("Starting with config from: {:?}", cli.config);

    // Run the main application logic
    remote::app::run_application(&cli, &config).context("Application failed")?;

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
