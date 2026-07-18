//! `gx catalog` handler (design doc `2026-07-17-gx-intel-catalog.md`, Track B1,
//! Phases 2 & 4). Opens the SQLite catalog and runs the LOCAL walk over
//! `catalog.root`; `gx catalog --fetch` additionally fetches each repo's
//! existing `origin` remote (Phase 4) before re-walking so `last_fetch`
//! (FETCH_HEAD mtime) and ahead/behind refresh.
//!
//! This handler lives in `remote` (which owns the CLI + dispatch AND the
//! credential-bound `git::fetch_origin`), but the indexing walk lives in the
//! `catalog` crate, which depends on `local` ONLY. `--fetch` is the ONLY
//! network path and is CLI-only: it is NEVER an MCP tool, and the intel tools in
//! `catalog` cannot reach it (no `remote` dep). The one network path iterates
//! one repo at a time, each fetching its own `origin` -- N independent
//! single-org fetches, never one cross-org operation.

use crate::cli::Cli;
use crate::git;
use catalog::db;
use catalog::walk;
use eyre::{Context, Result};
use local::config::Config;
use local::repo::discover_repos;
use local::utils::get_max_depth_from_config;
use log::{debug, info, warn};
use rusqlite::Connection;
use std::path::Path;

/// Outcome of a `gx catalog --fetch` refresh: how many repos fetched cleanly,
/// how many fetch failures were reported-and-skipped, and the re-walk summary.
#[derive(Debug, Clone)]
pub struct FetchSummary {
    /// Repos whose `origin` fetch succeeded.
    pub fetched: usize,
    /// Repos whose `origin` fetch failed (reported loudly, skipped, run
    /// continued).
    pub fetch_failed: usize,
    /// The re-walk that folded FETCH_HEAD mtimes + refreshed ahead/behind into
    /// the catalog.
    pub walk: walk::WalkSummary,
}

/// Process the `gx catalog` subcommand. `--fetch` runs the per-repo network
/// refresh (Phase 4); otherwise a LOCAL walk of `catalog.root`.
pub fn process_catalog_command(cli: &Cli, config: &Config, fetch: bool) -> Result<()> {
    info!("Processing catalog command (fetch: {fetch})");

    let root = config.catalog_root();
    let max_depth = cli
        .max_depth
        .or_else(|| get_max_depth_from_config(config))
        .unwrap_or(3);
    let ignore_patterns = config.ignore_patterns();

    info!(
        "Catalog: root={} max_depth={max_depth} fetch={fetch}",
        root.display()
    );

    let mut conn = db::open_default().context("failed to open the catalog database")?;

    if fetch {
        let summary = fetch_refresh(&mut conn, &root, max_depth, &ignore_patterns)?;
        println!(
            "📚 Catalog refreshed (fetch): {} repo(s) fetched, {} fetch failure(s) skipped, \
             {} repo(s) indexed, {} dependency row(s), {} pruned",
            summary.fetched,
            summary.fetch_failed,
            summary.walk.repos_indexed,
            summary.walk.deps_indexed,
            summary.walk.pruned
        );
    } else {
        let summary = walk::walk(&mut conn, &root, max_depth, &ignore_patterns)
            .context("catalog walk failed")?;
        println!(
            "📚 Catalog updated: {} repo(s) indexed, {} dependency row(s), {} pruned",
            summary.repos_indexed, summary.deps_indexed, summary.pruned
        );
    }
    Ok(())
}

/// Fetch each repo's existing `origin` remote (per-repo, single-org; auth rides
/// the origin URL + `~/.ssh/config`, never a gx-selected token), then re-walk
/// the whole subtree so FETCH_HEAD mtimes (`last_fetch`) and the updated
/// remote-tracking refs (ahead/behind) land in the catalog.
///
/// A per-repo fetch failure (auth/network) is reported LOUDLY (a `warn!` plus a
/// stderr line) and SKIPPED -- the run NEVER aborts on one repo, so the rest of
/// the fleet still refreshes. The re-walk is LOCAL only (zero network), so even
/// a repo that failed to fetch is re-indexed from its current local state.
pub fn fetch_refresh(
    conn: &mut Connection,
    root: &Path,
    max_depth: usize,
    ignore_patterns: &[String],
) -> Result<FetchSummary> {
    info!(
        "fetch_refresh: root={} max_depth={max_depth}",
        root.display()
    );

    let repos = discover_repos(root, max_depth, ignore_patterns)
        .context("failed to discover repos for catalog fetch")?;
    debug!("fetch_refresh: discovered {} repos", repos.len());

    let mut fetched = 0usize;
    let mut fetch_failed = 0usize;
    for repo in &repos {
        match git::fetch_origin(&repo.path) {
            Ok(()) => {
                debug!("fetch_refresh: fetched {}", repo.slug);
                fetched += 1;
            }
            Err(e) => {
                // Fail loud, skip, continue: one repo's auth/network failure
                // never aborts the fleet refresh.
                warn!(
                    "fetch_refresh: fetch failed for {} ({e}); skipping",
                    repo.slug
                );
                eprintln!("⚠️  catalog fetch failed for {} ({e}); skipping", repo.slug);
                fetch_failed += 1;
            }
        }
    }

    // Re-walk the subtree so FETCH_HEAD mtimes (last_fetch) and the refreshed
    // remote-tracking refs (ahead/behind) land in the catalog. LOCAL only.
    let walk =
        walk::walk(conn, root, max_depth, ignore_patterns).context("re-walk after fetch failed")?;

    info!(
        "fetch_refresh: fetched={fetched} fetch_failed={fetch_failed} indexed={}",
        walk.repos_indexed
    );
    Ok(FetchSummary {
        fetched,
        fetch_failed,
        walk,
    })
}

#[cfg(test)]
mod tests;
