//! Catalog DB open + migrate (design doc `2026-07-17-gx-intel-catalog.md`,
//! Data Model). Opens `$XDG_CACHE_HOME/gx/catalog.db` with WAL, a named
//! busy-timeout, `synchronous=NORMAL`, and `PRAGMA foreign_keys=ON` per
//! connection (SQLite does not persist `foreign_keys` in the file -- it must
//! be set on every connection or `ON DELETE CASCADE` is silently inert). The
//! `repos`/`deps` DDL is idempotent and runs inside one transaction guarded by
//! `PRAGMA user_version`, so a re-open/re-migrate of an up-to-date DB is a
//! no-op.

use eyre::{Context, Result};
use log::debug;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::Duration;

use local::config::xdg_cache_dir;

/// How long a writer waits on `SQLITE_BUSY` before erroring, when a
/// stale-triggered auto-walk (Phase 3) briefly contends with a CLI `gx
/// catalog` run (design doc "Writer concurrency" edge case). Named per the
/// rusqlite house rule (rust.md) rather than an inline literal.
pub const BUSY_TIMEOUT_MS: u64 = 5_000;

/// Schema version this build expects. Bump alongside new migration DDL and
/// add the new statements to `migrate` guarded by the prior version.
const SCHEMA_VERSION: i64 = 1;

/// Resolve `$XDG_CACHE_HOME/gx/catalog.db`, creating the parent directory if
/// it does not already exist. The catalog is a rebuildable cache, not
/// operator config or durable state (taste.md "cache in ~/.cache not
/// ~/.config"), which is why it lives under the cache dir and not
/// `xdg_data_dir()` (where `changes/` -- durable undo state -- lives).
pub fn catalog_db_path() -> Result<PathBuf> {
    let cache_dir = xdg_cache_dir()
        .ok_or_else(|| eyre::eyre!("cannot resolve cache dir (set $HOME or $XDG_CACHE_HOME)"))?;
    let dir = cache_dir.join("gx");
    std::fs::create_dir_all(&dir).context(format!(
        "Failed to create catalog cache dir {}",
        dir.display()
    ))?;
    Ok(dir.join("catalog.db"))
}

/// Open (creating if absent) the catalog DB at `path`, apply the
/// per-connection pragmas, and run the idempotent migration.
pub fn open(path: &Path) -> Result<Connection> {
    debug!("catalog::db::open: path={path:?}");
    let conn = Connection::open(path)
        .context(format!("Failed to open catalog db at {}", path.display()))?;

    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
        .context("Failed to set busy_timeout")?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("Failed to set journal_mode=WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .context("Failed to set synchronous=NORMAL")?;
    // Per-connection: SQLite does NOT persist this pragma in the file, so it
    // must be re-applied on every open or `ON DELETE CASCADE` is inert.
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("Failed to set foreign_keys=ON")?;

    migrate(&conn)?;
    Ok(conn)
}

/// Open the catalog DB at the default `$XDG_CACHE_HOME/gx/catalog.db` path.
pub fn open_default() -> Result<Connection> {
    let path = catalog_db_path()?;
    open(&path)
}

/// Idempotent migration: no-op when `user_version` is already at
/// `SCHEMA_VERSION`; otherwise runs the DDL and the version bump inside one
/// transaction, so a crash mid-migration cannot brick the DB at a half-applied
/// schema (rust.md "one txn migration").
fn migrate(conn: &Connection) -> Result<()> {
    let current_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("Failed to read user_version")?;

    if current_version >= SCHEMA_VERSION {
        debug!("catalog::db::migrate: already at version {current_version}, no-op");
        return Ok(());
    }

    debug!("catalog::db::migrate: migrating from version {current_version} to {SCHEMA_VERSION}");
    conn.execute_batch(&format!(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS repos (
           slug             TEXT PRIMARY KEY,
           org              TEXT NOT NULL,
           name             TEXT NOT NULL,
           path             TEXT NOT NULL,
           branch           TEXT,
           dirty            INTEGER NOT NULL,
           ahead            INTEGER,
           behind           INTEGER,
           lang             TEXT,
           last_commit_sha  TEXT,
           last_commit_time INTEGER,
           last_walk        INTEGER NOT NULL,
           last_fetch       INTEGER
         );

         CREATE TABLE IF NOT EXISTS deps (
           repo_slug   TEXT NOT NULL REFERENCES repos(slug) ON DELETE CASCADE,
           ecosystem   TEXT NOT NULL,
           name        TEXT NOT NULL,
           version_req TEXT,
           kind        TEXT NOT NULL,
           PRIMARY KEY (repo_slug, ecosystem, name, kind)
         );
         CREATE INDEX IF NOT EXISTS deps_name ON deps(name);
         CREATE INDEX IF NOT EXISTS deps_repo ON deps(repo_slug);

         PRAGMA user_version = {SCHEMA_VERSION};
         COMMIT;"
    ))
    .context("Failed to run catalog migration")?;

    Ok(())
}

#[cfg(test)]
mod tests;
