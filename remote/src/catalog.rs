//! `gx catalog` handler (design doc `2026-07-17-gx-intel-catalog.md`, Track B1,
//! Phase 2). Opens the SQLite catalog and runs the LOCAL walk over
//! `catalog.root`. The `--fetch` network refresh is a Phase 4 deliverable and
//! errors loudly here rather than silently no-op'ing.
//!
//! This handler lives in `remote` (which owns the CLI + dispatch), but the walk
//! itself lives in the `catalog` crate, which depends on `local` ONLY. The
//! network/persona surface is never reachable from the indexing path.

use crate::cli::Cli;
use catalog::db;
use catalog::walk;
use eyre::{bail, Context, Result};
use local::config::Config;
use local::utils::get_max_depth_from_config;
use log::info;

/// Process the `gx catalog` subcommand. LOCAL walk of `catalog.root`; `--fetch`
/// is rejected until Phase 4 wires the per-repo network refresh.
pub fn process_catalog_command(cli: &Cli, config: &Config, fetch: bool) -> Result<()> {
    info!("Processing catalog command (fetch: {fetch})");

    if fetch {
        // Fail loud, fail closed: never pretend a fetch happened. Phase 4 wires
        // `git::fetch_origin` per repo; until then this is an explicit error,
        // not a silent local-only fallback.
        bail!(
            "`gx catalog --fetch` (network refresh) is not yet implemented (Phase 4 of the \
             intel-catalog design doc); run `gx catalog` for a local walk"
        );
    }

    let root = config.catalog_root();
    let max_depth = cli
        .max_depth
        .or_else(|| get_max_depth_from_config(config))
        .unwrap_or(3);
    let ignore_patterns = config.ignore_patterns();

    info!(
        "Catalog walk: root={} max_depth={max_depth}",
        root.display()
    );

    let mut conn = db::open_default().context("failed to open the catalog database")?;
    let summary =
        walk::walk(&mut conn, &root, max_depth, &ignore_patterns).context("catalog walk failed")?;

    println!(
        "📚 Catalog updated: {} repo(s) indexed, {} dependency row(s), {} pruned",
        summary.repos_indexed, summary.deps_indexed, summary.pruned
    );
    Ok(())
}
