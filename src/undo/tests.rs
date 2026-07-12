use super::*;
use crate::config::Config;
use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus};
use crate::test_utils::run_git_command;
use crate::transaction::{Phase, RecoveryState, RollbackStep, StepEntry};
use std::fs;
use tempfile::TempDir;

// ---- classify_action: pure state -> action mapping ----

#[test]
fn classify_open_pr_with_number_closes_pr() {
    assert_eq!(
        classify_action(&RepoChangeStatus::PrOpen, Some(7)),
        UndoAction::ClosePr { pr_number: 7 }
    );
    assert_eq!(
        classify_action(&RepoChangeStatus::PrDraft, Some(9)),
        UndoAction::ClosePr { pr_number: 9 }
    );
}

#[test]
fn classify_open_pr_without_number_falls_back_to_branch_delete() {
    assert_eq!(
        classify_action(&RepoChangeStatus::PrOpen, None),
        UndoAction::DeleteRemoteAndLocal
    );
}

#[test]
fn classify_pushed_no_pr_deletes_remote_and_local() {
    assert_eq!(
        classify_action(&RepoChangeStatus::BranchCreated, None),
        UndoAction::DeleteRemoteAndLocal
    );
}

#[test]
fn classify_closed_pr_deletes_remote_and_local() {
    assert_eq!(
        classify_action(&RepoChangeStatus::PrClosed, Some(3)),
        UndoAction::DeleteRemoteAndLocal
    );
}

#[test]
fn classify_merged_requires_revert_never_deletes() {
    // Load-bearing: a merged PR is REPORTED for revert (Phase 6), never
    // reversed by deleting shared history.
    assert_eq!(
        classify_action(&RepoChangeStatus::PrMerged, Some(42)),
        UndoAction::RequiresRevert {
            pr_number: Some(42)
        }
    );
}

#[test]
fn classify_cleaned_up_is_already_gone() {
    assert_eq!(
        classify_action(&RepoChangeStatus::CleanedUp, None),
        UndoAction::AlreadyGone
    );
}

// ---- build_plan: state + recovery association ----

#[test]
fn build_plan_maps_each_repo_to_its_action() {
    let mut state = ChangeState::new("GX-plan".to_string(), None);
    state.add_repository("org/open".to_string(), "GX-plan".to_string());
    state.set_pr_info(
        "org/open",
        11,
        "https://github.com/org/open/pull/11".to_string(),
        false,
    );
    state.add_repository("org/merged".to_string(), "GX-plan".to_string());
    state.mark_merged("org/merged");
    state.add_repository("org/pushed".to_string(), "GX-plan".to_string());

    let plan = build_plan(&state, &[], &[]);
    assert_eq!(plan.len(), 3);

    let by_slug = |s: &str| plan.iter().find(|p| p.slug == s).unwrap().action.clone();
    assert_eq!(by_slug("org/open"), UndoAction::ClosePr { pr_number: 11 });
    assert_eq!(
        by_slug("org/merged"),
        UndoAction::RequiresRevert {
            pr_number: None // set_pr_info wasn't called for merged; number stays None
        }
    );
    assert_eq!(by_slug("org/pushed"), UndoAction::DeleteRemoteAndLocal);
}

#[test]
fn build_plan_associates_recovery_file_by_path() {
    let repo_dir = TempDir::new().unwrap();
    let repo_path = repo_dir.path().to_path_buf();

    let mut state = ChangeState::new("GX-assoc".to_string(), None);
    state.add_repository("org/repo".to_string(), "GX-assoc".to_string());
    if let Some(rs) = state.repositories.get_mut("org/repo") {
        rs.local_path = Some(repo_path.to_string_lossy().to_string());
    }

    let rec = RecoveryState {
        version: 1,
        transaction_id: "gx-tx-assoc-1".to_string(),
        change_id: "GX-assoc".to_string(),
        repo_path: repo_path.clone(),
        created_at: "2026-07-11T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: Some("GX-assoc".to_string()),
        steps: vec![],
    };

    let plan = build_plan(&state, std::slice::from_ref(&rec), &[]);
    assert_eq!(
        plan.len(),
        1,
        "recovery must attach to the state repo, not add one"
    );
    assert_eq!(plan[0].recovery_tx_ids, vec!["gx-tx-assoc-1".to_string()]);
}

