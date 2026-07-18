use super::*;
use crate::state::{RepoChangeStatus, StateManager};
use local::config::{Config, CreateConfig, LlmConfig};
use local::test_utils::run_git_command;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::TempDir;

/// Init a git repo at `path` with the given files committed, no remote (propose
/// never needs `origin`; it operates on a detached worktree of HEAD).
fn init_repo(path: &Path, files: &[(&str, &[u8])]) {
    std::fs::create_dir_all(path).unwrap();
    let init = run_git_command(&["init", "--quiet"], path);
    assert!(init.status.success(), "git init failed");
    run_git_command(&["config", "user.email", "t@example.com"], path);
    run_git_command(&["config", "user.name", "Test"], path);
    run_git_command(&["config", "commit.gpgsign", "false"], path);
    for (rel, content) in files {
        let full = path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }
    run_git_command(&["add", "-A"], path);
    let commit = run_git_command(&["commit", "--quiet", "-m", "init"], path);
    assert!(commit.status.success(), "git commit failed");
}

/// Write an executable fake-agent shell script and return its path. `body` runs
/// with CWD = the temp worktree; the propose prompt is appended as `$1`.
fn fake_agent(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let script = format!("#!/bin/sh\n{body}\n");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// A `Config` whose `create.llm` points at `agent_command` with `timeout` secs.
fn llm_config(agent_command: &str, timeout: u64) -> Config {
    Config {
        create: Some(CreateConfig {
            confirm_threshold: Some(5),
            llm: Some(LlmConfig {
                agent_command: Some(agent_command.to_string()),
                timeout_seconds: Some(timeout),
            }),
        }),
        ..Config::default()
    }
}

/// Run `f` with `XDG_DATA_HOME` pointed at a fresh temp dir (env is global; the
/// lock/state/proposal dirs all resolve from it).
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

/// Snapshot every tracked file's bytes plus `git status --porcelain` for a repo,
/// so a test can assert byte-for-byte identity of the REAL worktree.
fn snapshot(repo: &Path) -> (String, Vec<(String, Vec<u8>)>) {
    let status = run_git_command(&["status", "--porcelain"], repo);
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    let ls = run_git_command(&["ls-files"], repo);
    let mut files = Vec::new();
    for name in String::from_utf8_lossy(&ls.stdout).lines() {
        let bytes = std::fs::read(repo.join(name)).unwrap_or_default();
        files.push((name.to_string(), bytes));
    }
    (status, files)
}

fn assert_worktree_unchanged(repo: &Path, before: &(String, Vec<(String, Vec<u8>)>)) {
    let after = snapshot(repo);
    assert_eq!(
        after.0, before.0,
        "real worktree `git status` changed: {:?} -> {:?}",
        before.0, after.0
    );
    assert_eq!(
        after.1, before.1,
        "real worktree tracked-file bytes changed"
    );
    assert!(
        after.0.trim().is_empty(),
        "real worktree must be clean after propose, got: {}",
        after.0
    );
}

#[test]
fn test_happy_path_proposes_persists_and_leaves_worktree_identical() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("myorg").join("myrepo");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();
        let before = snapshot(&repo_path);

        // Agent adds a new file in the (temp) worktree.
        let agent = fake_agent(
            scripts.path(),
            "add.sh",
            "printf 'hello proposed\\n' > proposed.txt",
        );
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary = execute_propose(
            std::slice::from_ref(&repo),
            "GX-happy",
            "add a file",
            &config,
            1,
        )
        .unwrap();

        assert_eq!(summary.proposed, 1);
        assert_eq!(summary.empty, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.token.len(), 16);

        // The real worktree is byte-identical; the agent's file never appeared.
        assert_worktree_unchanged(&repo_path, &before);
        assert!(!repo_path.join("proposed.txt").exists());

        // Manifest records one Add entry with a verifying hash + a real blob.
        let rp = &summary.repos[0];
        assert_eq!(rp.outcome, manifest::ProposalOutcome::Proposed);
        assert_eq!(rp.files.len(), 1);
        let entry = &rp.files[0];
        assert_eq!(entry.path, "proposed.txt");
        assert_eq!(entry.action, manifest::FileAction::Add);
        assert_eq!(entry.mode, "100644");

        let pdir = manifest::proposal_dir("GX-happy").unwrap();
        let blob = manifest::blob_path(&pdir, &rp.slug, "proposed.txt");
        let blob_bytes = std::fs::read(&blob).expect("blob must exist");
        assert_eq!(blob_bytes, b"hello proposed\n");
        assert_eq!(
            entry.sha256.as_deref().unwrap(),
            local::hash::sha256_hex(&blob_bytes),
            "manifest hash must verify against the persisted blob"
        );
        assert_eq!(entry.size, blob_bytes.len() as u64);

        // Patch (display) exists; manifest reload round-trips with the token.
        assert!(manifest::patch_path(&pdir, &rp.slug).exists());
        let mbytes = std::fs::read(pdir.join("manifest.json")).unwrap();
        assert_eq!(manifest::compute_token(&mbytes), summary.token);

        // State recorded the repo as Proposed with base_sha.
        let state = StateManager::new()
            .unwrap()
            .load("GX-happy")
            .unwrap()
            .expect("proposal state must be saved");
        let rs = state.repositories.get(&rp.slug).unwrap();
        assert_eq!(rs.status, RepoChangeStatus::Proposed);
        assert_eq!(rs.base_sha.as_deref(), Some(rp.base_sha.as_str()));
    });
}

