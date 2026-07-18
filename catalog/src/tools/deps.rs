//! `deps(dependency?)` -> repos using that dependency; `deps(slug?)` -> that
//! repo's dependency list. Indexed SELECT in both directions, scope-clamped and
//! output-bounded, surfacing `last_walk`/`last_fetch` staleness.

use eyre::{bail, Context, Result};
use log::debug;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

use super::{bound_items, clamp_root, scope_sql, Bounds};

/// One repo that uses a queried dependency (the `deps(dependency)` direction).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DepUse {
    pub slug: String,
    pub path: String,
    pub ecosystem: String,
    pub version_req: Option<String>,
    pub kind: String,
    pub last_walk: i64,
    pub last_fetch: Option<i64>,
}

/// One dependency of a queried repo (the `deps(slug)` direction).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DepRow {
    pub ecosystem: String,
    pub name: String,
    pub version_req: Option<String>,
    pub kind: String,
}

/// The result of a `deps` call, tagged by which direction was queried.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "direction", rename_all = "kebab-case")]
pub enum DepsResult {
    /// `deps(dependency)`: the repos (in scope) that depend on `dependency`.
    ByDependency {
        dependency: String,
        repos: Vec<DepUse>,
        truncated: bool,
    },
    /// `deps(slug)`: the dependency list of the named repo (which must be in
    /// scope), plus that repo's staleness stamps.
    BySlug {
        slug: String,
        last_walk: i64,
        last_fetch: Option<i64>,
        deps: Vec<DepRow>,
        truncated: bool,
    },
}

/// Look up dependencies in one of two directions. Exactly one of `dependency` /
/// `slug` must be supplied; supplying both or neither is a loud error (fail
/// closed rather than guessing). Results are clamped to `catalog_root` (the
/// ceiling): the repos returned, or the named repo, must live under it.
pub fn deps(
    conn: &Connection,
    catalog_root: &Path,
    dependency: Option<&str>,
    slug: Option<&str>,
    bounds: &Bounds,
) -> Result<DepsResult> {
    debug!(
        "deps: catalog_root={} dependency={dependency:?} slug={slug:?}",
        catalog_root.display()
    );
    // Scope ceiling: the catalog root itself (all indexed repos live under it).
    let root = clamp_root(catalog_root, Some(catalog_root))?;
    let (root_str, prefix) = scope_sql(&root);

    match (dependency, slug) {
        (Some(dep), None) => deps_by_dependency(conn, dep, &root_str, &prefix, bounds),
        (None, Some(slug)) => deps_by_slug(conn, slug, &root_str, &prefix, bounds),
        (Some(_), Some(_)) => bail!("deps: pass exactly one of `dependency` or `slug`, not both"),
        (None, None) => {
            bail!("deps: pass either `dependency` (repos using it) or `slug` (its deps)")
        }
    }
}

fn deps_by_dependency(
    conn: &Connection,
    dependency: &str,
    root_str: &str,
    prefix: &str,
    bounds: &Bounds,
) -> Result<DepsResult> {
    let mut stmt = conn
        .prepare(
            "SELECT r.slug, r.path, d.ecosystem, d.version_req, d.kind, r.last_walk, r.last_fetch
             FROM deps d JOIN repos r ON r.slug = d.repo_slug
             WHERE d.name = ?1 AND (r.path = ?2 OR r.path LIKE ?3)
             ORDER BY r.slug, d.kind",
        )
        .context("failed to prepare deps-by-dependency query")?;
    let repos: Vec<DepUse> = stmt
        .query_map(params![dependency, root_str, prefix], |row| {
            Ok(DepUse {
                slug: row.get(0)?,
                path: row.get(1)?,
                ecosystem: row.get(2)?,
                version_req: row.get(3)?,
                kind: row.get(4)?,
                last_walk: row.get(5)?,
                last_fetch: row.get(6)?,
            })
        })
        .context("failed to run deps-by-dependency query")?
        .collect::<rusqlite::Result<Vec<DepUse>>>()
        .context("failed to read deps-by-dependency rows")?;

    let matched = repos.len();
    let (repos, truncated) = bound_items(repos, bounds);
    debug!(
        "deps_by_dependency: dep={dependency} matched={matched} returned={}",
        repos.len()
    );
    Ok(DepsResult::ByDependency {
        dependency: dependency.to_string(),
        repos,
        truncated,
    })
}

fn deps_by_slug(
    conn: &Connection,
    slug: &str,
    root_str: &str,
    prefix: &str,
    bounds: &Bounds,
) -> Result<DepsResult> {
    // The repo must exist AND be in scope; fail loud otherwise (never an empty
    // success that hides an out-of-scope or unknown slug).
    let stamps: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT last_walk, last_fetch FROM repos
             WHERE slug = ?1 AND (path = ?2 OR path LIKE ?3)",
            params![slug, root_str, prefix],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .context("failed to look up repo for deps-by-slug")?;

    let Some((last_walk, last_fetch)) = stamps else {
        bail!("deps: no repo `{slug}` in the catalog under the catalog root (walk it first, or check the slug)");
    };

    let mut stmt = conn
        .prepare(
            "SELECT ecosystem, name, version_req, kind FROM deps
             WHERE repo_slug = ?1 ORDER BY ecosystem, name, kind",
        )
        .context("failed to prepare deps-by-slug query")?;
    let rows: Vec<DepRow> = stmt
        .query_map(params![slug], |row| {
            Ok(DepRow {
                ecosystem: row.get(0)?,
                name: row.get(1)?,
                version_req: row.get(2)?,
                kind: row.get(3)?,
            })
        })
        .context("failed to run deps-by-slug query")?
        .collect::<rusqlite::Result<Vec<DepRow>>>()
        .context("failed to read deps-by-slug rows")?;

    let matched = rows.len();
    let (deps, truncated) = bound_items(rows, bounds);
    debug!(
        "deps_by_slug: slug={slug} matched={matched} returned={}",
        deps.len()
    );
    Ok(DepsResult::BySlug {
        slug: slug.to_string(),
        last_walk,
        last_fetch,
        deps,
        truncated,
    })
}
