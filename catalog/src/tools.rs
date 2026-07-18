//! The four read-only intel tools (design doc `2026-07-17-gx-intel-catalog.md`,
//! Track B1, Phase 3): `query`, `search`, `read`, `deps`. All four live in the
//! `catalog` crate, which depends on `local` ONLY -- never `remote` -- so an
//! intel tool cannot compile a call to `persona`/`github`/`ssh`/remote-git. A
//! CI guard (`bin/check-catalog-boundary.sh`, wired into `otto ci`) asserts the
//! `remote` dependency never reappears.
//!
//! Two invariants are shared by every tool and enforced here, once:
//!
//! - **Scope clamp** ([`clamp_root`]): canonicalize `catalog.root` (the ceiling),
//!   the requested `root` (default = caller CWD), and compare component-wise.
//!   A requested root that canonicalizes OUTSIDE the ceiling, or does not exist,
//!   is REJECTED loudly (fail closed) -- never widened or emptied. Because
//!   `canonicalize` resolves symlinks and `..`, a symlink-escape or `..`-escape
//!   both resolve to a real path outside the ceiling and are rejected. The SQL
//!   predicate ([`scope_sql`]) then matches `path = :root OR path LIKE :root ||
//!   '/%'`; the trailing `/%` avoids the sibling-prefix bug where `/repos/foo`
//!   would otherwise match `/repos/foobar`.
//! - **Output bounds** ([`Bounds`], [`bound_items`]): every tool caps result
//!   count AND total serialized bytes and returns `truncated: true` when a cap
//!   trips. An MCP response serializes as one JSON content block, so a
//!   fleet-sized payload would blow the protocol limit.

use eyre::{bail, Context, Result};
use log::debug;
use rusqlite::Row;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub mod deps;
pub mod query;
pub mod read;
pub mod search;

/// Default cap on the number of rows/hits a tool returns before truncating.
/// The const IS the default; there is no per-tool config surface for it yet
/// (the design's `catalog:` block defines only `root`/`staleness-secs`), so a
/// future tunable would default to this value.
pub const DEFAULT_MAX_RESULTS: usize = 500;

/// Default cap on the total serialized bytes a tool returns before truncating
/// (~1 MiB). Keeps a single MCP JSON content block under the protocol limit.
pub const DEFAULT_MAX_BYTES: usize = 1_048_576;

/// The two output caps every tool honors. Constructed from the module-level
/// consts by [`Bounds::default`]; a caller (e.g. the MCP wiring) may pass a
/// tighter bound.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub max_results: usize,
    pub max_bytes: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_MAX_RESULTS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// The `repos`-table columns, in a fixed order shared by [`repo_row_from`] and
/// every SELECT that reads a full repo row.
pub const REPO_COLUMNS: &str = "slug, org, name, path, branch, dirty, ahead, behind, lang, \
     last_commit_sha, last_commit_time, last_walk, last_fetch";

/// One repo metadata row, surfacing `last_walk`/`last_fetch` so an agent can see
/// staleness (the catalog is a rebuildable cache, not a hot service).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoRow {
    pub slug: String,
    pub org: String,
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    pub dirty: bool,
    pub ahead: Option<i64>,
    pub behind: Option<i64>,
    pub lang: Option<String>,
    pub last_commit_sha: Option<String>,
    pub last_commit_time: Option<i64>,
    /// Unix time the local walk last read this repo's state.
    pub last_walk: i64,
    /// Unix time of the last `git fetch` (FETCH_HEAD mtime); `None` if never.
    pub last_fetch: Option<i64>,
}

/// Read a [`RepoRow`] from a row whose columns are [`REPO_COLUMNS`] in order.
pub fn repo_row_from(row: &Row) -> rusqlite::Result<RepoRow> {
    let dirty: i64 = row.get(5)?;
    Ok(RepoRow {
        slug: row.get(0)?,
        org: row.get(1)?,
        name: row.get(2)?,
        path: row.get(3)?,
        branch: row.get(4)?,
        dirty: dirty != 0,
        ahead: row.get(6)?,
        behind: row.get(7)?,
        lang: row.get(8)?,
        last_commit_sha: row.get(9)?,
        last_commit_time: row.get(10)?,
        last_walk: row.get(11)?,
        last_fetch: row.get(12)?,
    })
}

/// Canonicalize the requested `root` (default = caller CWD) and clamp it inside
/// the canonicalized `catalog_root` ceiling. Fail-closed: a root that does not
/// exist (canonicalize errors) or resolves outside the ceiling is REJECTED with
/// a loud error, never widened to the ceiling or silently emptied. `starts_with`
/// compares whole path components, so `/repos/foobar` does NOT match a ceiling
/// of `/repos/foo` (the sibling-prefix bug is closed at this layer too).
pub fn clamp_root(catalog_root: &Path, requested: Option<&Path>) -> Result<PathBuf> {
    let requested: PathBuf = match requested {
        Some(r) => r.to_path_buf(),
        None => {
            std::env::current_dir().context("failed to read caller CWD for the default root")?
        }
    };
    debug!(
        "clamp_root: catalog_root={} requested={}",
        catalog_root.display(),
        requested.display()
    );

    let ceiling = catalog_root.canonicalize().with_context(|| {
        format!(
            "catalog.root {} does not exist or cannot be resolved",
            catalog_root.display()
        )
    })?;
    // Fail closed: a requested root that does not exist cannot be canonicalized,
    // and is rejected here rather than widened to the ceiling.
    let root = requested.canonicalize().with_context(|| {
        format!(
            "requested root {} does not exist or cannot be resolved",
            requested.display()
        )
    })?;

    if root != ceiling && !root.starts_with(&ceiling) {
        bail!(
            "requested root {} is outside the catalog root {} (scope clamp; fail closed)",
            root.display(),
            ceiling.display()
        );
    }
    debug!("clamp_root: clamped root={}", root.display());
    Ok(root)
}

/// The `(root, prefix)` pair for the scope-clamp SQL predicate
/// `path = :root OR path LIKE :prefix`, where `:prefix` is `root || '/%'`. The
/// trailing `/%` (not a bare `%`) is what keeps `/repos/foo` from matching
/// `/repos/foobar`.
pub fn scope_sql(root: &Path) -> (String, String) {
    let root_str = root.to_string_lossy().to_string();
    let prefix = format!("{root_str}/%");
    (root_str, prefix)
}

/// Cap a serializable result set by BOTH count and total serialized bytes,
/// returning the kept items and whether anything was dropped. At least one item
/// is always kept if the input is non-empty (so a single oversized item is
/// returned WITH `truncated = true` rather than an empty success).
pub fn bound_items<T: Serialize>(items: Vec<T>, bounds: &Bounds) -> (Vec<T>, bool) {
    let total = items.len();
    let mut out: Vec<T> = Vec::with_capacity(total.min(bounds.max_results));
    let mut bytes = 0usize;
    for item in items {
        if out.len() >= bounds.max_results {
            break;
        }
        let sz = serde_json::to_vec(&item).map(|v| v.len()).unwrap_or(0);
        if !out.is_empty() && bytes + sz > bounds.max_bytes {
            break;
        }
        bytes += sz;
        out.push(item);
    }
    let truncated = out.len() < total;
    (out, truncated)
}

/// Truncate a string to at most `max` bytes on a char boundary, reporting
/// whether it was cut. Never panics on a multibyte boundary (rust.md UTF-8
/// footgun): walks the byte index down to the nearest boundary before cutting.
pub fn truncate_bytes(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s.get(..end).unwrap_or("").to_string(), true)
}

#[cfg(test)]
mod tests;