#[test]
fn test_empty_diff_is_empty_outcome_not_error() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("empty");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();
        let before = snapshot(&repo_path);

        // Agent does nothing (exit 0, no edits).
        let agent = fake_agent(scripts.path(), "noop.sh", "exit 0");
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary =
            execute_propose(std::slice::from_ref(&repo), "GX-empty", "noop", &config, 1).unwrap();

        assert_eq!(summary.empty, 1);
        assert_eq!(summary.proposed, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.repos[0].outcome, manifest::ProposalOutcome::Empty);
        assert!(summary.repos[0].error.is_none());
        assert_worktree_unchanged(&repo_path, &before);

        // No Proposed repo => no change state file.
        assert!(StateManager::new()
            .unwrap()
            .load("GX-empty")
            .unwrap()
            .is_none());
    });
}

#[test]
fn test_agent_nonzero_exit_is_loud_failure() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("boom");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();
        let before = snapshot(&repo_path);

        let agent = fake_agent(scripts.path(), "fail.sh", "echo nope 1>&2; exit 3");
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary =
            execute_propose(std::slice::from_ref(&repo), "GX-fail", "fail", &config, 1).unwrap();

        assert_eq!(summary.failed, 1);
        let rp = &summary.repos[0];
        assert_eq!(rp.outcome, manifest::ProposalOutcome::Failed);
        assert!(
            rp.error.as_deref().unwrap().contains("status 3"),
            "error must name the nonzero exit: {:?}",
            rp.error
        );
        assert_worktree_unchanged(&repo_path, &before);
    });
}

#[test]
fn test_timeout_kills_whole_process_group() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("slow");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();
        let before = snapshot(&repo_path);

        // The agent spawns a long-lived grandchild (records its pid), then hangs.
        // A group-wide kill must fell the grandchild too.
        let pidfile = scripts.path().join("grandchild.pid");
        let body = format!(
            "sleep 300 &\necho $! > {}\nsleep 300\n",
            pidfile.to_str().unwrap()
        );
        let agent = fake_agent(scripts.path(), "hang.sh", &body);
        // 1s timeout keeps the test fast.
        let config = llm_config(agent.to_str().unwrap(), 1);

        let start = Instant::now();
        let summary = execute_propose(
            std::slice::from_ref(&repo),
            "GX-timeout",
            "hang",
            &config,
            1,
        )
        .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(summary.failed, 1);
        assert!(
            summary.repos[0]
                .error
                .as_deref()
                .unwrap()
                .contains("timed out"),
            "error must name the timeout: {:?}",
            summary.repos[0].error
        );
        // Killed near the 1s deadline, well under a generous ceiling.
        assert!(
            elapsed < Duration::from_secs(20),
            "timeout took too long: {elapsed:?}"
        );

        // The grandchild must be dead (process-group kill worked).
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("grandchild pid must have been recorded")
            .trim()
            .parse()
            .unwrap();
        let mut alive = true;
        for _ in 0..50 {
            let check = Command::new("kill").arg("-0").arg(pid.to_string()).status();
            if matches!(check, Ok(s) if !s.success()) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            !alive,
            "grandchild pid {pid} survived the process-group kill"
        );

        assert_worktree_unchanged(&repo_path, &before);
    });
}

