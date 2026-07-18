use crate::db;
use crate::tools::deps::{deps, DepsResult};
use crate::tools::query::{query, QueryFilter};
use crate::tools::read::read;
use crate::tools::search::search;
use crate::tools::{clamp_root, Bounds};
use crate::walk::walk;
use local::test_utils::create_minimal_test_repo;
use rusqlite::Connection;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

fn temp_conn(dir: &TempDir) -> Connection {
    db::open(&dir.path().join("catalog.db")).unwrap()
}

/// Create a minimal git repo at `base/dir` and repoint origin at `slug` so the
/// walk records `slug`/`org` from the remote (not the temp directory name).
fn make_repo(base: &Path, dir: &str, slug: &str) -> PathBuf {
    let path = create_minimal_test_repo(base, dir);
    let out = Command::new("git")
        .args([
            "remote",
            "set-url",
            "origin",
            &format!("git@github.com:{slug}.git"),
        ])
        .current_dir(&path)
        .output()
        .expect("git remote set-url");
    assert!(out.status.success());
    path
}

fn no_filter() -> QueryFilter {
    QueryFilter::default()
}

/// (1) Under a valid clamped root, `query` returns exactly the in-scope rows.
#[test]
fn test_query_returns_only_in_scope_rows() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    let foo = root.path().join("foo");
    let foobar = root.path().join("foobar");
    std::fs::create_dir_all(&foo).unwrap();
    std::fs::create_dir_all(&foobar).unwrap();
    make_repo(&foo, "svc", "orga/svc");
    make_repo(&foobar, "svc", "orgb/svc");

    walk(&mut conn, root.path(), 5, &[]).unwrap();

    // Whole-tree query sees both.
    let all = query(
        &conn,
        root.path(),
        Some(root.path()),
        &no_filter(),
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(all.rows.len(), 2);
    assert!(!all.truncated);

    // (2) Sibling-prefix bite: a query rooted at `foo` must NOT pick up the repo
    // under the sibling `foobar` (the trailing `/%` in the scope predicate).
    let scoped = query(
        &conn,
        root.path(),
        Some(&foo),
        &no_filter(),
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(scoped.rows.len(), 1, "only the repo under foo, not foobar");
    let foo_canon = foo.canonicalize().unwrap();
    assert!(Path::new(&scoped.rows[0].path).starts_with(&foo_canon));
}

/// (1) `query` filters (org here) narrow the rows.
#[test]
fn test_query_org_filter() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    make_repo(root.path(), "a", "orga/a");
    make_repo(root.path(), "b", "orgb/b");
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    let filter = QueryFilter {
        org: Some("orgb".to_string()),
        ..Default::default()
    };
    let out = query(
        &conn,
        root.path(),
        Some(root.path()),
        &filter,
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].org, "orgb");
}

/// (2) The clamp BITES on every escape variant: sibling-prefix is covered above;
/// here `..`, symlink escape, out-of-root, and non-existent all fail closed.
#[test]
fn test_clamp_rejects_escapes() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();

    // `..` escapes the ceiling -> rejected.
    let dotdot = root.path().join("..");
    assert!(
        clamp_root(root.path(), Some(&dotdot)).is_err(),
        ".. must escape the ceiling and be rejected"
    );

    // Symlink escape: a link inside the root pointing outside -> rejected.
    let link = root.path().join("escape");
    symlink(outside.path(), &link).unwrap();
    assert!(
        clamp_root(root.path(), Some(&link)).is_err(),
        "a symlink resolving outside the ceiling must be rejected"
    );

    // A wholly out-of-root root -> rejected.
    assert!(
        clamp_root(root.path(), Some(outside.path())).is_err(),
        "an out-of-root root must be rejected"
    );

    // A non-existent root cannot canonicalize -> rejected (never widened).
    let missing = root.path().join("does-not-exist");
    assert!(
        clamp_root(root.path(), Some(&missing)).is_err(),
        "a non-existent root must be rejected, not emptied"
    );

    // Sanity: the root itself, and a real child, are accepted.
    assert!(clamp_root(root.path(), Some(root.path())).is_ok());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    assert!(clamp_root(root.path(), Some(&child)).is_ok());
}

