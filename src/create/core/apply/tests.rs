//! Phase 5 apply-core tests: a propose (fake agent, no live LLM) followed by an
//! `execute_apply`, covering the happy path, the post-pull `base_sha` drift
//! refusal, the per-blob tamper refusal, and the loud missing-proposal / token-
//! mismatch errors. The drift and tamper tests are the "must bite" ones: each
//! asserts NOTHING was written (no GX branch, worktree byte-identical) and the
//! repo stayed `Proposed` with its error recorded.

use super::*;
use crate::confirm::Confirmation;
use crate::create::core::propose::execute_propose;
use crate::state::{RepoChangeStatus, StateManager};
use local::config::{Config, CreateConfig, LlmConfig};
use local::repo::Repo;
use local::test_utils::run_git_command;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;

/// A repo at `dir/repo` on its default branch with a bare remote at `dir/repo.git`
/// (apply needs `origin` to resolve the head branch), containing `README.md`.
fn repo_with_remote(dir: &Path) -> (std::path::PathBuf, String) {
    let repo = dir.join("repo");
    let bare = dir.join("repo.git");
    run_git_command(&["init", "--quiet", "--bare", bare.to_str().unwrap()], dir);
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), b"base\n").unwrap();
    run_git_command(&["init", "--quiet"], &repo);
    run_git_command(&["config", "user.email", "t@e.com"], &repo);
    run_git_command(&["config", "user.name", "T"], &repo);
    run_git_command(&["config", "commit.gpgsign", "false"], &repo);
    run_git_command(&["add", "-A"], &repo);
    run_git_command(&["commit", "--quiet", "-m", "init"], &repo);
    run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    let branch = crate::git::get_current_branch_name(&repo).unwrap();
    run_git_command(&["push", "--quiet", "-u", "origin", &branch], &repo);
    run_git_command(&["remote", "set-head", "origin", &branch], &repo);
    (repo, branch)
}

/// An executable fake agent that runs `body` (CWD = temp worktree).
fn fake_agent(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("agent.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn llm_config(agent_command: &str) -> Config {
    Config {
        create: Some(CreateConfig {
            confirm_threshold: Some(5),
            llm: Some(LlmConfig {
                agent_command: Some(agent_command.to_string()),
                timeout_seconds: Some(60),
            }),
        }),
        ..Config::default()
    }
}

fn with_data_home<F: FnOnce()>(f: F) {
    let guard = local::test_utils::env_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    let tmp = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };
    f();
    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

fn local_branch_exists(repo: &Path, branch: &str) -> bool {
    crate::git::branch_exists_locally(repo, branch).unwrap_or(false)
}

fn on_branch_file(repo: &Path, branch: &str, path: &str) -> String {
    let out = run_git_command(&["show", &format!("{branch}:{path}")], repo);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn test_apply_happy_path_commits_the_proposed_blob() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let (repo_path, _branch) = repo_with_remote(ws.path());
        let repo = Repo::new(repo_path.clone()).unwrap();

        // Agent rewrites README.md; propose captures the change against HEAD.
        let agent = fake_agent(scripts.path(), "printf 'applied\\n' > README.md");
        let config = llm_config(agent.to_str().unwrap());
        let change_id = "GX-apply-happy";
        let summary = execute_propose(
            std::slice::from_ref(&repo),
            change_id,
            "rewrite readme",
            &config,
            1,
        )
        .unwrap();
        assert_eq!(
            summary.proposed, 1,
            "the fake agent should propose a change"
        );

        // Apply: rides the create pipeline, pushes a GX branch with the blob.
        let report = execute_apply(
            change_id,
            Some("apply readme"),
            false,
            false,
            &config,
            1,
            Confirmation::AlreadyConfirmed,
        )
        .unwrap();
        assert_eq!(report.applied, 1, "one repo should apply");
        assert_eq!(report.drifted_or_failed, 0);
        assert_eq!(report.token, summary.token, "token binds the same manifest");

        // The GX branch carries the proposed content.
        assert!(
            local_branch_exists(&repo_path, change_id),
            "apply must create the GX branch"
        );
        assert_eq!(
            on_branch_file(&repo_path, change_id, "README.md"),
            "applied\n",
            "the GX branch must carry the proposed blob"
        );

        // State advanced Proposed -> BranchCreated (committed, no PR).
        let state = StateManager::new()
            .unwrap()
            .load(change_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            state.repositories.get(&repo.slug).unwrap().status,
            RepoChangeStatus::BranchCreated
        );
    });
}