#[test]
fn test_binary_file_roundtrips() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("bin");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();

        // Emit 3 raw bytes including a NUL and a high byte (non-UTF-8 content).
        let agent = fake_agent(
            scripts.path(),
            "bin.sh",
            "printf '\\000\\377\\001' > blob.bin",
        );
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary =
            execute_propose(std::slice::from_ref(&repo), "GX-bin", "binary", &config, 1).unwrap();

        assert_eq!(summary.proposed, 1);
        let rp = &summary.repos[0];
        let entry = rp.files.iter().find(|f| f.path == "blob.bin").unwrap();
        let pdir = manifest::proposal_dir("GX-bin").unwrap();
        let blob = std::fs::read(manifest::blob_path(&pdir, &rp.slug, "blob.bin")).unwrap();
        assert_eq!(blob, vec![0x00, 0xff, 0x01], "binary blob must round-trip");
        assert_eq!(
            entry.sha256.as_deref().unwrap(),
            local::hash::sha256_hex(&blob)
        );
    });
}

#[test]
fn test_mode_only_change_is_captured() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("mode");
        init_repo(&repo_path, &[("run.sh", b"#!/bin/sh\necho hi\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();

        // Only flip the executable bit; content unchanged.
        let agent = fake_agent(scripts.path(), "chmod.sh", "chmod +x run.sh");
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary =
            execute_propose(std::slice::from_ref(&repo), "GX-mode", "chmod", &config, 1).unwrap();

        assert_eq!(
            summary.proposed, 1,
            "a mode-only change is a real proposal: {:?}",
            summary.repos[0].error
        );
        let rp = &summary.repos[0];
        let entry = rp.files.iter().find(|f| f.path == "run.sh").unwrap();
        assert_eq!(entry.mode, "100755", "executable bit must be captured");
        // The (unchanged) blob is still captured so apply has no special case.
        let pdir = manifest::proposal_dir("GX-mode").unwrap();
        let blob = std::fs::read(manifest::blob_path(&pdir, &rp.slug, "run.sh")).unwrap();
        assert_eq!(blob, b"#!/bin/sh\necho hi\n");
    });
}

#[test]
fn test_symlink_is_rejected_naming_the_path() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("link");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();
        let before = snapshot(&repo_path);

        let agent = fake_agent(scripts.path(), "link.sh", "ln -s README.md link.txt");
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary = execute_propose(
            std::slice::from_ref(&repo),
            "GX-link",
            "symlink",
            &config,
            1,
        )
        .unwrap();

        assert_eq!(summary.failed, 1);
        let err = summary.repos[0].error.as_deref().unwrap();
        assert!(
            err.contains("symlink"),
            "must name the rejection kind: {err}"
        );
        assert!(err.contains("link.txt"), "must name the path: {err}");
        assert_worktree_unchanged(&repo_path, &before);

        // A rejected repo persists NO blob/patch for the symlink.
        let pdir = manifest::proposal_dir("GX-link").unwrap();
        assert!(!manifest::blob_path(&pdir, &repo.slug, "link.txt").exists());
    });
}

#[test]
fn test_non_utf8_path_is_rejected() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("badname");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let repo = Repo::new(repo_path.clone()).unwrap();
        let before = snapshot(&repo_path);

        // Create a file whose name is invalid UTF-8 (bytes 0xFF 0xFE).
        let agent = fake_agent(
            scripts.path(),
            "badname.sh",
            "f=$(printf '\\377\\376'); : > \"$f\"",
        );
        let config = llm_config(agent.to_str().unwrap(), 60);

        let summary = execute_propose(
            std::slice::from_ref(&repo),
            "GX-badname",
            "badname",
            &config,
            1,
        )
        .unwrap();

        assert_eq!(
            summary.failed, 1,
            "a non-UTF-8 path must be a loud failure, got {:?}",
            summary.repos[0].outcome
        );
        assert!(
            summary.repos[0]
                .error
                .as_deref()
                .unwrap()
                .contains("non-UTF-8"),
            "error must name the rejection: {:?}",
            summary.repos[0].error
        );
        assert_worktree_unchanged(&repo_path, &before);
    });
}

#[test]
fn test_process_single_repo_rejects_llm_change() {
    // Defensive: the deterministic per-repo pipeline must never apply a
    // Change::Llm - it is a fleet-level barrier. Reaching it is a loud error.
    // A bare `origin` is needed so the pipeline gets past get_head_branch and
    // actually reaches the change match where the defensive arm lives.
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("defensive");
        let bare = ws.path().join("defensive.git");
        run_git_command(
            &["init", "--quiet", "--bare", bare.to_str().unwrap()],
            ws.path(),
        );
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        run_git_command(
            &["remote", "add", "origin", bare.to_str().unwrap()],
            &repo_path,
        );
        let branch = crate::git::get_current_branch_name(&repo_path).unwrap();
        run_git_command(&["push", "--quiet", "-u", "origin", &branch], &repo_path);
        run_git_command(&["remote", "set-head", "origin", &branch], &repo_path);
        let repo = Repo::new(repo_path).unwrap();

        // `process_single_repo` is private to `core`; a descendant test module
        // may call it. Pass the Llm change directly to exercise the defensive arm.
        let result = super::super::process_single_repo(
            &repo,
            "GX-defensive",
            &[],
            &super::super::Change::Llm("prompt".to_string()),
            Some("msg"),
            false,
            false,
            &Config::default(),
            None,
            None,
        );
        assert!(result.error.is_some());
        assert!(
            result.error.as_deref().unwrap().contains("propose pass"),
            "error must explain the routing: {:?}",
            result.error
        );
    });
}