#[test]
fn build_plan_adds_recovery_only_repo_as_committed_local_only() {
    // F12: a pushed branch may be recorded ONLY in a recovery file (crash
    // between push and state save). It must become its own committed-local-only
    // entry, never stranded.
    let repo_dir = TempDir::new().unwrap();
    let state = ChangeState::new("GX-orphan".to_string(), None);

    let rec = RecoveryState {
        version: 1,
        transaction_id: "gx-tx-orphan-1".to_string(),
        change_id: "GX-orphan".to_string(),
        repo_path: repo_dir.path().to_path_buf(),
        created_at: "2026-07-11T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: Some("GX-orphan".to_string()),
        steps: vec![],
    };

    let plan = build_plan(&state, std::slice::from_ref(&rec), &[]);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].action, UndoAction::DeleteLocal);
    assert_eq!(plan[0].recovery_tx_ids, vec!["gx-tx-orphan-1".to_string()]);
    assert!(plan[0].status.is_none());
}

#[test]
fn needs_action_skips_already_gone_without_recovery() {
    let gone = UndoPlan {
        slug: "org/repo".to_string(),
        repo_path: None,
        branch: None,
        pr_number: None,
        status: Some(RepoChangeStatus::CleanedUp),
        action: UndoAction::AlreadyGone,
        recovery_tx_ids: vec![],
        merge_commit_oid: None,
        base_ref_name: None,
    };
    assert!(!needs_action(&gone));

    let gone_with_recovery = UndoPlan {
        recovery_tx_ids: vec!["tx".to_string()],
        ..gone.clone()
    };
    assert!(needs_action(&gone_with_recovery));
}

// ---- finalize_state: aggregate transitions ----

#[test]
fn finalize_state_abandons_when_all_undone_and_none_merged() {
    let mut state = ChangeState::new("GX-fin".to_string(), None);
    state.add_repository("org/a".to_string(), "GX-fin".to_string());
    state.add_repository("org/b".to_string(), "GX-fin".to_string());

    let outcomes = vec![
        UndoOutcome {
            slug: "org/a".to_string(),
            pr_number: None,
            kind: OutcomeKind::Undone,
        },
        UndoOutcome {
            slug: "org/b".to_string(),
            pr_number: None,
            kind: OutcomeKind::Undone,
        },
    ];
    finalize_state(&mut state, &outcomes);
    assert_eq!(state.status, ChangeStatus::Abandoned);
    assert_eq!(
        state.repositories.get("org/a").unwrap().status,
        RepoChangeStatus::CleanedUp
    );
}

#[test]
fn finalize_state_abandons_when_merged_row_reverted() {
    // Phase 6 [F4] (inverts the Phase 5 hold-at-PartiallyMerged behavior): a
    // merged row that got a revert PR opened resolves to `RevertPrOpen`, and once
    // every row is resolved the aggregate reaches `Abandoned` -- undo is done.
    let mut state = ChangeState::new("GX-fin2".to_string(), None);
    state.add_repository("org/a".to_string(), "GX-fin2".to_string());
    state.add_repository("org/merged".to_string(), "GX-fin2".to_string());
    state.mark_merged("org/merged");

    let outcomes = vec![
        UndoOutcome {
            slug: "org/a".to_string(),
            pr_number: None,
            kind: OutcomeKind::Undone,
        },
        UndoOutcome {
            slug: "org/merged".to_string(),
            pr_number: Some(5),
            kind: OutcomeKind::RevertPrOpened {
                pr_number: Some(77),
            },
        },
    ];
    finalize_state(&mut state, &outcomes);
    assert_eq!(
        state.repositories.get("org/merged").unwrap().status,
        RepoChangeStatus::RevertPrOpen,
        "a reverted merged row must be marked RevertPrOpen"
    );
    assert_eq!(state.status, ChangeStatus::Abandoned);
}

