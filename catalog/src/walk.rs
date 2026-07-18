//! The LOCAL walk indexer (design doc `2026-07-17-gx-intel-catalog.md`, Track
//! B1, Phase 2). Discovers every repo under a root and populates the catalog
//! with ZERO network access: branch/dirty/ahead/behind come from
//! `local::git::get_repo_status_local` (which reads LOCAL tracking refs, never
//! fetches), last-commit from `git log`, and last-fetch from the `FETCH_HEAD`
//! file's mtime (never a fetch). Manifest parsing (`Cargo.toml`,
//! `package.json`) yields a primary-language guess plus the dependency list.
//!
//! Read-only per-repo state is gathered in parallel (rayon `par_iter`), then
//! ALL writes are serialized through the single DB connection: each repo's
//! `repos` upsert and `deps` replace ride ONE transaction (`DELETE FROM deps
//! WHERE repo_slug=?` then insert), matching the Data Model idempotence rule.
//! Repos no longer on disk under the walked root are pruned (their `deps`
//! cascade via `ON DELETE CASCADE`).

use eyre::{Context, Result};
use log::{debug, trace, warn};
use rayon::prelude::*;
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use local::git::{get_repo_status_local, RemoteStatus};
use local::repo::{discover_repos, Repo};
use local::subprocess::{run_checked, subprocess_timeout};

/// A single dependency parsed from a repo's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepRecord {
    /// `cargo` | `npm`.
    pub ecosystem: String,
    pub name: String,
    /// The version requirement string, or `None` for a git/path/workspace dep
    /// that carries no version.
    pub version_req: Option<String>,
    /// `normal` | `dev` | `build`.
    pub kind: String,
}

/// The fully-resolved, catalog-ready record for one repo. Built read-only (no
/// DB, no network) so the gather step can run under rayon; the write step
/// consumes it against the single connection.
#[derive(Debug, Clone)]
pub struct RepoRecord {
    pub slug: String,
    pub org: String,
    pub name: String,
    /// Canonical absolute path (the scope clamp filters on this).
    pub path: PathBuf,
    pub branch: Option<String>,
    pub dirty: bool,
    pub ahead: Option<i64>,
    pub behind: Option<i64>,
    pub lang: Option<String>,
    pub last_commit_sha: Option<String>,
    pub last_commit_time: Option<i64>,
    pub last_walk: i64,
    pub last_fetch: Option<i64>,
    pub deps: Vec<DepRecord>,
}

/// Outcome of a walk, for the CLI to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkSummary {
    /// Repos discovered on disk and upserted.
    pub repos_indexed: usize,
    /// Total dep rows written across all repos.
    pub deps_indexed: usize,
    /// Repos pruned (present in the catalog under `root`, gone from disk).
    pub pruned: usize,
}

/// Walk `root` (to `max_depth`, honoring `ignore_patterns`) and populate the
/// catalog behind `conn`. LOCAL only: never fetches. Idempotent (a re-walk
/// replaces each repo's rows) and self-pruning (repos gone from disk under
/// `root` are removed, cascading their deps).
pub fn walk(
    conn: &mut Connection,
    root: &Path,
    max_depth: usize,
    ignore_patterns: &[String],
) -> Result<WalkSummary> {
    debug!(
        "walk: root={} max_depth={} ignore_patterns={:?}",
        root.display(),
        max_depth,
        ignore_patterns
    );

    let repos = discover_repos(root, max_depth, ignore_patterns)
        .context("failed to discover repos for catalog walk")?;
    debug!("walk: discovered {} repos", repos.len());

    // Gather read-only per-repo state in parallel. NO DB, NO network here.
    let records: Vec<RepoRecord> = repos.par_iter().map(build_record).collect();

    let last_walk = now_unix();

    // Serialize every write through the single connection. One txn per repo.
    let mut deps_indexed = 0usize;
    let mut seen_slugs: HashSet<String> = HashSet::with_capacity(records.len());
    for record in &records {
        deps_indexed += write_record(conn, record, last_walk)
            .with_context(|| format!("failed to index repo {}", record.slug))?;
        seen_slugs.insert(record.slug.clone());
    }

    // Prune rows for repos gone from disk, scoped to the walked subtree so a
    // subtree walk never wipes out-of-scope repos.
    let pruned = prune_missing(conn, root, &seen_slugs)
        .context("failed to prune removed repos from catalog")?;

    let summary = WalkSummary {
        repos_indexed: records.len(),
        deps_indexed,
        pruned,
    };
    debug!(
        "walk: done repos_indexed={} deps_indexed={} pruned={}",
        summary.repos_indexed, summary.deps_indexed, summary.pruned
    );
    Ok(summary)
}