#[test]
fn test_apply_refuses_on_base_sha_drift_and_touches_nothing() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let (repo_path, branch) = repo_with_remote(ws.path());
        let repo = Repo::new(repo_path.clone()).unwrap();

        let agent = fake_agent(scripts.path(), "printf 'applied\\n' > README.md");
        let config = llm_config(agent.to_str().unwrap());
        let change_id = "GX-apply-drift";
        execute_propose(
            std::slice::from_ref(&repo),
            change_id,
            "rewrite",
            &config,
            1,
        )
        .unwrap();

        // DRIFT: advance HEAD past the proposal's base and push it, so the
        // apply's post-pull head no longer matches base_sha.
        std::fs::write(repo_path.join("other.txt"), b"drift\n").unwrap();
        run_git_command(&["add", "-A"], &repo_path);
        run_git_command(&["commit", "--quiet", "-m", "drift"], &repo_path);
        run_git_command(&["push", "--quiet", "origin", &branch], &repo_path);

        let before = run_git_command(&["status", "--porcelain"], &repo_path);
        let readme_before = std::fs::read(repo_path.join("README.md")).unwrap();

        let report = execute_apply(
            change_id,
            Some("apply"),
            false,
            false,
            &config,
            1,
            Confirmation::AlreadyConfirmed,
        )
        .unwrap();
        assert_eq!(report.applied, 0, "a drifted repo must not apply");
        assert_eq!(report.drifted_or_failed, 1);

        // Loud, correct error; repo stays Proposed.
        let state = StateManager::new()
            .unwrap()
            .load(change_id)
            .unwrap()
            .unwrap();
        let rs = state.repositories.get(&repo.slug).unwrap();
        assert_eq!(
            rs.status,
            RepoChangeStatus::Proposed,
            "drift keeps it Proposed"
        );
        assert!(
            rs.error.as_deref().unwrap_or("").contains("drifted"),
            "error must name the drift: {:?}",
            rs.error
        );

        // NOTHING written: no GX branch, README unchanged, worktree clean.
        assert!(
            !local_branch_exists(&repo_path, change_id),
            "drift must not create the GX branch"
        );
        assert_eq!(
            std::fs::read(repo_path.join("README.md")).unwrap(),
            readme_before,
            "README must be untouched after a drift refusal"
        );
        let after = run_git_command(&["status", "--porcelain"], &repo_path);
        assert_eq!(
            after.stdout, before.stdout,
            "worktree status must be unchanged"
        );
    });
}

