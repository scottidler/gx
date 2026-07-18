use super::*;
use local::test_utils::env_lock;
use rusqlite::params;
use tempfile::TempDir;

/// Every column the Data Model SQL specifies for `repos`, in declaration order.
const REPOS_COLUMNS: &[&str] = &[
    "slug",
    "org",
    "name",
    "path",
    "branch",
    "dirty",
    "ahead",
    "behind",
    "lang",
    "last_commit_sha",
    "last_commit_time",
    "last_walk",
    "last_fetch",
];

/// Every column the Data Model SQL specifies for `deps`, in declaration order.
const DEPS_COLUMNS: &[&str] = &["repo_slug", "ecosystem", "name", "version_req", "kind"];

fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// (1) DB opens under `$XDG_CACHE_HOME/gx/` -- success criterion #1.
#[test]
fn test_catalog_db_path_resolves_under_xdg_cache_home() {
    let guard = env_lock();
    let prior = std::env::var("XDG_CACHE_HOME").ok();

    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path()) };

    let path = catalog_db_path().unwrap();
    assert_eq!(path, dir.path().join("gx").join("catalog.db"));

    let conn = open(&path).unwrap();
    assert!(path.exists());
    drop(conn);

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_CACHE_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CACHE_HOME") },
    }
    drop(guard);
}

/// `open_default` resolves the same `$XDG_CACHE_HOME/gx/catalog.db` path and
/// succeeds end to end (path resolution + open + migrate).
#[test]
fn test_open_default_uses_xdg_cache_home() {
    let guard = env_lock();
    let prior = std::env::var("XDG_CACHE_HOME").ok();

    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path()) };

    let conn = open_default().unwrap();
    drop(conn);
    assert!(dir.path().join("gx").join("catalog.db").exists());

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_CACHE_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CACHE_HOME") },
    }
    drop(guard);
}

/// (2) Re-running the migration (a fresh `open` of the same file, and a direct
/// re-call of `migrate`) is a no-op: `user_version` stays at `SCHEMA_VERSION`
/// and no error is raised.
#[test]
fn test_migration_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("catalog.db");

    let conn = open(&path).unwrap();
    let version_after_first: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version_after_first, SCHEMA_VERSION);

    // Re-running `migrate` directly on the same connection must not error and
    // must leave the version and schema untouched.
    migrate(&conn).unwrap();
    let version_after_second: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version_after_second, SCHEMA_VERSION);
    drop(conn);

    // Re-opening the same DB file (fresh connection) is likewise a no-op.
    let reopened = open(&path).unwrap();
    let version_after_reopen: i64 = reopened
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version_after_reopen, SCHEMA_VERSION);
}

/// (3) `repos` carries every column in the Data Model, and `deps` carries
/// every column too.
#[test]
fn test_repos_and_deps_tables_have_every_column() {
    let dir = TempDir::new().unwrap();
    let conn = open(&dir.path().join("catalog.db")).unwrap();

    let repos_cols = table_columns(&conn, "repos");
    for col in REPOS_COLUMNS {
        assert!(
            repos_cols.iter().any(|c| c == col),
            "repos table missing column {col}, has {repos_cols:?}"
        );
    }

    let deps_cols = table_columns(&conn, "deps");
    for col in DEPS_COLUMNS {
        assert!(
            deps_cols.iter().any(|c| c == col),
            "deps table missing column {col}, has {deps_cols:?}"
        );
    }
}

/// `PRAGMA foreign_keys=ON` is applied per connection, so `ON DELETE CASCADE`
/// actually fires: deleting a `repos` row removes its `deps` rows. This bites
/// the pragma -- without it (verified by commenting out the pragma_update
/// during development), the delete leaves the `deps` row orphaned instead of
/// erroring or cascading.
#[test]
fn test_foreign_keys_cascade_deletes_deps_on_repo_delete() {
    let dir = TempDir::new().unwrap();
    let conn = open(&dir.path().join("catalog.db")).unwrap();

    conn.execute(
        "INSERT INTO repos (slug, org, name, path, dirty, last_walk)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "acme/widgets",
            "acme",
            "widgets",
            "/repos/acme/widgets",
            0,
            1_700_000_000i64
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO deps (repo_slug, ecosystem, name, version_req, kind)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params!["acme/widgets", "cargo", "serde", "1.0", "normal"],
    )
    .unwrap();

    let dep_count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM deps WHERE repo_slug = ?1",
            params!["acme/widgets"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dep_count_before, 1);

    conn.execute("DELETE FROM repos WHERE slug = ?1", params!["acme/widgets"])
        .unwrap();

    let dep_count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM deps WHERE repo_slug = ?1",
            params!["acme/widgets"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        dep_count_after, 0,
        "ON DELETE CASCADE should have removed the dep row"
    );
}

/// A negative counterpart to the cascade test: inserting a `deps` row whose
/// `repo_slug` names no `repos` row is rejected by the foreign key, proving
/// `foreign_keys=ON` is genuinely enforced (not just present in the DDL).
#[test]
fn test_foreign_keys_reject_dangling_dep_insert() {
    let dir = TempDir::new().unwrap();
    let conn = open(&dir.path().join("catalog.db")).unwrap();

    let result = conn.execute(
        "INSERT INTO deps (repo_slug, ecosystem, name, version_req, kind)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params!["no-such/repo", "cargo", "serde", "1.0", "normal"],
    );
    assert!(
        result.is_err(),
        "inserting a dep for a nonexistent repo should fail the foreign key constraint"
    );
}
