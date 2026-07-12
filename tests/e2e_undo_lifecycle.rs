//! End-to-end lifecycle test for `gx undo` (Phase 5, F4): a temp "org" of bare
//! remotes plus a `gh` PATH shim. An open-PR campaign is undone end-to-end -
//! PRs closed, GX branches deleted (remote and local) - with the base branches
//! byte-identical before and after (undo NEVER touches a base branch), the
//! change marked `Abandoned`, and a second `undo` run a clean no-op.
//!
//! The `gh` shim actually mutates the bare remotes on the DELETE call (locating
//! them via `$GX_TEST_REMOTES` + the repo name parsed from the api path), so the
//! remote-branch deletion is really exercised, not merely asserted, per the
//! 2026-06-11 gh-shim precedent (assert argv shape, perform the real effect).

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn gx_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("gx");
    path
}

/// A repo at `workspace/<name>` on `main` with a bare remote at
/// `remotes/<name>.git`, containing `data.md` = "old value".
fn make_repo(workspace: &Path, remotes: &Path, name: &str) -> std::path::PathBuf {
    let bare = remotes.join(format!("{name}.git"));
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );

    let repo = workspace.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &repo);
    git(&["config", "user.email", "t@e.com"], &repo);
    git(&["config", "user.name", "T"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);
    std::fs::write(repo.join("data.md"), "old value\n").unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);
    repo
}

/// A `gh` PATH shim: `api graphql` -> empty search (reconcile finds nothing to
/// change), `pr close` -> success, `api repos/<org>/<repo>/git/refs/heads/<b>
/// --method DELETE` -> deletes the ref from the matching bare remote under
/// `$GX_TEST_REMOTES`. Any other invocation fails loudly.
const GH_SHIM: &str = r#"#!/bin/sh
if [ "$1" = "api" ] && [ "$2" = "graphql" ]; then
  printf '%s' '{"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "close" ]; then
  exit 0
fi
if [ "$1" = "api" ]; then
  path="$2"
  case "$path" in
    repos/*/git/refs/heads/*)
      rest="${path#repos/}"
      rest="${rest#*/}"
      repo="${rest%%/*}"
      branch="${path#*refs/heads/}"
      git --git-dir "$GX_TEST_REMOTES/$repo.git" update-ref -d "refs/heads/$branch"
      exit $?
      ;;
  esac
fi
echo "gh shim: unexpected invocation: $*" >&2
exit 1
"#;

fn write_shim(dir: &Path) {
    let gh = dir.join("gh");
    std::fs::write(&gh, GH_SHIM).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&gh).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&gh, perms).unwrap();
    }
}