#[test]
fn test_apply_refuses_tampered_blob_and_writes_nothing() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let (repo_path, _branch) = repo_with_remote(ws.path());
        let repo = Repo::new(repo_path.clone()).unwrap();

        let agent = fake_agent(scripts.path(), "printf 'applied\\n' > README.md");
        let config = llm_config(agent.to_str().unwrap());
        let change_id = "GX-apply-tamper";
        execute_propose(
            std::slice::from_ref(&repo),
            change_id,
            "rewrite",
            &config,
            1,
        )
        .unwrap();

        // TAMPER: overwrite the persisted blob with content of the SAME length
        // (the agent wrote "applied\n" = 8 bytes) so the size check passes and
        // ONLY the per-blob sha256 check can catch it. The manifest.json (and
        // thus the token) is untouched, so this isolates the hash verification.
        let dir = manifest::proposal_dir(change_id).unwrap();
        let blob = manifest::blob_path(&dir, &repo.slug, "README.md");
        assert_eq!(
            std::fs::read(&blob).unwrap(),
            b"applied\n",
            "sanity: blob bytes"
        );
        std::fs::write(&blob, b"hacked!\n").unwrap();

        let readme_before = std::fs::read(repo_path.join("README.md")).unwrap();
        let report = execute_apply(
            change_id,
            Some("apply"),
            false,
            false,
            &config,
            1,
            Confirmation::AlreadyConfirmed,
        )
        .unwrap();
        assert_eq!(report.applied, 0, "a tampered blob must not apply");
        assert_eq!(report.drifted_or_failed, 1);

        let state = StateManager::new()
            .unwrap()
            .load(change_id)
            .unwrap()
            .unwrap();
        let rs = state.repositories.get(&repo.slug).unwrap();
        assert_eq!(rs.status, RepoChangeStatus::Proposed);
        assert!(
            rs.error.as_deref().unwrap_or("").contains("hash mismatch"),
            "error must name the hash mismatch: {:?}",
            rs.error
        );

        assert!(
            !local_branch_exists(&repo_path, change_id),
            "a tampered proposal must not create the GX branch"
        );
        assert_eq!(
            std::fs::read(repo_path.join("README.md")).unwrap(),
            readme_before,
            "README must be untouched after a tamper refusal"
        );
    });
}

#[test]
fn test_apply_missing_proposal_is_a_loud_error_naming_the_path() {
    with_data_home(|| {
        let err = execute_apply(
            "GX-does-not-exist",
            None,
            false,
            false,
            &Config::default(),
            1,
            Confirmation::AlreadyConfirmed,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("no proposal to apply") && err.contains("manifest.json"),
            "missing proposal must name the expected manifest path: {err}"
        );
    });
}

#[test]
fn test_apply_refuses_on_token_mismatch() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let (_repo_path, _branch) = repo_with_remote(ws.path());
        let repo = Repo::new(ws.path().join("repo")).unwrap();
        let agent = fake_agent(scripts.path(), "printf 'applied\\n' > README.md");
        let config = llm_config(agent.to_str().unwrap());
        let change_id = "GX-apply-token";
        execute_propose(&[repo], change_id, "rewrite", &config, 1).unwrap();

        let err = execute_apply(
            change_id,
            None,
            false,
            false,
            &config,
            1,
            Confirmation::Token("deadbeefdeadbeef".to_string()),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("token mismatch"),
            "a wrong token must be refused: {err}"
        );
    });
}

