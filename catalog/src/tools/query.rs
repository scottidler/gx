//! `query(root?, where{ dirty?, branch?, org?, lang?, behind_gt? })` -> repo
//! metadata rows (indexed SELECT), scope-clamped and output-bounded. Surfaces
//! `last_walk`/`last_fetch` per row so an agent sees staleness.

use eyre::{Context, Result};
use log::debug;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};
use serde::Serialize;
use std::path::Path;

use super::{bound_items, clamp_root, repo_row_from, scope_sql, Bounds, RepoRow, REPO_COLUMNS};

/// The `where{}` filters. Every field is optional; an absent field imposes no
/// constraint. Modeled as typed options (not free strings) so a caller cannot
/// silently mis-name a filter.
#[derive(Debug, Clone, Default)]
pub struct QueryFilter {
    pub dirty: Option<bool>,
    pub branch: Option<String>,
    pub org: Option<String>,
    pub lang: Option<String>,
    /// Match repos whose local `behind` count is strictly greater than this.
    pub behind_gt: Option<i64>,
}

/// The bounded result of a `query`: the matching repo rows plus whether a cap
/// truncated them.
#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub rows: Vec<RepoRow>,
    pub truncated: bool,
}

/// Run the indexed metadata query under a clamped `root` (default = caller CWD),
/// applying the `where{}` filters and the output bounds.
pub fn query(
    conn: &Connection,
    catalog_root: &Path,
    requested_root: Option<&Path>,
    filter: &QueryFilter,
    bounds: &Bounds,
) -> Result<QueryResult> {
    debug!(
        "query: catalog_root={} requested_root={:?} filter={filter:?}",
        catalog_root.display(),
        requested_root.map(|p| p.display().to_string())
    );
    let root = clamp_root(catalog_root, requested_root)?;
    let (root_str, prefix) = scope_sql(&root);

    let mut sql = format!("SELECT {REPO_COLUMNS} FROM repos WHERE (path = ? OR path LIKE ?)");
    let mut binds: Vec<Value> = vec![Value::Text(root_str), Value::Text(prefix)];

    if let Some(dirty) = filter.dirty {
        sql.push_str(" AND dirty = ?");
        binds.push(Value::Integer(dirty as i64));
    }
    if let Some(branch) = &filter.branch {
        sql.push_str(" AND branch = ?");
        binds.push(Value::Text(branch.clone()));
    }
    if let Some(org) = &filter.org {
        sql.push_str(" AND org = ?");
        binds.push(Value::Text(org.clone()));
    }
    if let Some(lang) = &filter.lang {
        sql.push_str(" AND lang = ?");
        binds.push(Value::Text(lang.clone()));
    }
    if let Some(behind_gt) = filter.behind_gt {
        // NULL behind (no local tracking ref) is never > N, exactly right.
        sql.push_str(" AND behind > ?");
        binds.push(Value::Integer(behind_gt));
    }
    sql.push_str(" ORDER BY slug");

    let mut stmt = conn.prepare(&sql).context("failed to prepare query")?;
    let rows: Vec<RepoRow> = stmt
        .query_map(params_from_iter(binds), repo_row_from)
        .context("failed to run query")?
        .collect::<rusqlite::Result<Vec<RepoRow>>>()
        .context("failed to read query rows")?;

    let matched = rows.len();
    let (rows, truncated) = bound_items(rows, bounds);
    debug!(
        "query: matched={matched} returned={} truncated={truncated}",
        rows.len()
    );
    Ok(QueryResult { rows, truncated })
}