/// SHA of a ref in a bare remote (base-branch byte-identity check).
fn bare_ref(remotes: &Path, name: &str, refname: &str) -> String {
    let out = Command::new("git")
        .args([
            "--git-dir",
            remotes.join(format!("{name}.git")).to_str().unwrap(),
            "rev-parse",
            refname,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rev-parse {refname} in {name}.git failed"
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn branch_on_bare(remotes: &Path, name: &str, branch: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            remotes.join(format!("{name}.git")).to_str().unwrap(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}

fn branch_local(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
#[cfg(unix)]
fn test_undo_open_pr_campaign_end_to_end() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let shim_dir = TempDir::new().unwrap();
    write_shim(shim_dir.path());

    let change_id = "GX-undo-e2e";
    let repos = ["frontend", "backend"];
    let frontend = make_repo(workspace.path(), remotes.path(), "frontend");
    let backend = make_repo(workspace.path(), remotes.path(), "backend");

    // Base-branch safe points, captured before any undo touches the remotes.
    let base_before: Vec<String> = repos
        .iter()
        .map(|r| bare_ref(remotes.path(), r, "main"))
        .collect();

    // 1. gx create: push a GX branch to each bare remote, record change state.
    let create = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "--files",
            "**/*.md",
            "--change-id",
            change_id,
            "--commit",
            "e2e: old to new",
            "--yes",
            "sub",
            "old",
            "new",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .output()
        .expect("gx create failed to spawn");
    assert!(
        create.status.success(),
        "gx create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    for r in &repos {
        assert!(
            branch_on_bare(remotes.path(), r, change_id),
            "{r} bare should have the pushed GX branch after create"
        );
    }

    // 2. Doctor the recorded state so each repo looks like it has an OPEN PR
    //    (create ran offline with no --pr; this simulates PRs having been
    //    opened, so undo exercises the close-PR path).
    let state_path = data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    for (pr, (_slug, repo)) in (101u64..).zip(
        state["repositories"]
            .as_object_mut()
            .expect("repositories object")
            .iter_mut(),
    ) {
        repo["status"] = serde_json::json!("PrOpen");
        repo["pr_number"] = serde_json::json!(pr);
        repo["pr_url"] = serde_json::json!(format!("https://github.com/x/y/pull/{pr}"));
    }
    std::fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // 3. gx undo, with the gh shim on PATH and the bare remotes located via env.
    let path_env = format!(
        "{}:{}",
        shim_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let undo = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "undo",
            change_id,
            "--yes",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .env("PATH", &path_env)
        .env("GX_TEST_REMOTES", remotes.path())
        .output()
        .expect("gx undo failed to spawn");
    assert!(
        undo.status.success(),
        "gx undo failed: {}",
        String::from_utf8_lossy(&undo.stderr)
    );

    // The GX branch is gone from every bare remote AND locally.
    for r in &repos {
        assert!(
            !branch_on_bare(remotes.path(), r, change_id),
            "{r} bare GX branch should be deleted by undo"
        );
    }
    assert!(
        !branch_local(&frontend, change_id),
        "local GX branch remains"
    );
    assert!(
        !branch_local(&backend, change_id),
        "local GX branch remains"
    );

    // Base branches are byte-identical before and after: undo never touches them.
    let base_after: Vec<String> = repos
        .iter()
        .map(|r| bare_ref(remotes.path(), r, "main"))
        .collect();
    assert_eq!(
        base_before, base_after,
        "undo must leave every base branch byte-identical"
    );

    // The change is marked Abandoned.
    let final_state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(
        final_state["status"], "Abandoned",
        "change should be Abandoned after a full undo"
    );

    // 4. A second undo run is a clean no-op.
    let undo2 = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "undo",
            change_id,
            "--yes",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .env("PATH", &path_env)
        .env("GX_TEST_REMOTES", remotes.path())
        .output()
        .expect("second gx undo failed to spawn");
    assert!(
        undo2.status.success(),
        "second gx undo failed: {}",
        String::from_utf8_lossy(&undo2.stderr)
    );
    let out2 = String::from_utf8_lossy(&undo2.stdout);
    assert!(
        out2.contains("Nothing to undo"),
        "second undo should be a no-op, got: {out2}"
    );
}

#[test]
#[cfg(unix)]
fn test_undo_recovery_only_pushed_deletes_remote_branch() {
    // FIX B (second post-audit hardening): a pushed GX branch may be recorded
    // ONLY in a recovery file, with NO change-state file at all (crash between
    // push and state save, F12). `gx undo` must still run from the recovery
    // files alone and delete the pushed REMOTE branch (not just the local one),
    // leaving no orphan; a second run is a clean no-op.
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let shim_dir = TempDir::new().unwrap();
    write_shim(shim_dir.path());

    let change_id = "GX-recovery-only";
    let repo = make_repo(workspace.path(), remotes.path(), "svc");

    // Simulate the crash-after-push: a GX branch pushed to the remote, then the
    // local checkout switched back to main (as finalize would), with the state
    // save never having happened -- only a recovery file remains.
    git(&["checkout", "--quiet", "-b", change_id, "main"], &repo);
    git(&["push", "--quiet", "origin", change_id], &repo);
    git(&["checkout", "--quiet", "main"], &repo);
    assert!(
        branch_on_bare(remotes.path(), "svc", change_id),
        "sanity: the GX branch must be on the remote"
    );
    let base_before = bare_ref(remotes.path(), "svc", "main");

    // A live `pushed`-phase recovery file for this repo; NO change-state file.
    let recovery_dir = data_home.path().join("gx").join("recovery");
    std::fs::create_dir_all(&recovery_dir).unwrap();
    let tx_id = "gx-tx-recovery-only-e2e";
    let rec = serde_json::json!({
        "version": 1,
        "transaction_id": tx_id,
        "change_id": change_id,
        "repo_path": repo.to_str().unwrap(),
        "created_at": "2026-07-11T00:00:00Z",
        "phase": "pushed",
        "branch": change_id,
        "steps": []
    });
    std::fs::write(
        recovery_dir.join(format!("{tx_id}.json")),
        serde_json::to_string_pretty(&rec).unwrap(),
    )
    .unwrap();
    assert!(
        !data_home
            .path()
            .join("gx")
            .join("changes")
            .join(format!("{change_id}.json"))
            .exists(),
        "there must be NO change-state file for this recovery-only campaign"
    );

    let path_env = format!(
        "{}:{}",
        shim_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let undo = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "undo",
            change_id,
            "--yes",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .env("PATH", &path_env)
        .env("GX_TEST_REMOTES", remotes.path())
        .output()
        .expect("gx undo failed to spawn");
    assert!(
        undo.status.success(),
        "gx undo (recovery-only) failed: {}",
        String::from_utf8_lossy(&undo.stderr)
    );

    // The pushed REMOTE branch is gone (not stranded), the local one too, and
    // the base branch is byte-identical.
    assert!(
        !branch_on_bare(remotes.path(), "svc", change_id),
        "the recovery-only pushed REMOTE branch must be deleted by undo"
    );
    assert!(
        !branch_local(&repo, change_id),
        "the local GX branch must be deleted by undo"
    );
    assert_eq!(
        bare_ref(remotes.path(), "svc", "main"),
        base_before,
        "undo must leave the base branch byte-identical"
    );
    assert!(
        !recovery_dir.join(format!("{tx_id}.json")).exists(),
        "the drained recovery file must be removed"
    );

    // A second run is a clean no-op (no state, no recovery files left).
    let undo2 = Command::new(gx_binary())
        .args([
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "undo",
            change_id,
            "--yes",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .env("PATH", &path_env)
        .env("GX_TEST_REMOTES", remotes.path())
        .output()
        .expect("second gx undo failed to spawn");
    assert!(
        undo2.status.success(),
        "second recovery-only undo failed: {}",
        String::from_utf8_lossy(&undo2.stderr)
    );
    assert!(
        String::from_utf8_lossy(&undo2.stdout).contains("Nothing to undo"),
        "second recovery-only undo should be a no-op"
    );
}
