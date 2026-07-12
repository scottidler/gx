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

    let plan = build_plan(&state, &[]);
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

    let plan = build_plan(&state, std::slice::from_ref(&rec));
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

    let plan = build_plan(&state, std::slice::from_ref(&rec));
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
fn finalize_state_holds_partially_merged_when_a_merged_row_remains() {
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
            kind: OutcomeKind::MergedPendingRevert,
        },
    ];
    finalize_state(&mut state, &outcomes);
    assert_eq!(state.status, ChangeStatus::PartiallyMerged);
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
    };

    let outcome = undo_one(&plan, &Config::default());

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