/// Ringer addendum #7: a crashed prior run leaves its temp worktree (and its
/// git-side registration) behind under the gx-owned tmp root. The NEXT
/// propose - for a totally unrelated change - must self-heal it before doing
/// its own work: neither the leftover directory nor its `git worktree list`
/// registration should survive.
#[test]
fn test_propose_prunes_leftover_worktree_from_a_crashed_prior_run() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("crashed");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let base_sha = crate::git::get_head_sha(&repo_path).unwrap();

        // Simulate a crashed prior run: a worktree registered under the
        // gx-owned tmp root that was never cleaned up (the process died
        // before `worktree_remove` ran).
        let tmp_root = worktree_tmp_root().unwrap();
        let leftover = tmp_root.join("wt-crashed-run");
        let leftover_wt = leftover.join("wt");
        std::fs::create_dir_all(&leftover).unwrap();
        git::worktree_add_detached(&repo_path, &leftover_wt, &base_sha).unwrap();
        assert!(leftover_wt.exists(), "sanity: leftover worktree exists");
        let list_before = run_git_command(&["worktree", "list"], &repo_path);
        assert!(
            String::from_utf8_lossy(&list_before.stdout).contains("wt-crashed-run"),
            "sanity: git must know about the leftover registration"
        );

        // A fresh, unrelated propose for the SAME repo.
        let repo = Repo::new(repo_path.clone()).unwrap();
        let agent = fake_agent(scripts.path(), "add.sh", "printf 'hello\\n' > proposed.txt");
        let config = llm_config(agent.to_str().unwrap(), 60);
        let summary = execute_propose(
            std::slice::from_ref(&repo),
            "GX-heals",
            "add a file",
            &config,
            1,
        )
        .unwrap();
        assert_eq!(summary.proposed, 1, "this run's own propose must succeed");

        // The bite: without the prune, the leftover directory AND its git
        // registration would still be there after this call.
        assert!(
            !leftover.exists(),
            "the leftover propose tmp dir must be removed"
        );
        let list_after = run_git_command(&["worktree", "list"], &repo_path);
        assert!(
            !String::from_utf8_lossy(&list_after.stdout).contains("wt-crashed-run"),
            "the leftover worktree's git registration must be pruned"
        );
    });
}

/// The prune must touch ONLY entries under the gx-owned tmp root: a worktree
/// registered elsewhere (a normal, live worktree a user or another tool
/// created) must survive a propose pass untouched.
#[test]
fn test_propose_does_not_touch_a_non_gx_worktree() {
    with_data_home(|| {
        let ws = TempDir::new().unwrap();
        let scripts = TempDir::new().unwrap();
        let repo_path = ws.path().join("org").join("liveworktree");
        init_repo(&repo_path, &[("README.md", b"# repo\n")]);
        let base_sha = crate::git::get_head_sha(&repo_path).unwrap();

        // A worktree OUTSIDE the gx tmp root - not ours to touch.
        let elsewhere = TempDir::new().unwrap();
        let other_wt = elsewhere.path().join("someone-elses-worktree");
        git::worktree_add_detached(&repo_path, &other_wt, &base_sha).unwrap();
        assert!(other_wt.exists());

        let repo = Repo::new(repo_path.clone()).unwrap();
        let agent = fake_agent(scripts.path(), "noop.sh", "exit 0");
        let config = llm_config(agent.to_str().unwrap(), 60);
        execute_propose(
            std::slice::from_ref(&repo),
            "GX-leave-others",
            "noop",
            &config,
            1,
        )
        .unwrap();

        assert!(
            other_wt.exists(),
            "a non-gx worktree directory must survive a propose pass"
        );
        let list_after = run_git_command(&["worktree", "list"], &repo_path);
        assert!(
            String::from_utf8_lossy(&list_after.stdout).contains("someone-elses-worktree"),
            "a non-gx worktree's git registration must be left alone"
        );
    });
}
