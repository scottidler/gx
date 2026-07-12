//! gx-mcp: MCP stdio server fronting gx's core fns for agent-driven fleet
//! campaigns (design: docs/design/2026-07-12-llm-propose-apply-and-mcp-server.md).
//!
//! Logging is FILE ONLY, matching gx's own `env_logger` + file-target setup
//! (`gx::main::setup_logging`): stdout carries nothing but JSON-RPC bytes,
//! since rmcp's stdio transport IS stdin/stdout here. A stray `println!`
//! would corrupt every client's protocol stream, so this binary never writes
//! to stdout/stderr outside the transport itself.

mod gate;
mod logic;
mod schema;
mod server;

use eyre::{Context, Result};
use log::info;
use rmcp::ServiceExt;
use std::fs;
use std::path::PathBuf;

use gx::config::Config;
use server::GxMcpServer;

/// Set up file-only logging under gx's own XDG data dir (`gx::config::xdg_data_dir`),
/// sibling to gx's `gx.log` but its own file so the two processes' logs don't
/// interleave. Never targets stdout/stderr -- see the module doc for why.
fn setup_logging() -> Result<()> {
    let log_dir = gx::config::xdg_data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gx")
        .join("logs");

    fs::create_dir_all(&log_dir).context("Failed to create log directory")?;
    let log_file = log_dir.join("gx-mcp.log");

    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .context("Failed to open log file")?,
    );

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(target))
        .init();

    info!(
        "gx-mcp logging initialized, writing to: {}",
        log_file.display()
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging()?;
    info!("gx-mcp starting");

    // Load gx's config the same way gx itself does (XDG path), so `mcp.tools`
    // gating is read from the shared `gx.yml`. A parse failure is fatal and
    // never reaches stdout (logged to the file, returned as a process error).
    let config = Config::load(None).context("Failed to load gx config")?;
    let server = GxMcpServer::new(config);
    let transport = (tokio::io::stdin(), tokio::io::stdout());

    info!("gx-mcp: starting stdio MCP transport");
    let service = server
        .serve(transport)
        .await
        .context("Failed to start MCP server over stdio")?;
    info!("gx-mcp: server started, waiting for requests");

    service.waiting().await.context("MCP server error")?;
    info!("gx-mcp: server shutting down");

    Ok(())
}