/// (2) The sibling-prefix bite at the CLAMP layer too: `/repos/foo` must not be
/// accepted as being "inside" a ceiling of `/repos/foobar` (or vice-versa).
#[test]
fn test_clamp_sibling_prefix_not_inside() {
    let base = TempDir::new().unwrap();
    let foo = base.path().join("foo");
    let foobar = base.path().join("foobar");
    std::fs::create_dir_all(&foo).unwrap();
    std::fs::create_dir_all(&foobar).unwrap();
    // Ceiling = foo; requested = foobar (a sibling sharing the `foo` prefix).
    assert!(
        clamp_root(&foo, Some(&foobar)).is_err(),
        "foobar shares a string prefix with foo but is a sibling; must be rejected"
    );
}

/// (3) An oversized result truncates with `truncated: true` -- by count AND by
/// bytes.
#[test]
fn test_query_output_bounds_truncate() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    make_repo(root.path(), "a", "orga/a");
    make_repo(root.path(), "b", "orgb/b");
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    // Count cap.
    let by_count = Bounds {
        max_results: 1,
        max_bytes: usize::MAX,
    };
    let out = query(
        &conn,
        root.path(),
        Some(root.path()),
        &no_filter(),
        &by_count,
    )
    .unwrap();
    assert_eq!(out.rows.len(), 1);
    assert!(out.truncated, "count cap must set truncated");

    // Byte cap (still returns at least one row).
    let by_bytes = Bounds {
        max_results: usize::MAX,
        max_bytes: 1,
    };
    let out = query(
        &conn,
        root.path(),
        Some(root.path()),
        &no_filter(),
        &by_bytes,
    )
    .unwrap();
    assert_eq!(out.rows.len(), 1);
    assert!(out.truncated, "byte cap must set truncated");
}

/// (5) A cross-org read succeeds (intel reads span orgs, subtree-scoped), and
/// (4) a non-utf8 file errors clearly rather than reporting empty success.
#[test]
fn test_read_cross_org_and_non_utf8() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    make_repo(root.path(), "a", "orga/a");
    let repo_b = make_repo(root.path(), "b", "orgb/b");
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    // (5) Read a file in org B (a different org than A) -- succeeds.
    let out = read(
        &conn,
        root.path(),
        "orgb/b",
        "README.md",
        None,
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(out.content, "# b");
    assert!(!out.truncated);

    // (4) A binary (non-utf8) file errors clearly.
    std::fs::write(repo_b.join("bin.dat"), [0xff, 0xfe, 0x00, 0xff]).unwrap();
    let err = read(
        &conn,
        root.path(),
        "orgb/b",
        "bin.dat",
        None,
        &Bounds::default(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not valid UTF-8"),
        "non-utf8 read must error clearly, got: {err}"
    );
}

/// (3)/(read) An oversized whole-file read is refused loudly unless a line range
/// is given; a bounded line range reads a slice.
#[test]
fn test_read_oversized_requires_line_range() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    let repo = make_repo(root.path(), "big", "orga/big");
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    // Ten lines; a tiny byte cap makes the whole file "oversized".
    let body = (1..=10)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(repo.join("data.txt"), &body).unwrap();
    let tiny = Bounds {
        max_results: usize::MAX,
        max_bytes: 8,
    };

    let err = read(&conn, root.path(), "orga/big", "data.txt", None, &tiny).unwrap_err();
    assert!(
        err.to_string().contains("request a bounded line range"),
        "oversized whole-file read must be refused loudly, got: {err}"
    );

    // A bounded line range is allowed (and may itself be byte-truncated).
    let out = read(
        &conn,
        root.path(),
        "orga/big",
        "data.txt",
        Some((2, 3)),
        &Bounds::default(),
    )
    .unwrap();
    assert_eq!(out.content, "line 2\nline 3");
    assert_eq!(out.line_start, Some(2));
    assert_eq!(out.line_end, Some(3));
}

/// `read` clamps the path inside the repo: `..` and absolute paths are rejected.
#[test]
fn test_read_path_clamp() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    make_repo(root.path(), "a", "orga/a");
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    assert!(
        read(
            &conn,
            root.path(),
            "orga/a",
            "../../etc/passwd",
            None,
            &Bounds::default()
        )
        .is_err(),
        ".. must be rejected by the path clamp"
    );
    assert!(
        read(
            &conn,
            root.path(),
            "orga/a",
            "/etc/passwd",
            None,
            &Bounds::default()
        )
        .is_err(),
        "an absolute path must be rejected"
    );
    // An unknown slug fails loud, not empty-success.
    assert!(read(
        &conn,
        root.path(),
        "nope/nope",
        "README.md",
        None,
        &Bounds::default()
    )
    .is_err());
}