#[test]
fn test_apply_refuses_escaping_path_and_writes_nothing() {
    // Audit fix #3: the confirm token binds CONTENT (per-blob sha256), NOT the
    // PATH. A proposal manifest carrying a `.git/`-targeting path must still be
    // refused at the path-confinement seam (`file::validate_new_file_path`),
    // nothing written. Without that seam a hostile/buggy manifest would write an
    // executable hook straight INTO the repo's .git dir. Bite check: drop the
    // `validate_new_file_path` call in `apply_patchset_change`'s verify loop and
    // this test fails (the hook lands and/or the repo applies).
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let (repo_path, _branch) = repo_with_remote(ws.path());
        let repo = Repo::new(repo_path.clone()).unwrap();
        let slug = repo.slug.clone();
        let base_sha = crate::git::get_head_sha(&repo_path).unwrap();
        let change_id = "GX-apply-escape";

        // Hand-craft a proposal whose only file targets `.git/hooks/pre-commit`.
        let evil: &[u8] = b"#!/bin/sh\necho pwned\n";
        let entry = manifest::FileEntry {
            path: ".git/hooks/pre-commit".to_string(),
            action: manifest::FileAction::Add,
            mode: "100755".to_string(),
            sha256: Some(local::hash::sha256_hex(evil)),
            size: evil.len() as u64,
        };
        let rp = manifest::RepoProposal {
            slug: slug.clone(),
            base_sha: base_sha.clone(),
            outcome: manifest::ProposalOutcome::Proposed,
            error: None,
            files: vec![entry],
        };
        let m = manifest::ProposalManifest::new(
            change_id.to_string(),
            "inject a hook".to_string(),
            "fake-agent".to_string(),
            vec![rp],
        );
        let dir = manifest::proposal_dir(change_id).unwrap();
        manifest::write_manifest(&dir, &m).unwrap();
        manifest::write_blob(&dir, &slug, ".git/hooks/pre-commit", evil).unwrap();

        // Record the Proposed repo in change state so apply resolves it.
        let mgr = StateManager::new().unwrap();
        let mut state = ChangeState::new(change_id.to_string(), Some("inject".to_string()));
        state.mark_proposed(
            &slug,
            base_sha,
            vec![".git/hooks/pre-commit".to_string()],
            Some(repo_path.to_string_lossy().to_string()),
        );
        mgr.save(&state).unwrap();

        let hook = repo_path.join(".git").join("hooks").join("pre-commit");
        let hook_before = std::fs::read(&hook).ok();

        let report = execute_apply(
            change_id,
            Some("apply"),
            false,
            false,
            &Config::default(),
            1,
            Confirmation::AlreadyConfirmed,
        )
        .unwrap();
        assert_eq!(
            report.applied, 0,
            "an escaping-path proposal must not apply"
        );
        assert_eq!(report.drifted_or_failed, 1);

        // Repo stays Proposed with a loud error naming the unsafe path.
        let rs_state = mgr.load(change_id).unwrap().unwrap();
        let rs = rs_state.repositories.get(&slug).unwrap();
        assert_eq!(rs.status, RepoChangeStatus::Proposed);
        let err = rs.error.as_deref().unwrap_or("");
        assert!(
            err.contains("unsafe path") && err.contains(".git"),
            "error must name the unsafe path: {:?}",
            rs.error
        );

        // NOTHING written: the .git hook is untouched (still absent, or its prior
        // bytes) and no GX branch was created.
        assert_eq!(
            std::fs::read(&hook).ok(),
            hook_before,
            "the .git hook must be untouched by a refused apply"
        );
        assert!(
            !local_branch_exists(&repo_path, change_id),
            "an escaping-path proposal must not create the GX branch"
        );
    });
}

#[test]
fn test_apply_fails_fast_while_change_lock_is_held() {
    // Audit fix #1: the ENTIRE apply RMW (manifest+token read, state load,
    // pipeline write, straggler reconcile) is serialized under ONE ChangeLock
    // acquired at the TOP of execute_apply. A concurrent apply/undo already
    // holding this change's lock makes apply fail fast and write nothing, rather
    // than interleaving the artifact/state RMW. Bite check: remove the
    // `ChangeLock::acquire` at the top of execute_apply (the core no longer
    // self-locks) and apply proceeds to create the GX branch while the lock is
    // held - both assertions below fail.
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let (repo_path, _branch) = repo_with_remote(ws.path());
        let repo = Repo::new(repo_path.clone()).unwrap();
        let agent = fake_agent(scripts.path(), "printf 'applied\\n' > README.md");
        let config = llm_config(agent.to_str().unwrap());
        let change_id = "GX-apply-serialize";
        execute_propose(
            std::slice::from_ref(&repo),
            change_id,
            "rewrite",
            &config,
            1,
        )
        .unwrap();

        // Simulate a concurrent apply/undo already holding this change's lock.
        // (flock is per-open-fd, so this same-process second acquire inside
        // execute_apply contends and fails fast - Phase 2's proven property.)
        let held = crate::lock::ChangeLock::acquire(change_id).unwrap();

        let err = execute_apply(
            change_id,
            Some("apply"),
            false,
            false,
            &config,
            1,
            Confirmation::AlreadyConfirmed,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("locked"),
            "apply must fail fast while the change lock is held: {err}"
        );
        assert!(
            !local_branch_exists(&repo_path, change_id),
            "a lock-blocked apply must not create the GX branch (no interleaving)"
        );
        drop(held);
    });
}