/// Auto-walk-on-stale gate for the MCP read tools (design doc Architecture
/// "an MCP `query` may trigger only a local walk when rows are stale" +
/// Edge Cases "Empty or fully-stale catalog on an MCP query"). Walks the scoped
/// subtree LOCALLY -- never a fetch -- when the catalog has NO rows under the
/// clamped `requested_root` (empty/unbuilt) OR its OLDEST in-scope row is older
/// than `staleness_secs`. Returns whether a walk was performed.
///
/// LOCAL only: it reuses [`walk`], which issues zero network calls, so the
/// cross-org boundary and the no-fetch guarantee both hold. `--fetch` remains
/// the ONLY network path and stays CLI-only; this gate never fetches. It never
/// returns empty-as-success on an unbuilt catalog -- a missing/empty catalog is
/// built on the first query.
pub fn ensure_fresh(
    conn: &mut Connection,
    catalog_root: &Path,
    requested_root: Option<&Path>,
    max_depth: usize,
    ignore_patterns: &[String],
    staleness_secs: u64,
) -> Result<bool> {
    debug!(
        "ensure_fresh: catalog_root={} requested_root={:?} max_depth={} staleness_secs={}",
        catalog_root.display(),
        requested_root.map(|p| p.display().to_string()),
        max_depth,
        staleness_secs
    );

    // Clamp the requested root inside the ceiling exactly as the tools do (same
    // fail-closed semantics), so the freshness check and the query it precedes
    // agree on the scope.
    let root = crate::tools::clamp_root(catalog_root, requested_root)
        .context("failed to clamp root for the staleness check")?;
    let (root_str, prefix) = crate::tools::scope_sql(&root);

    // COUNT(*) always returns one row; MIN(last_walk) is NULL when the scope is
    // empty (count == 0), which we treat as "unbuilt -> walk".
    let (count, min_last_walk): (i64, Option<i64>) = conn
        .query_row(
            "SELECT COUNT(*), MIN(last_walk) FROM repos WHERE (path = ?1 OR path LIKE ?2)",
            params![root_str, prefix],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("failed to read catalog freshness")?;

    let now = now_unix();
    let stale = match (count, min_last_walk) {
        // Empty/unbuilt scope: never serve empty-as-success, walk first.
        (0, _) | (_, None) => {
            debug!(
                "ensure_fresh: catalog empty/unbuilt under {} (count={count}); walking",
                root.display()
            );
            true
        }
        (_, Some(oldest)) => {
            let age = now.saturating_sub(oldest);
            let stale = age > staleness_secs as i64;
            debug!(
                "ensure_fresh: root={} count={count} oldest_age={age}s staleness={staleness_secs}s stale={stale}",
                root.display()
            );
            stale
        }
    };

    if stale {
        walk(conn, &root, max_depth, ignore_patterns).context("auto-walk-on-stale failed")?;
    }
    Ok(stale)
}

/// Build the catalog record for one repo: LOCAL status, last-commit, last-fetch
/// mtime, and manifest-derived lang + deps. No DB, no network -- safe under
/// rayon.
fn build_record(repo: &Repo) -> RepoRecord {
    trace!(
        "build_record: slug={} path={}",
        repo.slug,
        repo.path.display()
    );

    let status = get_repo_status_local(repo);
    let (ahead, behind) = ahead_behind(&status.remote_status);

    let (org, name) = split_slug(&repo.slug);
    let path = canonical_path(&repo.path);
    let (last_commit_sha, last_commit_time) = last_commit(&repo.path);
    let last_fetch = last_fetch_mtime(&repo.path);
    let (lang, deps) = parse_manifests(&repo.path);

    RepoRecord {
        slug: repo.slug.clone(),
        org,
        name,
        path,
        branch: status.branch,
        dirty: !status.is_clean,
        ahead,
        behind,
        lang,
        last_commit_sha,
        last_commit_time,
        // Overwritten with the shared walk timestamp at write time; a per-record
        // placeholder keeps the struct self-contained for tests.
        last_walk: now_unix(),
        last_fetch,
        deps,
    }
}

/// Upsert one repo and REPLACE its deps inside a single transaction. Returns the
/// number of dep rows written for this repo.
fn write_record(conn: &mut Connection, record: &RepoRecord, last_walk: i64) -> Result<usize> {
    let txn = conn.transaction().context("failed to begin repo txn")?;

    let path_str = record.path.to_string_lossy().to_string();
    txn.execute(
        "INSERT INTO repos (
             slug, org, name, path, branch, dirty, ahead, behind, lang,
             last_commit_sha, last_commit_time, last_walk, last_fetch
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(slug) DO UPDATE SET
             org              = excluded.org,
             name             = excluded.name,
             path             = excluded.path,
             branch           = excluded.branch,
             dirty            = excluded.dirty,
             ahead            = excluded.ahead,
             behind           = excluded.behind,
             lang             = excluded.lang,
             last_commit_sha  = excluded.last_commit_sha,
             last_commit_time = excluded.last_commit_time,
             last_walk        = excluded.last_walk,
             last_fetch       = excluded.last_fetch",
        params![
            record.slug,
            record.org,
            record.name,
            path_str,
            record.branch,
            record.dirty as i64,
            record.ahead,
            record.behind,
            record.lang,
            record.last_commit_sha,
            record.last_commit_time,
            last_walk,
            record.last_fetch,
        ],
    )
    .context("failed to upsert repos row")?;

    // Replace this repo's deps: clear then re-insert, so a removed dep does not
    // linger across walks (Data Model idempotence rule).
    txn.execute(
        "DELETE FROM deps WHERE repo_slug = ?1",
        params![record.slug],
    )
    .context("failed to clear stale deps")?;

    let mut written = 0usize;
    for dep in &record.deps {
        // OR IGNORE: a manifest that names the same dep twice under the same
        // kind collapses to one row (the PK is repo_slug+ecosystem+name+kind).
        let n = txn
            .execute(
                "INSERT OR IGNORE INTO deps (repo_slug, ecosystem, name, version_req, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.slug,
                    dep.ecosystem,
                    dep.name,
                    dep.version_req,
                    dep.kind
                ],
            )
            .context("failed to insert dep row")?;
        written += n;
    }

    txn.commit().context("failed to commit repo txn")?;
    Ok(written)
}