#[test]
fn finalize_state_holds_when_merged_revert_failed() {
    // A merged row whose revert FAILED stays `PrMerged` (not resolved), so the
    // aggregate must NOT flip to Abandoned -- a re-run retries the revert.
    let mut state = ChangeState::new("GX-fin2b".to_string(), None);
    state.add_repository("org/a".to_string(), "GX-fin2b".to_string());
    state.add_repository("org/merged".to_string(), "GX-fin2b".to_string());
    state.mark_merged("org/merged");
    let reconciled = state.status.clone();

    let outcomes = vec![
        UndoOutcome {
            slug: "org/a".to_string(),
            pr_number: None,
            kind: OutcomeKind::Undone,
        },
        UndoOutcome {
            slug: "org/merged".to_string(),
            pr_number: Some(5),
            kind: OutcomeKind::Failed("revert conflict".to_string()),
        },
    ];
    finalize_state(&mut state, &outcomes);
    assert_eq!(
        state.repositories.get("org/merged").unwrap().status,
        RepoChangeStatus::PrMerged,
        "a failed revert must leave the row PrMerged for a retry"
    );
    assert_eq!(
        state.status, reconciled,
        "a failed revert must not flip the aggregate to Abandoned"
    );
}

#[test]
fn finalize_state_leaves_aggregate_on_partial_failure() {
    // A failed repo is not advanced; the aggregate must NOT claim Abandoned so a
    // re-run converges.
    let mut state = ChangeState::new("GX-fin3".to_string(), None);
    state.add_repository("org/a".to_string(), "GX-fin3".to_string());
    state.add_repository("org/b".to_string(), "GX-fin3".to_string());
    state.status = ChangeStatus::PrsCreated;

    let outcomes = vec![
        UndoOutcome {
            slug: "org/a".to_string(),
            pr_number: None,
            kind: OutcomeKind::Undone,
        },
        UndoOutcome {
            slug: "org/b".to_string(),
            pr_number: None,
            kind: OutcomeKind::Failed("boom".to_string()),
        },
    ];
    finalize_state(&mut state, &outcomes);
    assert_eq!(
        state.status,
        ChangeStatus::PrsCreated,
        "partial failure must not flip the aggregate to Abandoned"
    );
}

// ---- undo_one: recovery drain happens BEFORE the branch delete (success criterion) ----

