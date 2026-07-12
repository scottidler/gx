//! Phase 6 end-to-end: the `llm`/`apply` CLI surface and confirm gate #5,
//! driven through the REAL `gx` binary with a fake-agent fixture (no live
//! LLM).
//!
//! Three proofs the design's Phase 6 success criteria demand:
//!
//! 1. Confirm gate #5 fails closed on non-interactive stdin without `--yes`,
//!    for BOTH the one-shot `gx create ... llm` flow and `gx apply`.
//! 2. The split flow (`gx create ... llm ... --propose` then a separate
//!    `gx apply <change-id>`) produces the identical end state (same branch,
//!    same committed content, pushed to the same remote) as the one-shot flow
//!    (`gx create ... llm ...` with no `--propose`).

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
/// tracking `data.md` = "old value".
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
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);
    repo
}

/// A fake agent: rewrites `data.md` (its CWD is the temp worktree).
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

/// `data.md` on the tip of `branch` in the bare remote (empty if the branch
/// doesn't exist), for comparing the one-shot and split flows' end states.
fn data_on_bare_branch(remotes: &Path, branch: &str) -> String {
    let bare = remotes.join("app.git");
    Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            &format!("refs/heads/{branch}:data.md"),
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Base `gx` args common to every invocation in this file.
fn base_args<'a>(cfg: &'a Path, workspace: &'a Path) -> Vec<&'a str> {
    vec![
        "--config",
        cfg.to_str().unwrap(),
        "--cwd",
        workspace.to_str().unwrap(),
        "--log-level",
        "off",
    ]
}

#[test]
fn test_one_shot_llm_confirm_gate_fails_closed_without_yes_on_non_tty() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(scripts.path());
    let cfg = write_config(scripts.path(), &agent);

    // -p app matches exactly one repo (under the default confirm-threshold),
    // so the up-front blast-radius gate auto-proceeds without a prompt; the
    // ONLY gate left standing is confirm gate #5 (present + apply).
    let mut args = base_args(&cfg, workspace.path());
    args.extend([
        "create",
        "-p",
        "app",
        "--change-id",
        "GX-cli-oneshot-noyes",
        "llm",
        "make data.md say new value",
    ]);
    let out = Command::new(gx_binary())
        .args(&args)
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx create llm failed to spawn");

    assert!(
        !out.status.success(),
        "one-shot llm without --yes must fail closed on non-interactive stdin"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--yes"),
        "error must name --yes; got: {stderr}"
    );
}