/// `deps` resolves both directions and stays scope-clamped.
#[test]
fn test_deps_both_directions() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    let a = make_repo(root.path(), "a", "orga/a");
    let b = make_repo(root.path(), "b", "orgb/b");
    std::fs::write(
        a.join("Cargo.toml"),
        "[package]\nname=\"a\"\nversion=\"0.1.0\"\n\n[dependencies]\nserde=\"1.0\"\nlog=\"0.4\"\n",
    )
    .unwrap();
    std::fs::write(
        b.join("Cargo.toml"),
        "[package]\nname=\"b\"\nversion=\"0.1.0\"\n\n[dependencies]\nserde=\"1.0\"\n",
    )
    .unwrap();
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    // Direction 1: who uses `serde`? both repos.
    let by_dep = deps(&conn, root.path(), Some("serde"), None, &Bounds::default()).unwrap();
    match by_dep {
        DepsResult::ByDependency {
            dependency, repos, ..
        } => {
            assert_eq!(dependency, "serde");
            let slugs: Vec<_> = repos.iter().map(|r| r.slug.as_str()).collect();
            assert!(slugs.contains(&"orga/a"));
            assert!(slugs.contains(&"orgb/b"));
        }
        other => panic!("expected ByDependency, got {other:?}"),
    }

    // Direction 2: what does `orga/a` depend on? serde + log.
    let by_slug = deps(&conn, root.path(), None, Some("orga/a"), &Bounds::default()).unwrap();
    match by_slug {
        DepsResult::BySlug {
            slug,
            deps,
            last_walk,
            ..
        } => {
            assert_eq!(slug, "orga/a");
            assert!(last_walk > 0, "staleness stamp surfaced");
            let names: Vec<_> = deps.iter().map(|d| d.name.as_str()).collect();
            assert!(names.contains(&"serde"));
            assert!(names.contains(&"log"));
        }
        other => panic!("expected BySlug, got {other:?}"),
    }

    // Fail closed: both / neither is an error.
    assert!(deps(
        &conn,
        root.path(),
        Some("serde"),
        Some("orga/a"),
        &Bounds::default()
    )
    .is_err());
    assert!(deps(&conn, root.path(), None, None, &Bounds::default()).is_err());
    // An out-of-catalog slug fails loud.
    assert!(deps(
        &conn,
        root.path(),
        None,
        Some("no/such"),
        &Bounds::default()
    )
    .is_err());
}

/// (1) `search` returns live `rg` hits mapped back to their repo slug, and (2)
/// stays inside the clamped root.
#[test]
fn test_search_live_hits_and_scope() {
    let root = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);

    let foo = root.path().join("foo");
    let foobar = root.path().join("foobar");
    std::fs::create_dir_all(&foo).unwrap();
    std::fs::create_dir_all(&foobar).unwrap();
    let repo_a = make_repo(&foo, "svc", "orga/svc");
    let repo_b = make_repo(&foobar, "svc", "orgb/svc");
    std::fs::write(repo_a.join("code.rs"), "fn find_me() {}\n").unwrap();
    std::fs::write(repo_b.join("code.rs"), "fn find_me() {}\n").unwrap();
    walk(&mut conn, root.path(), 5, &[]).unwrap();

    // Whole-tree search finds both.
    let all = search(
        &conn,
        root.path(),
        Some(root.path()),
        "find_me",
        None,
        &Bounds::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(all.hits.len(), 2, "both repos contain find_me");
    assert!(all.hits.iter().all(|h| h.line.contains("find_me")));
    let slugs: Vec<_> = all.hits.iter().map(|h| h.slug.as_str()).collect();
    assert!(slugs.contains(&"orga/svc"));
    assert!(slugs.contains(&"orgb/svc"));

    // Sibling-prefix scope bite: search rooted at foo hits only orga/svc.
    let scoped = search(
        &conn,
        root.path(),
        Some(&foo),
        "find_me",
        None,
        &Bounds::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(scoped.hits.len(), 1);
    assert_eq!(scoped.hits[0].slug, "orga/svc");
}

/// `search` fails closed on an out-of-root root (never a silent empty).
#[test]
fn test_search_rejects_out_of_root() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_conn(&db_dir);
    make_repo(root.path(), "a", "orga/a");
    walk(&mut conn, root.path(), 4, &[]).unwrap();

    assert!(search(
        &conn,
        root.path(),
        Some(outside.path()),
        "anything",
        None,
        &Bounds::default(),
        Duration::from_secs(30),
    )
    .is_err());
}