#[test]
fn undo_one_drains_mutating_recovery_stash_before_deleting_branch() {
    // Phase 5 success criterion (load-bearing panel finding): a `mutating`-phase
    // recovery file in the campaign is DRAINED (its stash restored via the SAME
    // rollback interpreter) BEFORE the repo's branch is deleted. Without the
    // drain, the WIP would be stranded in an un-recorded stash.
    let guard = crate::test_utils::ENV_LOCK.lock().unwrap();
    let prior_data_home = std::env::var("XDG_DATA_HOME").ok();

    let data_home = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

    // A repo on `main` with one commit, plus a GX branch to be undone.
    let repo_dir = TempDir::new().unwrap();
    let repo = repo_dir.path();
    run_git_command(&["init", "--quiet", "-b", "main"], repo);
    run_git_command(&["config", "user.email", "t@e.com"], repo);
    run_git_command(&["config", "user.name", "T"], repo);
    run_git_command(&["config", "commit.gpgsign", "false"], repo);
    fs::write(repo.join("README.md"), "# r\n").unwrap();
    run_git_command(&["add", "-A"], repo);
    run_git_command(&["commit", "--quiet", "-m", "init"], repo);
    run_git_command(&["branch", "GX-drain"], repo);

    // Uncommitted WIP, then stash it (as gx create would) - the worktree reverts
    // to HEAD, so the WIP file is gone until recovery restores it.
    fs::write(repo.join("wip.txt"), "precious work in progress").unwrap();
    let stash_sha =
        crate::git::stash_save_with_untracked(repo, "GX auto-stash for GX-drain").unwrap();
    assert!(
        !repo.join("wip.txt").exists(),
        "stash should have removed the WIP from the worktree"
    );

    // Hand-author a live `mutating`-phase recovery file (Phase 8's crash hook
    // is not landed yet, so this is authored like Phase 2's fixtures).
    let tx_id = "gx-tx-drain-1";
    let rec = RecoveryState {
        version: 1,
        transaction_id: tx_id.to_string(),
        change_id: "GX-drain".to_string(),
        repo_path: repo.to_path_buf(),
        created_at: "2026-07-11T00:00:00Z".to_string(),
        phase: Phase::Mutating,
        branch: Some("GX-drain".to_string()),
        steps: vec![StepEntry::pending(RollbackStep::PopStash {
            repo: repo.to_path_buf(),
            stash_sha,
        })],
    };
    let recovery_dir = data_home.path().join("gx").join("recovery");
    fs::create_dir_all(&recovery_dir).unwrap();
    let recovery_file = recovery_dir.join(format!("{tx_id}.json"));
    fs::write(&recovery_file, serde_json::to_string_pretty(&rec).unwrap()).unwrap();

    // The campaign entry for this repo (committed-local-only) carries the
    // recovery file to drain first, then deletes the local branch.
    let plan = UndoPlan {
        slug: "repo".to_string(),
        repo_path: Some(repo.to_path_buf()),
        branch: Some("GX-drain".to_string()),
        pr_number: None,
        status: None,
        action: UndoAction::DeleteLocal,
        recovery_tx_ids: vec![tx_id.to_string()],
        merge_commit_oid: None,
        base_ref_name: None,
    };

    let outcome = undo_one(&plan, "GX-drain", &Config::default());

    assert_eq!(outcome.kind, OutcomeKind::Undone, "undo should succeed");
    // The stash was restored BEFORE the branch was deleted: the WIP is back.
    assert_eq!(
        fs::read_to_string(repo.join("wip.txt")).unwrap(),
        "precious work in progress",
        "recovery drain must restore the stashed WIP"
    );
    // The recovery file was consumed by the drain (full reverse completed).
    assert!(
        !recovery_file.exists(),
        "drained recovery file should be removed"
    );
    // The GX branch was deleted by the campaign action.
    assert!(
        !crate::git::branch_exists_locally(repo, "GX-drain").unwrap(),
        "the local GX branch should be deleted"
    );

    match prior_data_home {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

// ---- Phase 7 [F13]: undo tolerates an already-absent remote branch ----

#[test]
#[cfg(unix)]
fn undo_one_treats_never_pushed_remote_branch_as_no_op() {
    // Phase 5 explicitly deferred this to Phase 7: a branch that was never
    // pushed (or already deleted remotely) must not fail the whole repo.
    // `delete_branches` pre-probes with `git ls-remote --exit-code` BEFORE
    // ever calling `github::delete_remote_branch` (a `gh api ... DELETE` that
    // 404s on an absent ref) -- proven here with a `gh` shim that fails loudly
    // if invoked at all: the probe must skip it entirely.
    use std::os::unix::fs::PermissionsExt;

    let guard = crate::test_utils::ENV_LOCK.lock().unwrap();
    let prior_data_home = std::env::var("XDG_DATA_HOME").ok();
    let prior_path = std::env::var("PATH").ok();

    let data_home = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

    let remotes = TempDir::new().unwrap();
    let bare = remotes.path().join("repo.git");
    run_git_command(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes.path(),
    );

    let repo_dir = TempDir::new().unwrap();
    let repo = repo_dir.path();
    run_git_command(&["init", "--quiet", "-b", "main"], repo);
    run_git_command(&["config", "user.email", "t@e.com"], repo);
    run_git_command(&["config", "user.name", "T"], repo);
    run_git_command(&["config", "commit.gpgsign", "false"], repo);
    std::fs::write(repo.join("README.md"), "# r\n").unwrap();
    run_git_command(&["add", "-A"], repo);
    run_git_command(&["commit", "--quiet", "-m", "init"], repo);
    run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], repo);
    run_git_command(&["push", "--quiet", "-u", "origin", "main"], repo);
    // The GX branch exists LOCALLY only -- never pushed to `origin`.
    run_git_command(&["branch", "GX-neverpushed"], repo);

    // A `gh` shim that fails loudly if invoked -- proves the probe skips it.
    let shim_dir = TempDir::new().unwrap();
    let gh = shim_dir.path().join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\necho 'gh should not be called for an absent remote branch' >&2\nexit 1\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&gh).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&gh, perms).unwrap();
    let new_path = format!(
        "{}:{}",
        shim_dir.path().display(),
        prior_path.clone().unwrap_or_default()
    );
    unsafe { std::env::set_var("PATH", &new_path) };

    let plan = UndoPlan {
        slug: "org/repo".to_string(),
        repo_path: Some(repo.to_path_buf()),
        branch: Some("GX-neverpushed".to_string()),
        pr_number: None,
        status: Some(RepoChangeStatus::BranchCreated),
        action: UndoAction::DeleteRemoteAndLocal,
        recovery_tx_ids: vec![],
        merge_commit_oid: None,
        base_ref_name: None,
    };

    let outcome = undo_one(&plan, "GX-neverpushed", &Config::default());

    assert_eq!(
        outcome.kind,
        OutcomeKind::Undone,
        "a never-pushed remote branch must be a no-op, not a failure: {:?}",
        outcome.kind
    );
    assert!(
        !crate::git::branch_exists_locally(repo, "GX-neverpushed").unwrap(),
        "the local branch should still be deleted"
    );

    match prior_path {
        Some(v) => unsafe { std::env::set_var("PATH", v) },
        None => unsafe { std::env::remove_var("PATH") },
    }
    match prior_data_home {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

// ---- Phase 6: merged-PR revert path (success criteria) ----

/// A `gh` PATH shim that answers only `pr create` with a fixed PR URL (the revert
/// PR). Any other invocation fails loudly, so the test proves exactly one gh call
/// shape is made. Real git push still hits the real bare remote.
#[cfg(unix)]
const REVERT_GH_SHIM: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  echo "https://github.com/acme/repo/pull/555"
  exit 0
fi
echo "revert gh shim: unexpected invocation: $*" >&2
exit 1
"#;

/// Write the revert gh shim (executable) to `dir/gh`.
#[cfg(unix)]
fn write_revert_shim(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let gh = dir.join("gh");
    fs::write(&gh, REVERT_GH_SHIM).unwrap();
    let mut perms = fs::metadata(&gh).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&gh, perms).unwrap();
}

