use super::*;
use crate::db;
use local::test_utils::create_minimal_test_repo;
use std::fs;
use tempfile::TempDir;

/// Open a fresh catalog DB in a temp dir (no `$XDG_CACHE_HOME` dependency).
fn temp_db(dir: &TempDir) -> Connection {
    db::open(&dir.path().join("catalog.db")).unwrap()
}

fn run_git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo_row_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))
        .unwrap()
}

fn dep_row_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM deps", [], |r| r.get(0))
        .unwrap()
}

/// (1) The walk populates N repos with ZERO `git fetch` calls. Proof: both
/// repos point `origin` at an UNREACHABLE URL (an RFC-5737 TEST-NET host that
/// never resolves/connects); any fetch would hang or error. The walk still
/// succeeds and indexes both, because `get_repo_status_local` + FETCH_HEAD
/// mtime touch only local state. FETCH_HEAD is absent, so `last_fetch` is NULL.
#[test]
fn test_walk_is_zero_fetch_and_indexes_all_repos() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    for name in ["alpha", "beta"] {
        let path = create_minimal_test_repo(root.path(), name);
        // Repoint origin at an unreachable host: a fetch would fail, a local
        // walk cannot.
        run_git(
            &["remote", "set-url", "origin", "https://192.0.2.1/nope.git"],
            &path,
        );
    }

    let summary = walk(&mut conn, root.path(), 3, &[]).unwrap();
    assert_eq!(summary.repos_indexed, 2);
    assert_eq!(repo_row_count(&conn), 2);

    // No repo was ever fetched -> FETCH_HEAD absent -> last_fetch NULL for both.
    let null_fetches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM repos WHERE last_fetch IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        null_fetches, 2,
        "walk must never fetch, so last_fetch stays NULL"
    );

    // last_walk and last_commit are populated from local state.
    let walked: i64 = conn
        .query_row("SELECT COUNT(*) FROM repos WHERE last_walk > 0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(walked, 2);
    let with_commit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM repos WHERE last_commit_sha IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(with_commit, 2, "each repo has an initial commit");
}

/// (2a) A re-walk is idempotent: running twice over the same tree leaves the
/// same row counts (no duplicate repos, no duplicate deps).
#[test]
fn test_rewalk_is_idempotent() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    let path = create_minimal_test_repo(root.path(), "svc");
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nserde = \"1.0\"\n\n[dev-dependencies]\ntempfile = \"3\"\n",
    )
    .unwrap();

    let first = walk(&mut conn, root.path(), 3, &[]).unwrap();
    let repos_1 = repo_row_count(&conn);
    let deps_1 = dep_row_count(&conn);

    let second = walk(&mut conn, root.path(), 3, &[]).unwrap();
    assert_eq!(repo_row_count(&conn), repos_1, "repo count must be stable");
    assert_eq!(dep_row_count(&conn), deps_1, "dep count must be stable");
    assert_eq!(first.repos_indexed, second.repos_indexed);
    assert_eq!(first.deps_indexed, second.deps_indexed);
    assert_eq!(repos_1, 1);
    assert_eq!(deps_1, 2, "serde (normal) + tempfile (dev)");
}

/// (2b) Pruning a removed repo clears BOTH its `repos` row AND its `deps` rows.
#[test]
fn test_prune_removes_repo_and_deps() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    let keep = create_minimal_test_repo(root.path(), "keeper");
    let drop_path = create_minimal_test_repo(root.path(), "goner");
    for p in [&keep, &drop_path] {
        fs::write(
            p.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\nlog = \"0.4\"\n",
        )
        .unwrap();
    }

    walk(&mut conn, root.path(), 3, &[]).unwrap();
    assert_eq!(repo_row_count(&conn), 2);
    assert_eq!(dep_row_count(&conn), 2);

    // Remove one repo from disk, then re-walk.
    fs::remove_dir_all(&drop_path).unwrap();
    let summary = walk(&mut conn, root.path(), 3, &[]).unwrap();
    assert_eq!(summary.pruned, 1);
    assert_eq!(repo_row_count(&conn), 1, "goner repo row pruned");
    assert_eq!(
        dep_row_count(&conn),
        1,
        "goner's dep row cascaded away, keeper's remains"
    );

    let remaining: String = conn
        .query_row("SELECT slug FROM repos", [], |r| r.get(0))
        .unwrap();
    assert!(remaining.ends_with("/keeper"));
}