/// Delete catalog rows for repos under `root` whose slug was not seen this walk.
/// Scoped to the walked subtree (canonical `root` prefix) so a partial walk
/// never prunes repos outside its scope. Deps cascade via `ON DELETE CASCADE`.
fn prune_missing(conn: &Connection, root: &Path, seen: &HashSet<String>) -> Result<usize> {
    let root_canon = canonical_path(root);
    let root_str = root_canon.to_string_lossy().to_string();

    // Match the repo itself (`path = root`) or anything strictly beneath it
    // (`root/%`). The trailing `/%` avoids the sibling-prefix bug where
    // `/repos/foo` would wrongly match `/repos/foobar`.
    let prefix = format!("{root_str}/%");
    let mut stmt = conn
        .prepare("SELECT slug FROM repos WHERE path = ?1 OR path LIKE ?2")
        .context("failed to prepare prune scan")?;
    let candidates: Vec<String> = stmt
        .query_map(params![root_str, prefix], |row| row.get::<_, String>(0))
        .context("failed to scan repos for prune")?
        .collect::<rusqlite::Result<Vec<String>>>()
        .context("failed to read prune candidates")?;

    let mut pruned = 0usize;
    for slug in candidates {
        if !seen.contains(&slug) {
            debug!("walk: pruning removed repo {slug}");
            conn.execute("DELETE FROM repos WHERE slug = ?1", params![slug])
                .context("failed to delete pruned repo")?;
            pruned += 1;
        }
    }
    Ok(pruned)
}