/// Create a repo on `main` with data.md = "old value", wired to a fresh bare
/// remote and pushed. Returns (repo_path, bare_path).
#[cfg(unix)]
fn repo_with_remote(
    workspace: &std::path::Path,
    remotes: &std::path::Path,
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let bare = remotes.join(format!("{name}.git"));
    run_git_command(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );

    let repo = workspace.join(name);
    fs::create_dir_all(&repo).unwrap();
    run_git_command(&["init", "--quiet", "--initial-branch=main"], &repo);
    run_git_command(&["config", "user.email", "t@e.com"], &repo);
    run_git_command(&["config", "user.name", "T"], &repo);
    run_git_command(&["config", "commit.gpgsign", "false"], &repo);
    fs::write(repo.join("data.md"), "old value\n").unwrap();
    run_git_command(&["add", "-A"], &repo);
    run_git_command(&["commit", "--quiet", "-m", "init"], &repo);
    run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    run_git_command(&["push", "--quiet", "-u", "origin", "main"], &repo);
    (repo, bare)
}

/// The full oid of `refname` in `repo`.
#[cfg(unix)]
fn rev_parse(repo: &std::path::Path, refname: &str) -> String {
    let out = run_git_command(&["rev-parse", refname], repo);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run `undo_one` for a merged (revert) plan with the gh shim on PATH and an
/// isolated XDG_DATA_HOME (RepoLock writes there). Returns the outcome.
#[cfg(unix)]
fn run_revert(
    repo: &std::path::Path,
    change_id: &str,
    oid: &str,
    shim_dir: &std::path::Path,
    data_home: &std::path::Path,
) -> OutcomeKind {
    let plan = UndoPlan {
        slug: "acme/repo".to_string(),
        repo_path: Some(repo.to_path_buf()),
        branch: Some(change_id.to_string()),
        pr_number: Some(42),
        status: Some(RepoChangeStatus::PrMerged),
        action: UndoAction::RequiresRevert {
            pr_number: Some(42),
        },
        recovery_tx_ids: vec![],
        merge_commit_oid: Some(oid.to_string()),
        base_ref_name: Some("main".to_string()),
    };

    let prior_path = std::env::var("PATH").ok();
    let prior_data = std::env::var("XDG_DATA_HOME").ok();
    let new_path = format!(
        "{}:{}",
        shim_dir.display(),
        prior_path.clone().unwrap_or_default()
    );
    unsafe {
        std::env::set_var("PATH", &new_path);
        std::env::set_var("XDG_DATA_HOME", data_home);
    }

    let outcome = undo_one(&plan, change_id, &Config::default());

    match prior_path {
        Some(v) => unsafe { std::env::set_var("PATH", v) },
        None => unsafe { std::env::remove_var("PATH") },
    }
    match prior_data {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    outcome.kind
}

#[test]
#[cfg(unix)]
fn revert_squash_merge_opens_inverse_revert_pr() {
    // Success criterion: a merged-PR fixture (squash = ONE parent) produces a
    // revert PR whose diff is the INVERSE of the original. The single squash
    // commit changed "old value" -> "new value"; the revert branch must restore
    // "old value", and the parent-count dispatch must pick a plain `git revert`.
    let guard = crate::test_utils::ENV_LOCK.lock().unwrap();

    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let shim_dir = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    write_revert_shim(shim_dir.path());

    let (repo, bare) = repo_with_remote(workspace.path(), remotes.path(), "repo");

    // The landed squash commit: a single-parent commit on main, pushed.
    fs::write(repo.join("data.md"), "new value\n").unwrap();
    run_git_command(&["add", "-A"], &repo);
    run_git_command(&["commit", "--quiet", "-m", "GX-sq: old to new"], &repo);
    let oid = rev_parse(&repo, "HEAD");
    run_git_command(&["push", "--quiet", "origin", "main"], &repo);

    // Sanity: the merge commit really has one parent (dispatch input).
    assert_eq!(
        crate::git::commit_parent_count(&repo, &oid).unwrap(),
        1,
        "squash commit must have exactly one parent"
    );

    let base_before = rev_parse_bare(&bare, "refs/heads/main");
    let change_id = "GX-sq";
    let kind = run_revert(&repo, change_id, &oid, shim_dir.path(), data_home.path());

    assert!(
        matches!(kind, OutcomeKind::RevertPrOpened { .. }),
        "squash revert should open a revert PR, got {kind:?}"
    );

    // The revert branch content is the INVERSE of the original change: back to
    // "old value".
    let revert_branch = format!("revert/{change_id}");
    let content = run_git_command(&["show", &format!("{revert_branch}:data.md")], &repo);
    assert_eq!(
        String::from_utf8_lossy(&content.stdout),
        "old value\n",
        "revert must restore the pre-change content (inverse diff)"
    );
    // The revert branch was pushed to the bare remote.
    assert!(
        branch_on_bare(&bare, &revert_branch),
        "revert branch must be pushed to the remote"
    );
    // The base branch was NEVER touched.
    assert_eq!(
        rev_parse_bare(&bare, "refs/heads/main"),
        base_before,
        "undo must never move the base branch"
    );

    drop(guard);
}

#[test]
#[cfg(unix)]
fn revert_true_merge_uses_dash_m_one() {
    // Success criterion: a true-merge fixture (TWO parents) reverts with `-m 1`.
    // A plain `git revert` on a merge commit fails ("is a merge but no -m option
    // given"), so a successful revert here PROVES the parent-count dispatch chose
    // `-m 1`. The revert restores the base's pre-merge state ("old value").
    let guard = crate::test_utils::ENV_LOCK.lock().unwrap();

    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let shim_dir = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    write_revert_shim(shim_dir.path());

    let (repo, bare) = repo_with_remote(workspace.path(), remotes.path(), "repo");

    // A true merge commit: feature branch changes the file, merged --no-ff into
    // main so the merge commit has two parents (parent 1 = main pre-merge).
    run_git_command(&["checkout", "--quiet", "-b", "feature"], &repo);
    fs::write(repo.join("data.md"), "new value\n").unwrap();
    run_git_command(&["add", "-A"], &repo);
    run_git_command(&["commit", "--quiet", "-m", "feature: old to new"], &repo);
    run_git_command(&["checkout", "--quiet", "main"], &repo);
    run_git_command(
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            "GX-mrg: merge feature",
            "feature",
        ],
        &repo,
    );
    let oid = rev_parse(&repo, "HEAD");
    run_git_command(&["push", "--quiet", "origin", "main"], &repo);

    assert_eq!(
        crate::git::commit_parent_count(&repo, &oid).unwrap(),
        2,
        "true merge commit must have two parents"
    );

    let change_id = "GX-mrg";
    let kind = run_revert(&repo, change_id, &oid, shim_dir.path(), data_home.path());

    assert!(
        matches!(kind, OutcomeKind::RevertPrOpened { .. }),
        "true-merge revert with -m 1 should succeed and open a revert PR, got {kind:?}"
    );

    let revert_branch = format!("revert/{change_id}");
    let content = run_git_command(&["show", &format!("{revert_branch}:data.md")], &repo);
    assert_eq!(
        String::from_utf8_lossy(&content.stdout),
        "old value\n",
        "reverting the merge with -m 1 must restore the base's pre-merge content"
    );
    assert!(
        branch_on_bare(&bare, &revert_branch),
        "revert branch must be pushed to the remote"
    );

    drop(guard);
}

#[test]
#[cfg(unix)]
fn revert_collision_existing_branch_fails_and_touches_nothing() {
    // Success criterion: a pre-existing `revert/<change-id>` branch FAILS that
    // repo with a message naming the branch, and touches nothing (no reuse, no
    // force). No revert branch is pushed to the remote.
    let guard = crate::test_utils::ENV_LOCK.lock().unwrap();

    let workspace = TempDir::new().unwrap();
    let remotes = TempDir::new().unwrap();
    let shim_dir = TempDir::new().unwrap();
    let data_home = TempDir::new().unwrap();
    write_revert_shim(shim_dir.path());

    let (repo, bare) = repo_with_remote(workspace.path(), remotes.path(), "repo");

    fs::write(repo.join("data.md"), "new value\n").unwrap();
    run_git_command(&["add", "-A"], &repo);
    run_git_command(&["commit", "--quiet", "-m", "GX-col: old to new"], &repo);
    let oid = rev_parse(&repo, "HEAD");
    run_git_command(&["push", "--quiet", "origin", "main"], &repo);

    let change_id = "GX-col";
    let revert_branch = format!("revert/{change_id}");

    // Pre-create the revert branch locally: the collision case.
    run_git_command(&["branch", &revert_branch, "main"], &repo);
    let collided_sha = rev_parse(&repo, &revert_branch);

    let kind = run_revert(&repo, change_id, &oid, shim_dir.path(), data_home.path());

    match kind {
        OutcomeKind::Failed(msg) => {
            assert!(
                msg.contains(&revert_branch) && msg.contains("already exists"),
                "collision failure must name the branch, got: {msg}"
            );
        }
        other => panic!("collision must fail, got {other:?}"),
    }

    // Nothing touched: the pre-existing branch is unchanged, no new commits, and
    // the remote never received a revert branch.
    assert_eq!(
        rev_parse(&repo, &revert_branch),
        collided_sha,
        "the pre-existing revert branch must be left untouched"
    );
    assert!(
        !branch_on_bare(&bare, &revert_branch),
        "a collision must not push any revert branch to the remote"
    );

    drop(guard);
}

/// The full oid of `refname` in a bare remote.
#[cfg(unix)]
fn rev_parse_bare(bare: &std::path::Path, refname: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["--git-dir", bare.to_str().unwrap(), "rev-parse", refname])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Whether `branch` exists in the bare remote.
#[cfg(unix)]
fn branch_on_bare(bare: &std::path::Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}
