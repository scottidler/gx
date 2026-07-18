//! `search(root?, pattern, glob?)` -> `[{slug, path, line_no, line}]`. Shells out
//! to `rg` over the scoped repo paths (LIVE working tree, never the index), with
//! a subprocess timeout. The catalog is consulted ONLY to enumerate the in-scope
//! repo paths (so each hit maps back to a slug); the file CONTENT is always read
//! live by `rg`, never from a stale index.

use eyre::{bail, Context, Result};
use log::debug;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use local::subprocess::run_checked;

use super::{bound_items, clamp_root, scope_sql, Bounds};

/// One `rg` hit, with the owning repo's slug resolved from the scoped paths.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchHit {
    pub slug: String,
    pub path: String,
    pub line_no: u64,
    pub line: String,
}

/// The bounded result of a `search`.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
}

/// Search the working trees of the repos under a clamped `root` for `pattern`,
/// optionally restricted by an `rg` `glob`. `rg` is spawned once over every
/// in-scope repo path with a wall-clock `timeout` (a wedged `rg` is killed, not
/// waited on forever).
pub fn search(
    conn: &Connection,
    catalog_root: &Path,
    requested_root: Option<&Path>,
    pattern: &str,
    glob: Option<&str>,
    bounds: &Bounds,
    timeout: Duration,
) -> Result<SearchResult> {
    debug!(
        "search: catalog_root={} requested_root={:?} pattern_len={} glob={glob:?}",
        catalog_root.display(),
        requested_root.map(|p| p.display().to_string()),
        pattern.len()
    );
    if pattern.is_empty() {
        bail!("search: pattern must not be empty");
    }
    let root = clamp_root(catalog_root, requested_root)?;
    let (root_str, prefix) = scope_sql(&root);

    // The scoped repo paths (also used to resolve each hit's slug). Longest path
    // first so a nested repo wins the prefix match over its parent.
    let mut stmt = conn
        .prepare("SELECT slug, path FROM repos WHERE (path = ?1 OR path LIKE ?2) ORDER BY path")
        .context("failed to prepare scoped-repo lookup for search")?;
    let mut scoped: Vec<(String, PathBuf)> = stmt
        .query_map(params![root_str, prefix], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
            ))
        })
        .context("failed to run scoped-repo lookup for search")?
        .collect::<rusqlite::Result<Vec<(String, PathBuf)>>>()
        .context("failed to read scoped repo paths for search")?;
    drop(stmt);
    scoped.sort_by_key(|(_, path)| std::cmp::Reverse(path.as_os_str().len()));

    if scoped.is_empty() {
        // A valid but empty scope (nothing indexed under root): no hits, not an
        // error. An unwalked catalog surfaces as zero repos here.
        debug!("search: no repos in scope; returning empty result");
        return Ok(SearchResult {
            hits: Vec::new(),
            truncated: false,
        });
    }

    let mut cmd = Command::new("rg");
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--with-filename")
        .arg("--color")
        .arg("never");
    if let Some(glob) = glob {
        cmd.arg("--glob").arg(glob);
    }
    cmd.arg("-e").arg(pattern).arg("--");
    for (_, path) in &scoped {
        cmd.arg(path);
    }

    let output = run_checked(&mut cmd, timeout).map_err(|e| {
        // A spawn failure is almost always "rg not installed" -- fail loud and
        // name the fix, never an empty-as-success.
        eyre::eyre!("search: failed to run `rg` ({e}); is ripgrep installed? (`gx doctor` checks)")
    })?;

    // rg exit codes: 0 = matches, 1 = no matches (NOT an error), 2 = real error.
    match output.status.code() {
        Some(0) | Some(1) => {}
        _ => bail!(
            "search: `rg` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hits: Vec<SearchHit> = Vec::new();
    for line in stdout.lines() {
        if let Some(hit) = parse_rg_line(line, &scoped) {
            hits.push(hit);
        }
    }

    let matched = hits.len();
    let (hits, truncated) = bound_items(hits, bounds);
    debug!(
        "search: matched={matched} returned={} truncated={truncated}",
        hits.len()
    );
    Ok(SearchResult { hits, truncated })
}

/// Parse one `rg --with-filename --line-number --no-heading` line
/// (`<path>:<line_no>:<content>`) into a [`SearchHit`], resolving the slug from
/// the scoped repo paths. `None` for a malformed line or a path under no known
/// repo (should not happen given we searched those paths).
fn parse_rg_line(line: &str, scoped: &[(String, PathBuf)]) -> Option<SearchHit> {
    // Split off the path and line number. The content may itself contain `:`,
    // so split only twice from the left.
    let (path, rest) = line.split_once(':')?;
    let (line_no, content) = rest.split_once(':')?;
    let line_no: u64 = line_no.parse().ok()?;

    let hit_path = Path::new(path);
    // Longest-path-first `scoped` -> first prefix match is the innermost repo.
    let slug = scoped
        .iter()
        .find(|(_, repo)| hit_path.starts_with(repo))
        .map(|(slug, _)| slug.clone())?;

    Some(SearchHit {
        slug,
        path: path.to_string(),
        line_no,
        line: content.to_string(),
    })
}