/// Map a LOCAL `RemoteStatus` to `(ahead, behind)` counts. Any state without a
/// tracking ref (no upstream, no remote, detached, error) yields `(None, None)`
/// -- the columns are nullable precisely for "no local tracking ref to read".
fn ahead_behind(status: &RemoteStatus) -> (Option<i64>, Option<i64>) {
    match status {
        RemoteStatus::UpToDate => (Some(0), Some(0)),
        RemoteStatus::Ahead(a) => (Some(*a as i64), Some(0)),
        RemoteStatus::Behind(b) => (Some(0), Some(*b as i64)),
        RemoteStatus::Diverged(a, b) => (Some(*a as i64), Some(*b as i64)),
        RemoteStatus::NoRemote
        | RemoteStatus::NoUpstream
        | RemoteStatus::DetachedHead
        | RemoteStatus::Error(_) => (None, None),
    }
}

/// Split a `<org>/<name>` slug. A slug with no `/` (should not happen given
/// `resolve_slug` always emits `x/name`) falls back to `unknown` org.
fn split_slug(slug: &str) -> (String, String) {
    match slug.split_once('/') {
        Some((org, name)) => (org.to_string(), name.to_string()),
        None => ("unknown".to_string(), slug.to_string()),
    }
}

/// Canonicalize a path, falling back to the input unchanged if it cannot be
/// resolved (e.g. a transient race where the repo vanished mid-walk).
fn canonical_path(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canon) => canon,
        Err(e) => {
            warn!(
                "canonical_path: could not canonicalize {} ({e}); storing as-is",
                path.display()
            );
            path.to_path_buf()
        }
    }
}

/// Read the last commit's full SHA and committer unix time via `git log -1`.
/// Returns `(None, None)` for an empty repo or on any git error (LOCAL only).
fn last_commit(repo_path: &Path) -> (Option<String>, Option<i64>) {
    let output = match run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "log",
            "-1",
            "--format=%H%x09%ct",
        ]),
        subprocess_timeout(),
    ) {
        Ok(o) if o.status.success() => o,
        Ok(_) => return (None, None),
        Err(e) => {
            debug!(
                "last_commit: git log failed for {}: {e}",
                repo_path.display()
            );
            return (None, None);
        }
    };

    let text = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(e) => {
            debug!("last_commit: non-utf8 git log output: {e}");
            return (None, None);
        }
    };
    let line = text.trim();
    let mut parts = line.split('\t');
    let sha = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
    let time = parts.next().and_then(|t| t.trim().parse::<i64>().ok());
    (sha, time)
}

/// Read the `FETCH_HEAD` mtime as a unix timestamp, resolving the actual git
/// dir via `git rev-parse --git-path` (so flat repos, worktrees, and bare
/// containers all work). `None` when the repo was never fetched (no FETCH_HEAD).
fn last_fetch_mtime(repo_path: &Path) -> Option<i64> {
    let output = run_checked(
        Command::new("git").args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--git-path",
            "FETCH_HEAD",
        ]),
        subprocess_timeout(),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let rel = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if rel.is_empty() {
        return None;
    }
    // `--git-path` may return a path relative to the repo working dir.
    let fetch_head = {
        let p = PathBuf::from(&rel);
        if p.is_absolute() {
            p
        } else {
            repo_path.join(p)
        }
    };
    let meta = std::fs::metadata(&fetch_head).ok()?;
    let mtime = meta.modified().ok()?;
    Some(system_time_to_unix(mtime))
}