/// (3) Dep rows link repo <-> dependency in BOTH directions: a query by
/// `repo_slug` and a query by dep `name` both resolve.
#[test]
fn test_deps_link_both_directions() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    let a = create_minimal_test_repo(root.path(), "aaa");
    let b = create_minimal_test_repo(root.path(), "bbb");
    fs::write(
        a.join("Cargo.toml"),
        "[package]\nname=\"aaa\"\nversion=\"0.1.0\"\n\n[dependencies]\nserde=\"1.0\"\nlog=\"0.4\"\n",
    )
    .unwrap();
    fs::write(
        b.join("package.json"),
        r#"{"name":"bbb","dependencies":{"serde":"^1","react":"^18"},"devDependencies":{"jest":"^29"}}"#,
    )
    .unwrap();

    walk(&mut conn, root.path(), 3, &[]).unwrap();

    // Direction 1: query BY repo_slug -> that repo's deps.
    let aaa_slug: String = conn
        .query_row("SELECT slug FROM repos WHERE name = 'aaa'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM deps WHERE repo_slug = ?1 ORDER BY name")
        .unwrap();
    let aaa_deps: Vec<String> = stmt
        .query_map(params![aaa_slug], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(aaa_deps, vec!["log".to_string(), "serde".to_string()]);

    // Direction 2: query BY dep name -> repos using it. `serde` is used by both
    // the cargo repo and the npm repo.
    let mut stmt = conn
        .prepare("SELECT DISTINCT repo_slug FROM deps WHERE name = ?1 ORDER BY repo_slug")
        .unwrap();
    let serde_users: Vec<String> = stmt
        .query_map(params!["serde"], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(serde_users.len(), 2, "serde is used by both repos");

    // npm dev dep is recorded with kind=dev; cargo build/dev kinds distinct.
    let jest_kind: String = conn
        .query_row("SELECT kind FROM deps WHERE name = 'jest'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(jest_kind, "dev");
    let jest_eco: String = conn
        .query_row("SELECT ecosystem FROM deps WHERE name = 'jest'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(jest_eco, "npm");
}

/// A dirty repo is recorded `dirty = 1`, and `ahead`/`behind` come from LOCAL
/// tracking refs (a fresh repo with an unreachable origin and no upstream ->
/// NULL, not a fetched value).
#[test]
fn test_dirty_flag_and_local_ahead_behind() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    let path = create_minimal_test_repo(root.path(), "dirtyrepo");
    fs::write(path.join("scratch.txt"), "uncommitted").unwrap();

    walk(&mut conn, root.path(), 3, &[]).unwrap();

    let dirty: i64 = conn
        .query_row(
            "SELECT dirty FROM repos WHERE name = 'dirtyrepo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dirty, 1, "an untracked file makes the repo dirty");

    // No upstream tracking ref -> ahead/behind NULL (never fetched).
    let ahead: Option<i64> = conn
        .query_row(
            "SELECT ahead FROM repos WHERE name = 'dirtyrepo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ahead, None);
}

/// Manifest parsing unit tests, isolated from the DB.
#[test]
fn test_parse_cargo_deps_kinds_and_versions() {
    let text = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
serde = "1.0"
local = { path = "../local" }
tokio = { version = "1", features = ["full"] }

[dev-dependencies]
tempfile = "3"

[build-dependencies]
cc = "1.0"
"#;
    let mut deps = parse_cargo_deps(text).unwrap();
    deps.sort_by_key(|d| (d.kind.clone(), d.name.clone()));

    // build: cc
    let cc = deps.iter().find(|d| d.name == "cc").unwrap();
    assert_eq!(cc.kind, "build");
    assert_eq!(cc.version_req.as_deref(), Some("1.0"));
    // dev: tempfile
    let tf = deps.iter().find(|d| d.name == "tempfile").unwrap();
    assert_eq!(tf.kind, "dev");
    // normal: serde string form, tokio table form, local path (no version)
    let serde = deps.iter().find(|d| d.name == "serde").unwrap();
    assert_eq!(serde.kind, "normal");
    assert_eq!(serde.version_req.as_deref(), Some("1.0"));
    let tokio = deps.iter().find(|d| d.name == "tokio").unwrap();
    assert_eq!(tokio.version_req.as_deref(), Some("1"));
    let localdep = deps.iter().find(|d| d.name == "local").unwrap();
    assert_eq!(localdep.version_req, None, "a path dep carries no version");
    assert!(deps.iter().all(|d| d.ecosystem == "cargo"));
}

#[test]
fn test_parse_npm_deps_normal_and_dev() {
    let text = r#"{
        "name": "web",
        "dependencies": { "react": "^18.0.0" },
        "devDependencies": { "jest": "^29.0.0", "typescript": "^5" }
    }"#;
    let deps = parse_npm_deps(text).unwrap();
    let react = deps.iter().find(|d| d.name == "react").unwrap();
    assert_eq!(react.kind, "normal");
    assert_eq!(react.ecosystem, "npm");
    assert_eq!(react.version_req.as_deref(), Some("^18.0.0"));
    assert!(deps.iter().any(|d| d.name == "jest" && d.kind == "dev"));
    assert!(deps
        .iter()
        .any(|d| d.name == "typescript" && d.kind == "dev"));
}

#[test]
fn test_split_slug() {
    assert_eq!(
        split_slug("scottidler/gx"),
        ("scottidler".to_string(), "gx".to_string())
    );
    assert_eq!(
        split_slug("noorg"),
        ("unknown".to_string(), "noorg".to_string())
    );
}

/// (Phase 4, auto-walk-on-stale, criterion 5) An UNBUILT catalog on an MCP
/// `query`/`search` path walks first and returns real rows -- never
/// empty-as-success. `ensure_fresh` on an empty DB reports it walked, and the
/// rows are present afterward.
#[test]
fn test_ensure_fresh_walks_unbuilt_catalog() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    create_minimal_test_repo(root.path(), "alpha");
    create_minimal_test_repo(root.path(), "beta");

    // Empty DB: nothing indexed yet.
    assert_eq!(repo_row_count(&conn), 0);

    let walked = ensure_fresh(&mut conn, root.path(), Some(root.path()), 3, &[], 3600).unwrap();
    assert!(walked, "an unbuilt catalog must trigger a walk");
    assert_eq!(
        repo_row_count(&conn),
        2,
        "auto-walk populated real rows, not empty-as-success"
    );
}

/// (Phase 4, criterion 5) The auto-walk is LOCAL only: it must NOT fetch. Proof:
/// repos point `origin` at an unreachable host (a fetch would fail/hang), yet
/// `ensure_fresh` indexes them and NO FETCH_HEAD is written -> `last_fetch` NULL
/// for every row.
#[test]
fn test_ensure_fresh_never_fetches() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    for name in ["one", "two"] {
        let path = create_minimal_test_repo(root.path(), name);
        run_git(
            &["remote", "set-url", "origin", "https://192.0.2.1/nope.git"],
            &path,
        );
        // A repo that was never fetched has no FETCH_HEAD.
        assert!(
            !path.join(".git").join("FETCH_HEAD").exists(),
            "precondition: FETCH_HEAD absent before any walk"
        );
    }

    let walked = ensure_fresh(&mut conn, root.path(), Some(root.path()), 3, &[], 3600).unwrap();
    assert!(walked);

    // The auto-walk path writes zero FETCH_HEAD files, so last_fetch stays NULL.
    let null_fetches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM repos WHERE last_fetch IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        null_fetches, 2,
        "auto-walk is local-only: no FETCH_HEAD, so last_fetch is NULL"
    );
    for name in ["one", "two"] {
        assert!(
            !root
                .path()
                .join(name)
                .join(".git")
                .join("FETCH_HEAD")
                .exists(),
            "auto-walk must not create FETCH_HEAD for {name}"
        );
    }
}