#[test]
fn test_apply_confirm_gate_fails_closed_without_yes_on_non_tty() {
    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();
    make_fixture(workspace.path(), remotes.path());
    let agent = write_agent(scripts.path());
    let cfg = write_config(scripts.path(), &agent);
    let change_id = "GX-cli-apply-noyes";

    // Propose only (--propose, --yes so the up-front blast-radius gate
    // doesn't get in the way of isolating the apply-side gate under test).
    let mut propose_args = base_args(&cfg, workspace.path());
    propose_args.extend([
        "create",
        "-p",
        "app",
        "--change-id",
        change_id,
        "--yes",
        "llm",
        "make data.md say new value",
        "--propose",
    ]);
    let propose = Command::new(gx_binary())
        .args(&propose_args)
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx propose failed to spawn");
    assert!(
        propose.status.success(),
        "gx propose failed: {}",
        String::from_utf8_lossy(&propose.stderr)
    );

    // `gx apply` with no --yes on non-interactive stdin must refuse.
    let mut apply_args = base_args(&cfg, workspace.path());
    apply_args.extend(["apply", change_id]);
    let apply = Command::new(gx_binary())
        .args(&apply_args)
        .env("XDG_DATA_HOME", data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx apply failed to spawn");

    assert!(
        !apply.status.success(),
        "gx apply without --yes must fail closed on non-interactive stdin"
    );
    let stderr = String::from_utf8_lossy(&apply.stderr);
    assert!(
        stderr.contains("--yes"),
        "error must name --yes; got: {stderr}"
    );
}

#[test]
fn test_split_propose_then_apply_equals_one_shot() {
    // Two independent fixtures (own workspace/remote/data-home each) so the
    // two flows can't interfere; the fake agent is deterministic, so the same
    // prompt against the same starting content must produce the same result.
    let one_shot_workspace = TempDir::new().unwrap();
    let one_shot_remotes = TempDir::new().unwrap();
    let one_shot_data_home = TempDir::new().unwrap();
    let split_workspace = TempDir::new().unwrap();
    let split_remotes = TempDir::new().unwrap();
    let split_data_home = TempDir::new().unwrap();
    let scripts = TempDir::new().unwrap();

    make_fixture(one_shot_workspace.path(), one_shot_remotes.path());
    make_fixture(split_workspace.path(), split_remotes.path());
    let agent = write_agent(scripts.path());
    let cfg = write_config(scripts.path(), &agent);

    let change_id = "GX-cli-parity";

    // One-shot: propose -> present -> confirm -> apply, all in one command.
    let mut one_shot_args = base_args(&cfg, one_shot_workspace.path());
    one_shot_args.extend([
        "create",
        "-p",
        "app",
        "--change-id",
        change_id,
        "--yes",
        "llm",
        "make data.md say new value",
    ]);
    let one_shot = Command::new(gx_binary())
        .args(&one_shot_args)
        .env("XDG_DATA_HOME", one_shot_data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx create llm (one-shot) failed to spawn");
    assert!(
        one_shot.status.success(),
        "one-shot llm failed: {}",
        String::from_utf8_lossy(&one_shot.stderr)
    );

    // Split: propose (--propose) then a separate `gx apply`.
    let mut propose_args = base_args(&cfg, split_workspace.path());
    propose_args.extend([
        "create",
        "-p",
        "app",
        "--change-id",
        change_id,
        "--yes",
        "llm",
        "make data.md say new value",
        "--propose",
    ]);
    let propose = Command::new(gx_binary())
        .args(&propose_args)
        .env("XDG_DATA_HOME", split_data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx propose failed to spawn");
    assert!(
        propose.status.success(),
        "gx propose failed: {}",
        String::from_utf8_lossy(&propose.stderr)
    );

    let mut apply_args = base_args(&cfg, split_workspace.path());
    apply_args.extend(["apply", change_id, "--yes"]);
    let apply = Command::new(gx_binary())
        .args(&apply_args)
        .env("XDG_DATA_HOME", split_data_home.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("gx apply failed to spawn");
    assert!(
        apply.status.success(),
        "gx apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );

    // Both flows must land the identical branch name, with identical
    // committed content, pushed to their respective remotes.
    let one_shot_branch_data = data_on_bare_branch(one_shot_remotes.path(), change_id);
    let split_branch_data = data_on_bare_branch(split_remotes.path(), change_id);
    assert_eq!(
        one_shot_branch_data, "new value\n",
        "one-shot flow must push the agent's change"
    );
    assert_eq!(
        one_shot_branch_data, split_branch_data,
        "split (propose --propose, then gx apply) must equal the one-shot result"
    );

    // Same change state shape on both sides (Committed, no PR configured).
    let one_shot_state = one_shot_data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    let split_state = split_data_home
        .path()
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    let one_shot_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(one_shot_state).unwrap()).unwrap();
    let split_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(split_state).unwrap()).unwrap();
    let repo_action = |v: &serde_json::Value| v["repositories"]["app"]["status"].clone();
    assert_eq!(
        repo_action(&one_shot_json),
        repo_action(&split_json),
        "recorded per-repo status must match between one-shot and split flows"
    );

    // And the branch is gone from the sub matrix's `-p`/discovery concerns:
    // both flows read the same fake-agent config; the local checkouts stay
    // on their base branch (create switches back after finalize).
    let one_shot_head = git_stdout(
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &one_shot_workspace.path().join("app"),
    );
    let split_head = git_stdout(
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &split_workspace.path().join("app"),
    );
    assert_eq!(one_shot_head.trim(), split_head.trim());
}
