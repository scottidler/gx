//! Phase 5 end-to-end: drive a REAL `gx` binary through a fake-agent propose
//! (no live LLM) and then the apply of that persisted proposal.
//!
//! Two proofs the design's Phase 5 success criteria demand:
//!
//! 1. **Crash-injection parity.** An Llm apply rides the identical
//!    `process_single_repo` pipeline as a `sub`, so `GX_CRASH_POINT` recovery
//!    must behave IDENTICALLY: same recorded phase per boundary, worktree
//!    byte-identical after `gx rollback execute`, same remote-branch retention.
//! 2. **Undo of an applied campaign.** `gx undo` reverses a pushed llm campaign
//!    (branch gone remote + local, change `Abandoned`) AND the proposal
//!    artifacts are removed (retention).
//!
//! Propose and apply are driven through the real Phase 6 CLI verbs
//! (`gx create ... llm "<prompt>" --propose` and `gx apply <change-id>
//! --yes`), not the Phase 4/5 inert env hooks (which Phase 6 replaced). The
//! fake agent is configured via `--config` (`create.llm`).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
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

fn git_stdout(args: &[&str], dir: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git failed to spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).to_string()
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

/// A repo at `workspace/app` on `main` with a bare remote at `remotes/app.git`,
/// containing tracked `data.md` ("old value"), a tracked executable `run.sh`,
/// and an UNTRACKED `wip.txt` (so every run stashes, arming `after-stash`).
fn make_fixture(workspace: &Path, remotes: &Path) -> std::path::PathBuf {
    let bare = remotes.join("app.git");
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );
    let repo = workspace.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &repo);
    git(&["config", "user.email", "t@e.com"], &repo);
    git(&["config", "user.name", "T"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);
    std::fs::write(repo.join("data.md"), "old value\n").unwrap();
    let script = repo.join("run.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);
    std::fs::write(repo.join("wip.txt"), "WIP\n").unwrap();
    repo
}

/// A fake agent: rewrites `data.md` (its CWD is the temp worktree), matching the
/// deterministic "old value" -> "new value" change a `sub` would make, so the
/// applied patchset drives the pipeline identically.
fn write_agent(dir: &Path) -> std::path::PathBuf {
    let agent = dir.join("agent.sh");
    std::fs::write(&agent, "#!/bin/sh\nprintf 'new value\\n' > data.md\n").unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    agent
}

/// A gx config pointing `create.llm.agent-command` at the fake agent.
fn write_config(dir: &Path, agent: &Path) -> std::path::PathBuf {
    let cfg = dir.join("gx.yml");
    std::fs::write(
        &cfg,
        format!(
            "create:\n  llm:\n    agent-command: \"{}\"\n    timeout-seconds: 60\n",
            agent.display()
        ),
    )
    .unwrap();
    cfg
}

/// Byte-identity snapshot robust to git's umask mode drift (mirrors the sub
/// crash matrix): HEAD, sorted porcelain, `ls-files -s`, and the data files.
fn worktree_snapshot(repo: &Path) -> String {
    let head = git_stdout(&["rev-parse", "HEAD"], repo);
    let mut porcelain: Vec<String> = git_stdout(&["status", "--porcelain"], repo)
        .lines()
        .map(|l| l.to_string())
        .collect();
    porcelain.sort();
    let tracked = git_stdout(&["ls-files", "-s"], repo);
    let data = std::fs::read_to_string(repo.join("data.md")).unwrap_or_default();
    let wip = std::fs::read_to_string(repo.join("wip.txt")).unwrap_or_default();
    format!(
        "HEAD={head}\nPORCELAIN=\n{}\nTRACKED=\n{tracked}\nDATA={data}\nWIP={wip}",
        porcelain.join("\n")
    )
}

fn branch_on_bare(remotes: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            remotes.join("app.git").to_str().unwrap(),
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

fn sole_recovery(data_home: &Path) -> (String, String) {
    let dir = data_home.join("gx").join("recovery");
    let files: Vec<_> = std::fs::read_dir(&dir)
        .expect("recovery dir must exist after a crash")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    assert_eq!(files.len(), 1, "exactly one recovery file expected");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
    (
        json["transaction_id"].as_str().unwrap().to_string(),
        json["phase"].as_str().unwrap().to_string(),
    )
}

/// Run `gx create ... llm "<prompt>" --propose` for `change_id` through the
/// real `llm` clap verb; a config supplies the fake agent. `--yes` skips the
/// up-front blast-radius confirm (stdin is null in these tests); `--propose`
/// stops after persisting proposals, leaving apply to a separate `gx apply`.
fn run_propose(workspace: &Path, cfg: &Path, data_home: &Path, change_id: &str) {
    let out = Command::new(gx_binary())
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--cwd",
            workspace.to_str().unwrap(),
            "--log-level",
            "off",
            "create",
            "-p",
            "app",
            "--change-id",
            change_id,
            "--yes",
            "llm",
            "make data.md say new value",
            "--propose",
        ])
        .env("XDG_DATA_HOME", data_home)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx propose failed to spawn");
    assert!(
        out.status.success(),
        "gx propose failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_apply_crash_matrix_parity_with_sub() {
    let change_id = "GX-llm-crash";
    // (crash point, expected recorded phase, remote branch retained after recovery)
    let matrix = [
        ("after-stash", "mutating", false),
        ("after-branch", "mutating", false),
        ("after-commit", "mutating", false),
        ("before-push", "pushing", false),
        ("after-push", "pushed", true),
        ("mid-finalize", "finalizing", true),
    ];

    for (point, expected_phase, remote_retained) in matrix {
        let workspace = TempDir::new().unwrap();
        let remotes = TempDir::new().unwrap();
        let data_home = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo = make_fixture(workspace.path(), remotes.path());
        let agent = write_agent(scripts.path());
        let cfg = write_config(scripts.path(), &agent);

        // 1. Propose (no crash): persists the proposal + Proposed state.
        run_propose(workspace.path(), &cfg, data_home.path(), change_id);
        let before = worktree_snapshot(&repo);

        // 2. Apply, crashing at `point`. Same pipeline as a `sub` create.
        let apply = Command::new(gx_binary())
            .args([
                "--config",
                cfg.to_str().unwrap(),
                "--cwd",
                workspace.path().to_str().unwrap(),
                "--log-level",
                "off",
                "apply",
                change_id,
                "--yes",
            ])
            .env("XDG_DATA_HOME", data_home.path())
            .env("GX_CRASH_POINT", point)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("gx apply failed to spawn");
        assert!(
            !apply.status.success(),
            "[{point}] gx apply must die at the crash point"
        );

        // 3. Recovery records the RIGHT phase for this boundary (== sub).
        let (tx_id, phase) = sole_recovery(data_home.path());
        assert_eq!(
            phase, expected_phase,
            "[{point}] recovery phase must match the sub matrix"
        );

        // 4. gx rollback execute recovers byte-identically.
        let exec = Command::new(gx_binary())
            .args(["rollback", "execute", &tx_id, "--force", "--yes"])
            .env("XDG_DATA_HOME", data_home.path())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("gx rollback execute failed to spawn");
        assert!(
            exec.status.success(),
            "[{point}] rollback execute failed: {}",
            String::from_utf8_lossy(&exec.stderr)
        );

        let after = worktree_snapshot(&repo);
        assert_eq!(
            before, after,
            "[{point}] worktree must be byte-identical after recovery (parity with sub)"
        );

        // 5. Remote/local retention matches the phase decision (== sub).
        assert_eq!(
            branch_on_bare(remotes.path(), change_id),
            remote_retained,
            "[{point}] remote branch retention must match the sub matrix"
        );
        assert_eq!(
            branch_local(&repo, change_id),
            remote_retained,
            "[{point}] local GX branch retention must match the phase decision"
        );
    }
}

/// The `gh` PATH shim from the undo lifecycle e2e: `api graphql` -> empty,
/// `pr close` -> ok, `api repos/<org>/<repo>/git/refs/heads/<b> DELETE` ->
/// deletes the ref from the matching bare under `$GX_TEST_REMOTES`.
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

fn write_gh_shim(dir: &Path) {
    let gh = dir.join("gh");
    std::fs::write(&gh, GH_SHIM).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn test_undo_reverses_applied_llm_campaign_and_removes_proposal() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    let shim_dir = TempDir::new().unwrap();
    write_gh_shim(shim_dir.path());

    let change_id = "GX-llm-undo";
    let repo = make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(scripts.path());
    let cfg = write_config(scripts.path(), &agent);

    // 1. Propose, then 2. apply: pushes a GX branch to the bare remote.
    run_propose(workspace.path(), &cfg, data_home.path(), change_id);
    let apply = Command::new(gx_binary())
        .args([
            "--config",
            cfg.to_str().unwrap(),
            "--cwd",
            workspace.path().to_str().unwrap(),
            "--log-level",
            "off",
            "apply",
            change_id,
            "--yes",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx apply failed to spawn");
    assert!(
        apply.status.success(),
        "gx apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(
        branch_on_bare(remotes.path(), change_id),
        "apply must push the GX branch to the remote"
    );
    let proposal_dir = data_home
        .path()
        .join("gx")
        .join("proposals")
        .join(change_id);
    assert!(
        proposal_dir.exists(),
        "proposal artifacts must exist post-apply"
    );

    // 3. Undo the applied campaign with the gh shim on PATH.
    let path_env = format!(
        "{}:{}",
        shim_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let undo = Command::new(gx_binary())
        .args([
            "--config",
            cfg.to_str().unwrap(),
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
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx undo failed to spawn");
    assert!(
        undo.status.success(),
        "gx undo failed: {}",
        String::from_utf8_lossy(&undo.stderr)
    );

    // The GX branch is gone remote + local; the change is Abandoned; and the
    // proposal artifacts were removed by undo (retention).
    assert!(
        !branch_on_bare(remotes.path(), change_id),
        "undo must delete the pushed GX branch from the remote"
    );
    assert!(
        !branch_local(&repo, change_id),
        "undo must delete the local GX branch"
    );
    let state_path = data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["status"], "Abandoned", "change must be Abandoned");
    assert!(
        !proposal_dir.exists(),
        "undo must remove the proposal artifacts (retention)"
    );
}