/// (Phase 4, criterion 5) FRESH rows do NOT trigger a re-walk: a second
/// `ensure_fresh` inside the staleness window is a no-op (returns `false`).
#[test]
fn test_ensure_fresh_skips_when_fresh() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    create_minimal_test_repo(root.path(), "svc");

    // First call builds the catalog.
    assert!(ensure_fresh(&mut conn, root.path(), Some(root.path()), 3, &[], 3600).unwrap());
    // Second call, well inside a 1h window: rows are fresh, no walk.
    let walked = ensure_fresh(&mut conn, root.path(), Some(root.path()), 3, &[], 3600).unwrap();
    assert!(!walked, "fresh rows must not trigger a walk");
}

/// (Phase 4, criterion 5) STALE rows DO trigger a LOCAL re-walk: age the oldest
/// row past the staleness window and confirm `ensure_fresh` walks again.
#[test]
fn test_ensure_fresh_walks_when_stale() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    create_minimal_test_repo(root.path(), "svc");
    assert!(ensure_fresh(&mut conn, root.path(), Some(root.path()), 3, &[], 3600).unwrap());

    // Backdate the walk stamp far into the past so it exceeds any small window.
    conn.execute("UPDATE repos SET last_walk = 1", []).unwrap();

    let walked = ensure_fresh(&mut conn, root.path(), Some(root.path()), 3, &[], 3600).unwrap();
    assert!(
        walked,
        "a row older than staleness_secs must trigger a walk"
    );

    // The re-walk refreshed last_walk to ~now (well past the backdated 1).
    let last_walk: i64 = conn
        .query_row("SELECT last_walk FROM repos", [], |r| r.get(0))
        .unwrap();
    assert!(last_walk > 1, "re-walk refreshed the staleness stamp");
}

/// A subtree walk must NOT prune repos that live OUTSIDE the walked root: prune
/// is scoped to the canonical `root` prefix (and the trailing `/%` avoids the
/// sibling-prefix bug).
#[test]
fn test_prune_is_scoped_to_walked_root() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    // Two sibling subtrees under the same parent.
    let sub_a = root.path().join("orga");
    let sub_b = root.path().join("orgb");
    fs::create_dir_all(&sub_a).unwrap();
    fs::create_dir_all(&sub_b).unwrap();
    create_minimal_test_repo(&sub_a, "one");
    create_minimal_test_repo(&sub_b, "two");

    // Index the whole tree.
    walk(&mut conn, root.path(), 4, &[]).unwrap();
    assert_eq!(repo_row_count(&conn), 2);

    // Re-walk ONLY sub_a: its repo is still there, so nothing prunes, and
    // sub_b's repo (outside the walked root) must survive untouched.
    let summary = walk(&mut conn, &sub_a, 3, &[]).unwrap();
    assert_eq!(
        summary.pruned, 0,
        "sub_b is out of scope; must not be pruned"
    );
    assert_eq!(repo_row_count(&conn), 2, "both repos remain");
}
