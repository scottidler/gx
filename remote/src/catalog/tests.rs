//! Phase 4 `gx catalog --fetch` tests (design doc `2026-07-17-gx-intel-catalog.md`).
//!
//! All fetches use LOCAL bare repos as `origin` (a `git clone` of a bare
//! fixture), so a real `git fetch origin` succeeds OFFLINE -- the tests never
//! hit the network. The fail-loud-skip path points one repo's `origin` at a
//! non-existent local path, which fails INSTANTLY (no network, no hang) while
//! the reachable repos still refresh.

use super::*;
use catalog::db;
use local::test_utils::run_git_command;
use rusqlite::params;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Open a fresh catalog DB in a temp dir (no `$XDG_CACHE_HOME` dependency).
fn temp_db(dir: &TempDir) -> Connection {
    db::open(&dir.path().join("catalog.db")).unwrap()
}

fn config_user(repo: &Path) {
    run_git_command(&["config", "user.email", "test@example.com"], repo);
    run_git_command(&["config", "user.name", "Test User"], repo);
    run_git_command(&["config", "commit.gpgsign", "false"], repo);
}

/// Create a bare origin (branch `main`) seeded with one commit; return its path.
fn init_bare_origin(dir: &Path, name: &str) -> PathBuf {
    let bare = dir.join(format!("{name}.git"));
    run_git_command(
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            bare.to_str().unwrap(),
        ],
        dir,
    );
    let seed = dir.join(format!("{name}-seed"));
    run_git_command(
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
        dir,
    );
    config_user(&seed);
    fs::write(seed.join("README.md"), "# seed").unwrap();
    run_git_command(&["add", "."], &seed);
    run_git_command(&["commit", "--quiet", "-m", "init"], &seed);
    run_git_command(&["push", "--quiet", "origin", "main"], &seed);
    bare
}

/// Clone `bare` into `<parent>/<name>` (origin -> the local bare path); return
/// the clone path. The clone's upstream tracking ref is `origin/main`.
fn clone_into(bare: &Path, parent: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(parent).unwrap();
    let dest = parent.join(name);
    run_git_command(
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        parent,
    );
    config_user(&dest);
    dest
}

/// Push a NEW commit to `bare` so every existing clone becomes one commit behind
/// after it fetches.
fn advance_origin(bare: &Path, scratch: &Path, tag: &str) {
    let work = scratch.join(format!("adv-{tag}"));
    run_git_command(
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
        scratch,
    );
    config_user(&work);
    fs::write(work.join("more.txt"), "more").unwrap();
    run_git_command(&["add", "."], &work);
    run_git_command(&["commit", "--quiet", "-m", "advance"], &work);
    run_git_command(&["push", "--quiet", "origin", "main"], &work);
}

fn last_fetch_of(conn: &Connection, name: &str) -> Option<i64> {
    conn.query_row(
        "SELECT last_fetch FROM repos WHERE name = ?1",
        params![name],
        |r| r.get(0),
    )
    .unwrap()
}

fn behind_of(conn: &Connection, name: &str) -> Option<i64> {
    conn.query_row(
        "SELECT behind FROM repos WHERE name = ?1",
        params![name],
        |r| r.get(0),
    )
    .unwrap()
}

/// (Criteria 1 & 2) `--fetch` updates `last_fetch` + ahead/behind for reachable
/// repos, and succeeds for a repo in EACH of two orgs via their existing
/// `origin` remotes.
#[test]
fn test_fetch_refresh_updates_last_fetch_and_behind_two_orgs() {
    let origins = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    let bare_a = init_bare_origin(origins.path(), "a");
    let bare_b = init_bare_origin(origins.path(), "b");

    // Two org subtrees, each cloning its OWN origin (distinct-org fixture).
    let orga = workspace.path().join("orga");
    let orgb = workspace.path().join("orgb");
    clone_into(&bare_a, &orga, "repoA");
    clone_into(&bare_b, &orgb, "repoB");

    // Advance BOTH origins so each clone is one commit behind after fetch.
    advance_origin(&bare_a, origins.path(), "a");
    advance_origin(&bare_b, origins.path(), "b");

    let summary = fetch_refresh(&mut conn, workspace.path(), 4, &[]).unwrap();
    assert_eq!(summary.fetched, 2, "both origins reachable -> both fetched");
    assert_eq!(summary.fetch_failed, 0, "no fetch failures");
    assert_eq!(summary.walk.repos_indexed, 2, "both repos re-indexed");

    // last_fetch set for both (FETCH_HEAD written by the fetch).
    assert!(
        last_fetch_of(&conn, "repoA").is_some(),
        "repoA fetched -> last_fetch set"
    );
    assert!(
        last_fetch_of(&conn, "repoB").is_some(),
        "repoB fetched -> last_fetch set"
    );

    // ahead/behind refreshed from the updated remote-tracking ref.
    assert_eq!(
        behind_of(&conn, "repoA"),
        Some(1),
        "repoA one commit behind"
    );
    assert_eq!(
        behind_of(&conn, "repoB"),
        Some(1),
        "repoB one commit behind"
    );

    // Two distinct orgs, indexed via their own origins.
    let mut stmt = conn
        .prepare("SELECT DISTINCT org FROM repos ORDER BY org")
        .unwrap();
    let orgs: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(orgs, vec!["orga".to_string(), "orgb".to_string()]);
}

/// (Criterion 3) A repo whose fetch fails is reported LOUDLY and does NOT abort
/// the run: the reachable repo still refreshes (its `last_fetch` is set), and
/// BOTH repos are re-indexed by the local re-walk.
#[test]
fn test_fetch_failure_is_isolated_and_run_continues() {
    let origins = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let mut conn = temp_db(&db_dir);

    let bare_a = init_bare_origin(origins.path(), "a");

    // A reachable repo and a broken one whose origin points at a NON-EXISTENT
    // local path -> `git fetch` fails INSTANTLY (no network, no hang).
    let orga = workspace.path().join("orga");
    let orgb = workspace.path().join("orgb");
    clone_into(&bare_a, &orga, "good");
    let bad = clone_into(&bare_a, &orgb, "bad");
    let dead = origins.path().join("does-not-exist.git");
    run_git_command(
        &["remote", "set-url", "origin", dead.to_str().unwrap()],
        &bad,
    );

    advance_origin(&bare_a, origins.path(), "a");

    let summary = fetch_refresh(&mut conn, workspace.path(), 4, &[]).unwrap();
    assert_eq!(summary.fetch_failed, 1, "the broken repo failed to fetch");
    assert_eq!(summary.fetched, 1, "the reachable repo still fetched");
    assert_eq!(
        summary.walk.repos_indexed, 2,
        "run continued: BOTH repos re-indexed by the local re-walk"
    );

    // The failure did not abort the run: the good repo actually refreshed.
    assert!(
        last_fetch_of(&conn, "good").is_some(),
        "good repo fetched despite the sibling failure"
    );
    assert_eq!(
        behind_of(&conn, "good"),
        Some(1),
        "good repo's ahead/behind refreshed after fetch"
    );
    // The broken repo's fetch failed, so its remote-tracking ref never advanced:
    // it still sees origin at the pre-advance commit (behind 0), unlike `good`.
    // (Its `last_fetch` reflects the initial clone, not a fresh fetch, so the
    // ahead/behind delta is the clean isolation signal here.)
    assert_eq!(
        behind_of(&conn, "bad"),
        Some(0),
        "bad repo's failed fetch left its tracking ref unrefreshed"
    );
}