/// Parse `Cargo.toml` (ecosystem `cargo`) and `package.json` (ecosystem `npm`)
/// for a primary-language guess and the dependency list. A repo may have both;
/// the language guess prefers Rust, then TypeScript (a `tsconfig.json` beside
/// `package.json`), then JavaScript.
fn parse_manifests(repo_path: &Path) -> (Option<String>, Vec<DepRecord>) {
    let mut deps = Vec::new();
    let mut lang: Option<String> = None;

    let cargo_toml = repo_path.join("Cargo.toml");
    if cargo_toml.is_file() {
        if let Ok(text) = std::fs::read_to_string(&cargo_toml) {
            match parse_cargo_deps(&text) {
                Ok(mut cargo_deps) => {
                    lang = Some("rust".to_string());
                    deps.append(&mut cargo_deps);
                }
                Err(e) => warn!(
                    "parse_manifests: Cargo.toml parse failed at {}: {e}",
                    cargo_toml.display()
                ),
            }
        }
    }

    let package_json = repo_path.join("package.json");
    if package_json.is_file() {
        if let Ok(text) = std::fs::read_to_string(&package_json) {
            match parse_npm_deps(&text) {
                Ok(mut npm_deps) => {
                    if lang.is_none() {
                        lang = Some(if repo_path.join("tsconfig.json").is_file() {
                            "typescript".to_string()
                        } else {
                            "javascript".to_string()
                        });
                    }
                    deps.append(&mut npm_deps);
                }
                Err(e) => warn!(
                    "parse_manifests: package.json parse failed at {}: {e}",
                    package_json.display()
                ),
            }
        }
    }

    (lang, deps)
}

/// Parse the three Cargo dependency sections into `DepRecord`s. Each entry is
/// either a bare version string or a table (`{ version = "..", ... }`); a
/// git/path/workspace dep carries no `version` and stores `version_req = None`.
fn parse_cargo_deps(text: &str) -> Result<Vec<DepRecord>> {
    let table: toml::Table = toml::from_str(text).context("invalid Cargo.toml")?;
    let mut deps = Vec::new();
    for (section, kind) in [
        ("dependencies", "normal"),
        ("dev-dependencies", "dev"),
        ("build-dependencies", "build"),
    ] {
        let Some(toml::Value::Table(section_table)) = table.get(section) else {
            continue;
        };
        for (name, value) in section_table {
            let version_req = match value {
                toml::Value::String(s) => Some(s.clone()),
                toml::Value::Table(t) => t
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                _ => None,
            };
            deps.push(DepRecord {
                ecosystem: "cargo".to_string(),
                name: name.clone(),
                version_req,
                kind: kind.to_string(),
            });
        }
    }
    Ok(deps)
}

/// Parse `package.json` `dependencies` (normal) and `devDependencies` (dev).
/// npm has no build-dependency concept, so `build` never appears for npm.
fn parse_npm_deps(text: &str) -> Result<Vec<DepRecord>> {
    let root: serde_json::Value = serde_json::from_str(text).context("invalid package.json")?;
    let mut deps = Vec::new();
    for (section, kind) in [("dependencies", "normal"), ("devDependencies", "dev")] {
        let Some(serde_json::Value::Object(map)) = root.get(section) else {
            continue;
        };
        for (name, value) in map {
            let version_req = value.as_str().map(str::to_string);
            deps.push(DepRecord {
                ecosystem: "npm".to_string(),
                name: name.clone(),
                version_req,
                kind: kind.to_string(),
            });
        }
    }
    Ok(deps)
}

fn now_unix() -> i64 {
    system_time_to_unix(SystemTime::now())
}

fn system_time_to_unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
