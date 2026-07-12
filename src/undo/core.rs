//! Core of `gx undo`: never prints, never prompts, takes explicit params.
//!
//! Split from `src/undo.rs` (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 3): the CLI
//! wrapper in `undo.rs` prints the plan and prompts (or honors `--yes`), then
//! calls [`plan_undo`]/[`execute_undo`] here. Where `gx rollback` restores a
//! single repo's worktree from a recovery file and NEVER touches a remote,
//! `gx undo` owns everything remote: it reconciles the recorded change state
//! against GitHub, then per repo closes the PR, deletes the pushed branch
//! (remote and local), and drains any live recovery file first. It never
//! mutates a base branch and never force-pushes; merged PRs are reverted via
//! an opened revert PR, never silently skipped and never reversed by deleting
//! shared history.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use crate::config::Config;
use crate::confirm::Confirmation;
use crate::git;
use crate::github;
use crate::lock::RepoLock;
use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus, StateManager};
use crate::transaction::{Phase, RecoveryState, Transaction};
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The campaign action for one reconciled repo, per the Architecture table.
/// Recovery-file draining is orthogonal (it runs first for every entry that
/// carries one) and is NOT encoded here.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoAction {
    /// PR open: close it, then delete the remote and local branch.
    ClosePr { pr_number: u64 },
    /// Pushed with no open PR (or a closed PR): delete remote + local branch.
    DeleteRemoteAndLocal,
    /// Committed local only (recovery-derived, never pushed): delete local branch.
    DeleteLocal,
    /// PR merged: revert it via a `revert/<change-id>` PR (Phase 6 [F4]). The
    /// base branch is NEVER touched directly and undo NEVER force-pushes.
    RequiresRevert { pr_number: Option<u64> },
    /// Merge state could NOT be verified against GitHub (the repo's org fetch
    /// failed during reconcile), so a remote-mutating action would be unsafe: a
    /// repo recorded `PrOpen`/`PrClosed`/... might actually be MERGED, and
    /// deleting its remote branch would skip the required revert. Fail closed:
    /// report it, touch NO remote, and leave the repo for a re-run online
    /// (post-audit hardening). Recovery-file draining (local-only) still runs.
    UnverifiedOffline,
    /// Already gone (cleaned up): record and skip.
    AlreadyGone,
    /// A bare (unapplied) proposal (`RepoChangeStatus::Proposed`): LOCAL-ONLY.
    /// Nothing was pushed, so there is nothing remote to reverse. Undo deletes
    /// the persisted proposal artifacts under `proposals/<change-id>/` and marks
    /// the repo `CleanedUp`, and NEVER touches a remote (design Data Model,
    /// `Proposed` undo arm). Replaces Phase 4's fail-safe `AlreadyGone` stub.
    CleanupProposal,
}

/// One repo's undo plan: the campaign action plus any live recovery files to
/// drain first.
#[derive(Debug, Clone)]
pub struct UndoPlan {
    pub slug: String,
    pub repo_path: Option<PathBuf>,
    pub branch: Option<String>,
    pub pr_number: Option<u64>,
    /// Reconciled per-repo status, `None` for a recovery-only (not-in-state) repo.
    pub status: Option<RepoChangeStatus>,
    pub action: UndoAction,
    /// Transaction ids of live recovery files for this repo, drained FIRST.
    pub recovery_tx_ids: Vec<String>,
    /// The merge commit oid of the landed PR, from the GitHub reconcile (Phase 6):
    /// drives the parent-count dispatch for the revert. `None` unless the repo's
    /// PR reconciled as `Merged`.
    pub merge_commit_oid: Option<String>,
    /// The base branch the merged PR landed on (from the reconcile). The revert
    /// branch is cut from this branch's head; `None` unless merged.
    pub base_ref_name: Option<String>,
}

/// Outcome of undoing one repo, used to render results and true up state.
#[derive(Debug, Clone)]
pub struct UndoOutcome {
    pub slug: String,
    pub pr_number: Option<u64>,
    pub kind: OutcomeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeKind {
    /// PR closed / branches deleted (and any recovery drained): mark cleaned up.
    Undone,
    /// Nothing to do (already gone): leave state untouched.
    Skipped,
    /// Merged PR reverted: a `revert/<change-id>` PR was opened (Phase 6 [F4]).
    /// Marks the row `RevertPrOpen`; the aggregate reaches `Abandoned` once every
    /// merged row is reverted.
    RevertPrOpened { pr_number: Option<u64> },
    /// Merge state could not be verified offline: reported, no remote touched,
    /// state NOT advanced so a re-run online retries (post-audit hardening).
    Unverified(String),
    /// A step failed; the error is reported and state is NOT advanced, so a
    /// re-run retries this repo.
    Failed(String),
}

/// The full reconciled plan for `gx undo <change-id>`: the whole per-repo
/// plan (for display), the actionable subset (for the confirmation count and
/// execution), and whether a change-state file existed (gates the final
/// save in [`execute_undo`]).
#[derive(Debug, Clone)]
pub struct UndoPlanSet {
    pub plan: Vec<UndoPlan>,
    pub actionable: Vec<UndoPlan>,
    pub state_existed: bool,
}

/// The org/owner portion of a repo slug (`org/repo` -> `org`).
fn org_of(repo_slug: &str) -> &str {
    repo_slug.split('/').next().unwrap_or(repo_slug)
}

/// Map a reconciled repo status + recorded PR number to a campaign action.
/// Pure and directly unit-testable.
fn classify_action(status: &RepoChangeStatus, pr_number: Option<u64>) -> UndoAction {
    match status {
        // A bare (unapplied) proposal has NOTHING remote to reverse - no branch
        // pushed, no PR. The LOCAL-ONLY arm (Phase 5): delete the proposal
        // artifacts and mark the repo CleanedUp, touching no remote.
        RepoChangeStatus::Proposed => UndoAction::CleanupProposal,
        // Already reverted (revert PR open) or cleaned up: nothing more to do.
        RepoChangeStatus::CleanedUp | RepoChangeStatus::RevertPrOpen => UndoAction::AlreadyGone,
        RepoChangeStatus::PrMerged => UndoAction::RequiresRevert { pr_number },
        RepoChangeStatus::PrOpen | RepoChangeStatus::PrDraft => match pr_number {
            Some(n) => UndoAction::ClosePr { pr_number: n },
            // Open per state but no number recorded: treat as a pushed branch.
            None => UndoAction::DeleteRemoteAndLocal,
        },
        // A pushed branch with no PR (BranchCreated), an already-closed PR whose
        // branch may linger, or a failed repo: delete the pushed branch.
        RepoChangeStatus::PrClosed | RepoChangeStatus::BranchCreated | RepoChangeStatus::Failed => {
            UndoAction::DeleteRemoteAndLocal
        }
    }
}

/// Classify a recovery-only (not-in-state) repo by its recovery `phase`,
/// honoring the design's undo table ("pushed, no PR -> delete remote branch ->
/// delete local branch"; undo OWNS all remote reversal).
///
/// A phase that records a completed push — `Pushed`/`Finalizing`, or a
/// `Pushing` stamp whose branch may already be on the remote — is a
/// pushed-no-PR entry: undo must delete the REMOTE branch then the local one.
/// The remote delete is pre-probed (`ls-remote --exit-code`), so a `Pushing`
/// crash that never actually pushed is a safe no-op, not an error. A pre-push
/// `Mutating` crash is local-only.
fn recovery_only_action(phase: Phase) -> UndoAction {
    match phase {
        Phase::Pushed | Phase::Finalizing | Phase::Pushing => UndoAction::DeleteRemoteAndLocal,
        Phase::Mutating => UndoAction::DeleteLocal,
    }
}

/// True when a plan entry has real work: a non-`AlreadyGone` action, or a
/// recovery file to drain. `AlreadyGone` with no recovery is informational.
/// `UnverifiedOffline` is actionable so it is REPORTED (never silently dropped).
fn needs_action(plan: &UndoPlan) -> bool {
    !matches!(plan.action, UndoAction::AlreadyGone) || !plan.recovery_tx_ids.is_empty()
}

/// Whether an action would MUTATE a remote (close a PR or delete a remote
/// branch). Such an action is unsafe when the repo's merge state could not be
/// verified against GitHub, because the repo might actually be merged.
fn is_remote_mutating(action: &UndoAction) -> bool {
    matches!(
        action,
        UndoAction::ClosePr { .. }
            | UndoAction::DeleteRemoteAndLocal
            | UndoAction::RequiresRevert { .. }
    )
}

/// Whether two paths refer to the same repo, comparing canonical forms when
/// both resolve and falling back to a raw comparison otherwise.
fn same_repo(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Build the per-repo undo plan from the reconciled change state plus the live
/// recovery files for this change-id (already filtered by the caller). Pure and
/// directly unit-testable. A recovery file whose repo is not in the state file
/// (crash between push and state save, F12) becomes its own committed-local-only
/// entry so it is never stranded.
fn build_plan(
    state: &ChangeState,
    recoveries: &[RecoveryState],
    merged_prs: &[github::PrInfo],
    failed_orgs: &BTreeSet<String>,
) -> Vec<UndoPlan> {
    debug!(
        "build_plan: change_id={} repos={} recoveries={} merged_prs={} failed_orgs={failed_orgs:?}",
        state.change_id,
        state.repositories.len(),
        recoveries.len(),
        merged_prs.len()
    );
    let mut used = vec![false; recoveries.len()];
    let mut plans = Vec::new();

    for repo_state in state.repositories.values() {
        let repo_path = repo_state.local_path.as_ref().map(PathBuf::from);

        let mut recovery_tx_ids = Vec::new();
        if let Some(path) = &repo_path {
            for (i, rec) in recoveries.iter().enumerate() {
                if !used[i] && same_repo(path, &rec.repo_path) {
                    used[i] = true;
                    recovery_tx_ids.push(rec.transaction_id.clone());
                }
            }
        }

        // For a merged repo, carry the merge commit oid + base branch from the
        // reconcile so the revert can dispatch on parent count and cut its branch
        // from the right base head. Match on slug; only merged PRs are supplied.
        let merged = merged_prs
            .iter()
            .find(|p| p.repo_slug == repo_state.repo_slug);

        // Fail closed when this repo's merge state is unverified: if its org
        // fetch failed AND the classified action would touch the remote, hold
        // it back as `UnverifiedOffline` rather than risk deleting the branch of
        // a PR that is actually merged (post-audit hardening).
        let mut action = classify_action(&repo_state.status, repo_state.pr_number);
        if failed_orgs.contains(org_of(&repo_state.repo_slug)) && is_remote_mutating(&action) {
            debug!(
                "build_plan: {} merge state unverified (org fetch failed) -> UnverifiedOffline",
                repo_state.repo_slug
            );
            action = UndoAction::UnverifiedOffline;
        }

        plans.push(UndoPlan {
            slug: repo_state.repo_slug.clone(),
            repo_path,
            branch: Some(repo_state.branch_name.clone()),
            pr_number: repo_state.pr_number.or_else(|| merged.map(|p| p.number)),
            status: Some(repo_state.status.clone()),
            action,
            recovery_tx_ids,
            merge_commit_oid: merged.and_then(|p| p.merge_commit_oid.clone()),
            base_ref_name: merged.map(|p| p.base_ref_name.clone()),
        });
    }

    // Recovery-only repos: recorded ONLY in a recovery file (crash between push
    // and state save, F12), never in the change state. Classify by the recovery
    // `phase` so a pushed-phase crash's REMOTE branch is not stranded: a pushed
    // recovery-only repo is a pushed-no-PR entry (delete remote -> local), a
    // pre-push `Mutating` one stays local-only. Resolve a real `org/repo` slug
    // from the checkout so the remote delete can hit the gh API — the path leaf
    // alone is not a valid repo slug.
    for (i, rec) in recoveries.iter().enumerate() {
        if used[i] {
            continue;
        }
        let leaf = rec
            .repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        let slug = crate::repo::Repo::new(rec.repo_path.clone())
            .map(|r| r.slug)
            .unwrap_or(leaf);
        plans.push(UndoPlan {
            slug,
            repo_path: Some(rec.repo_path.clone()),
            branch: rec.branch.clone(),
            pr_number: None,
            status: None,
            action: recovery_only_action(rec.phase),
            recovery_tx_ids: vec![rec.transaction_id.clone()],
            merge_commit_oid: None,
            base_ref_name: None,
        });
    }

    plans
}

/// The outcome of reconciling recorded state against GitHub.
struct Reconciliation {
    /// The merged PRs (carry the merge commit oid + base branch the revert needs).
    merged_prs: Vec<github::PrInfo>,
    /// Orgs whose PR fetch FAILED, so any repo under them has UNVERIFIED merge
    /// state. Remote-mutating undo actions for those repos are held back
    /// (post-audit hardening) rather than risking deletion of a merged PR's
    /// branch.
    failed_orgs: BTreeSet<String>,
}

/// Reconcile the recorded change state against GitHub reality before planning,
/// reusing `gx review sync`'s core so merged/closed PRs are trued up. Orgs are
/// taken from the recorded repo slugs (or an explicit `--org`); a per-org fetch
/// failure is a warning, not a hard error, so an offline undo of LOCAL-only work
/// still proceeds -- but the failed orgs are returned so that REMOTE-mutating
/// actions on their repos fail closed (a PrOpen repo whose fetch failed might
/// actually be merged; deleting its branch would skip the revert).
fn reconcile(
    change_id: &str,
    org: Option<&str>,
    state: &ChangeState,
    config: &Config,
) -> Reconciliation {
    let orgs: BTreeSet<String> = match org {
        Some(o) => std::iter::once(o.to_string()).collect(),
        None => state
            .repositories
            .values()
            .map(|r| org_of(&r.repo_slug).to_string())
            .collect(),
    };
    debug!("reconcile: change_id={change_id} orgs={orgs:?}");

    let mut all_prs = Vec::new();
    let mut failed_orgs = BTreeSet::new();
    for org in &orgs {
        match github::list_prs_by_change_id(org, change_id, config) {
            Ok(prs) => all_prs.extend(prs),
            Err(e) => {
                warn!("Failed to list PRs from org '{org}' during reconcile: {e}");
                failed_orgs.insert(org.clone());
            }
        }
    }

    if all_prs.is_empty() {
        debug!("reconcile: no PRs returned; leaving recorded state as-is");
        return Reconciliation {
            merged_prs: all_prs,
            failed_orgs,
        };
    }

    match crate::review::sync_change_state(&all_prs, change_id) {
        Ok((merged, closed, status)) => {
            info!("Reconciled {change_id}: {merged} merged, {closed} closed (status {status:?})")
        }
        Err(e) => warn!("Failed to reconcile change state for {change_id}: {e}"),
    }

    // Return only the merged PRs; they carry the merge commit oid + base branch
    // the Phase 6 revert path needs.
    let merged_prs = all_prs
        .into_iter()
        .filter(|p| p.state == github::PrState::Merged)
        .collect();
    Reconciliation {
        merged_prs,
        failed_orgs,
    }
}

/// Delete the pushed branch: the remote branch (when `remote` is set) then the
/// local branch. See [`remove_remote_branch`] / [`remove_local_branch`] for the
/// per-side existence-check semantics.
fn delete_branches(plan: &UndoPlan, config: &Config, remote: bool) -> Result<(), String> {
    if remote {
        remove_remote_branch(plan, config)?;
    }
    remove_local_branch(plan)
}

/// Delete the pushed REMOTE branch via the token-consistent gh helper.
///
/// F13: the branch is pre-probed with `git ls-remote --exit-code` first, so a
/// never-pushed or already-deleted remote branch (a 404 from `gh api ... DELETE`)
/// is a no-op for this repo, not a per-repo failure. When no local checkout is
/// available to probe from, it falls back to attempting the delete.
fn remove_remote_branch(plan: &UndoPlan, config: &Config) -> Result<(), String> {
    let branch = plan
        .branch
        .as_deref()
        .ok_or_else(|| "no branch recorded".to_string())?;

    let exists_remotely = match &plan.repo_path {
        Some(path) if crate::bare::is_git_path(path) => {
            git::remote_branch_exists_probe(path, branch)
                .map_err(|e| format!("cannot verify remote branch {branch} (offline?): {e}"))?
        }
        // No local checkout to probe from: fall back to attempting the delete
        // (the prior behavior for this edge).
        _ => true,
    };
    if exists_remotely {
        github::delete_remote_branch(&plan.slug, branch, config)
            .map_err(|e| format!("failed to delete remote branch {branch}: {e}"))?;
    } else {
        debug!(
            "remote branch {branch} already absent for {}; no-op",
            plan.slug
        );
    }
    Ok(())
}

/// Delete the local branch resolved through the recorded checkout. An
/// already-gone branch is a no-op (checked, not error-sniffed); a
/// recorded-but-missing path is reported, never silently skipped.
fn remove_local_branch(plan: &UndoPlan) -> Result<(), String> {
    let branch = plan
        .branch
        .as_deref()
        .ok_or_else(|| "no branch recorded".to_string())?;

    match &plan.repo_path {
        Some(path) if crate::bare::is_git_path(path) => {
            match git::branch_exists_locally(path, branch) {
                Ok(true) => git::delete_local_branch(path, branch)
                    .map_err(|e| format!("failed to delete local branch {branch}: {e}"))?,
                Ok(false) => {
                    debug!("local branch {branch} already gone in {}", path.display())
                }
                Err(e) => return Err(format!("failed to check local branch {branch}: {e}")),
            }
            Ok(())
        }
        Some(path) => Err(format!("recorded local path missing: {}", path.display())),
        None => Err("no local path recorded".to_string()),
    }
}

/// Undo one repo: drain any live recovery file FIRST (via the same rollback
/// interpreter `gx rollback execute` uses), then perform the campaign action,
/// all under the per-repo lock.
fn undo_one(plan: &UndoPlan, change_id: &str, config: &Config) -> UndoOutcome {
    debug!(
        "undo_one: slug={} change_id={change_id} action={:?} recoveries={}",
        plan.slug,
        plan.action,
        plan.recovery_tx_ids.len()
    );
    let outcome = |kind: OutcomeKind| UndoOutcome {
        slug: plan.slug.clone(),
        pr_number: plan.pr_number,
        kind,
    };

    // Per-repo lock covers the drain AND the campaign action, matching create.
    let _lock = match &plan.repo_path {
        Some(path) => match RepoLock::acquire(path) {
            Ok(lock) => Some(lock),
            Err(e) => return outcome(OutcomeKind::Failed(format!("repository is locked: {e}"))),
        },
        None => None,
    };

    // Ordering guarantee for a recovery-only pushed repo: the recovery file is
    // the SOLE record of this pushed branch, and the drain below removes it.
    // Delete the REMOTE branch FIRST — while that record still exists — so a
    // crash between the drain and the remote delete can never strand the remote
    // GX branch with no gx record (undo OWNS all remote reversal). Repos WITH a
    // state entry keep their record in the change-state file across the whole
    // undo, so their drain-first ordering is unaffected. The post-drain
    // `DeleteRemoteAndLocal` arm re-probes the (now-absent) remote as a no-op
    // and deletes the local branch.
    let recovery_only_remote = plan.status.is_none()
        && !plan.recovery_tx_ids.is_empty()
        && matches!(plan.action, UndoAction::DeleteRemoteAndLocal);
    if recovery_only_remote {
        if let Err(e) = remove_remote_branch(plan, config) {
            return outcome(OutcomeKind::Failed(e));
        }
    }

    // 1. Recovery-file drain FIRST (panel finding): a `mutating`-phase crash
    //    that left WIP in a recovery file must be reversed through the rollback
    //    interpreter (restoring the stash) BEFORE the branch is deleted, or the
    //    user's work is stranded in an un-recorded stash.
    for tx_id in &plan.recovery_tx_ids {
        match Transaction::execute_recovery(tx_id) {
            Ok(_) => info!("Drained recovery file {tx_id} for {}", plan.slug),
            Err(e) => {
                return outcome(OutcomeKind::Failed(format!(
                    "recovery drain failed for {tx_id}: {e}"
                )))
            }
        }
    }

    // 2. Campaign action.
    match &plan.action {
        UndoAction::AlreadyGone => outcome(OutcomeKind::Skipped),
        // A bare proposal: LOCAL-ONLY. Delete the proposal artifacts for this
        // change; touch NO remote (there is nothing pushed to reverse). Removing
        // the whole `proposals/<change-id>/` dir is idempotent, so parallel
        // per-repo workers of the same change converge (later removes no-op).
        UndoAction::CleanupProposal => {
            match crate::create::manifest::remove_proposal_dir(change_id) {
                Ok(()) => outcome(OutcomeKind::Undone),
                Err(e) => outcome(OutcomeKind::Failed(format!(
                    "failed to remove proposal artifacts: {e}"
                ))),
            }
        }
        // Fail closed: merge state could not be verified. The recovery drain
        // above (local-only) already ran; touch NO remote here and report so the
        // user re-runs online (post-audit hardening).
        UndoAction::UnverifiedOffline => outcome(OutcomeKind::Unverified(format!(
            "merge state for {} could not be verified offline; no remote action taken - re-run `gx undo {change_id}` online",
            plan.slug
        ))),
        UndoAction::RequiresRevert { .. } => outcome(revert_merged(plan, change_id, config)),
        UndoAction::ClosePr { pr_number } => {
            if let Err(e) = github::close_pr(&plan.slug, *pr_number, config) {
                return outcome(OutcomeKind::Failed(format!(
                    "failed to close PR #{pr_number}: {e}"
                )));
            }
            match delete_branches(plan, config, true) {
                Ok(()) => outcome(OutcomeKind::Undone),
                Err(e) => outcome(OutcomeKind::Failed(e)),
            }
        }
        UndoAction::DeleteRemoteAndLocal => match delete_branches(plan, config, true) {
            Ok(()) => outcome(OutcomeKind::Undone),
            Err(e) => outcome(OutcomeKind::Failed(e)),
        },
        UndoAction::DeleteLocal => match delete_branches(plan, config, false) {
            Ok(()) => outcome(OutcomeKind::Undone),
            Err(e) => outcome(OutcomeKind::Failed(e)),
        },
    }
}

/// Revert a merged PR (Phase 6 [F4]): cut a `revert/<change-id>` branch from the
/// base branch head, `git revert` the landed commit (parent-count dispatch:
/// plain revert for a single-parent squash/rebase commit, `-m 1` for a true
/// merge commit), push the branch, and open a revert PR linking the original.
/// The base branch is NEVER moved and undo NEVER force-pushes.
///
/// Collision: an existing `revert/<change-id>` branch (local OR remote) FAILS
/// this repo with a message naming the branch -- no reuse, no force, nothing
/// touched. Conflict: a revert that conflicts is REPORTED and the revert branch
/// is left mid-revert for manual resolution; undo never force-resolves.
fn revert_merged(plan: &UndoPlan, change_id: &str, config: &Config) -> OutcomeKind {
    let repo_path = match &plan.repo_path {
        Some(p) if crate::bare::is_git_path(p) => p.clone(),
        Some(p) => {
            return OutcomeKind::Failed(format!("recorded local path missing: {}", p.display()))
        }
        None => return OutcomeKind::Failed("no local path recorded".to_string()),
    };

    // Fail closed: without the merge commit oid (reconcile offline, or the PR is
    // not actually merged) we cannot revert precisely -- never guess.
    let oid =
        match &plan.merge_commit_oid {
            Some(o) => o.clone(),
            None => return OutcomeKind::Failed(
                "merged PR has no merge commit oid available (reconcile offline?); cannot revert"
                    .to_string(),
            ),
        };

    // Base branch the merge landed on; fall back to the repo's head branch when
    // the reconcile didn't supply one.
    let base = match &plan.base_ref_name {
        Some(b) => b.clone(),
        None => match git::get_head_branch(&repo_path) {
            Ok(b) => b,
            Err(e) => {
                return OutcomeKind::Failed(format!(
                    "no base branch recorded and could not determine head branch: {e}"
                ))
            }
        },
    };

    let revert_branch = format!("revert/{change_id}");
    debug!(
        "revert_merged: slug={} repo={} oid={oid} base={base} branch={revert_branch}",
        plan.slug,
        repo_path.display()
    );

    // Collision: refuse if the revert branch already exists (local OR remote).
    // No reuse, no force; touch nothing.
    match git::branch_exists_locally(&repo_path, &revert_branch) {
        Ok(true) => {
            return OutcomeKind::Failed(format!(
                "revert branch {revert_branch} already exists locally; refusing to reuse or force (delete it and re-run)"
            ))
        }
        Ok(false) => {}
        Err(e) => {
            return OutcomeKind::Failed(format!(
                "failed to check for local revert branch {revert_branch}: {e}"
            ))
        }
    }
    match git::remote_branch_exists_probe(&repo_path, &revert_branch) {
        Ok(true) => {
            return OutcomeKind::Failed(format!(
                "revert branch {revert_branch} already exists on remote; refusing to reuse or force"
            ))
        }
        Ok(false) => {}
        Err(e) => {
            // Fail closed: cannot verify remotely -> do not risk a collision.
            return OutcomeKind::Failed(format!(
                "cannot verify revert branch {revert_branch} on remote (offline?): {e}"
            ));
        }
    }

    // Cut the revert branch from the up-to-date base head the merge landed on.
    if let Err(e) = git::fetch_origin(&repo_path) {
        return OutcomeKind::Failed(format!("failed to fetch origin before revert: {e}"));
    }
    let start_point = format!("origin/{base}");
    if let Err(e) = git::create_branch_at(&repo_path, &revert_branch, &start_point) {
        return OutcomeKind::Failed(format!(
            "failed to create revert branch {revert_branch} from {start_point}: {e}"
        ));
    }

    // Parent-count dispatch: 2 parents -> true merge (`-m 1`); otherwise a
    // single-parent squash/rebase commit (plain revert). Never inferred from the
    // PR's merge method.
    let parents = match git::commit_parent_count(&repo_path, &oid) {
        Ok(n) => n,
        Err(e) => return OutcomeKind::Failed(format!("failed to count parents of {oid}: {e}")),
    };
    let mainline = if parents >= 2 { Some(1) } else { None };

    if let Err(e) = git::revert_commit(&repo_path, &oid, mainline) {
        // Conflict (or any failure): leave the branch in place for manual
        // resolution; never force-resolve.
        return OutcomeKind::Failed(format!(
            "revert of {oid} on {revert_branch} failed; branch left for manual resolution: {e}"
        ));
    }

    if let Err(e) = git::push_branch(&repo_path, &revert_branch) {
        return OutcomeKind::Failed(format!("failed to push revert branch {revert_branch}: {e}"));
    }

    match github::create_revert_pr(
        &plan.slug,
        &revert_branch,
        &base,
        change_id,
        plan.pr_number,
        config,
    ) {
        Ok(res) => {
            info!(
                "Opened revert PR #{} for {} ({})",
                res.number, plan.slug, res.url
            );
            OutcomeKind::RevertPrOpened {
                pr_number: Some(res.number),
            }
        }
        Err(e) => OutcomeKind::Failed(format!(
            "revert branch {revert_branch} pushed but failed to open revert PR: {e}"
        )),
    }
}

/// Fold the per-repo outcomes into the change state and set the aggregate:
/// `Abandoned` once every repo is resolved -- cleaned up (branch/PR removed) or,
/// for a merged row, reverted via an open revert PR (Phase 6 [F4]). On partial
/// failure the aggregate is left as reconciled so a re-run converges.
fn finalize_state(state: &mut ChangeState, outcomes: &[UndoOutcome]) {
    for o in outcomes {
        // Recovery-only repos are not in state (slug is a path leaf); the marks
        // are no-ops for them, which is correct.
        match &o.kind {
            OutcomeKind::Undone => state.mark_cleaned_up(&o.slug),
            OutcomeKind::RevertPrOpened { .. } => state.mark_revert_pr_open(&o.slug),
            _ => {}
        }
    }

    if state.repositories.is_empty() {
        state.status = ChangeStatus::Abandoned;
        return;
    }

    // Every row resolved: cleaned up, or a merged row now reverted (revert PR
    // open). A merged row whose revert FAILED stays `PrMerged` -> not resolved
    // -> the aggregate is left as reconciled so a re-run retries the revert.
    let all_resolved = state.repositories.values().all(|r| {
        matches!(
            r.status,
            RepoChangeStatus::CleanedUp | RepoChangeStatus::RevertPrOpen
        )
    });

    if all_resolved {
        state.status = ChangeStatus::Abandoned;
    }
    debug!(
        "finalize_state: change_id={} status={:?}",
        state.change_id, state.status
    );
}

/// Build the undo plan for `change_id`: reconcile recorded state against
/// GitHub, gather live recovery files, and classify each repo's campaign
/// action. Returns `None` when nothing is recorded at all (no change state
/// AND no recovery files) - the caller reports "nothing to undo" without ever
/// printing a plan header. Never prints.
pub fn plan_undo(
    change_id: &str,
    org: Option<&str>,
    config: &Config,
) -> Result<Option<UndoPlanSet>> {
    debug!("plan_undo: change_id={change_id} org={org:?}");
    let manager = StateManager::new()?;

    // Gather live recovery files for this change-id FIRST (F12): a pushed branch
    // may be recorded ONLY in a recovery file, with NO change-state file at all
    // (crash between push and state save). Undo must still reverse such a repo,
    // remote included, so it is never stranded.
    let recoveries: Vec<RecoveryState> = Transaction::list_recovery_states()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.change_id == change_id)
        .collect();

    let state_existed = manager.load(change_id)?.is_some();
    if !state_existed && recoveries.is_empty() {
        // A fully-absent change: nothing to reverse. Idempotent no-op (a second
        // undo of a recovery-only campaign lands here once the recovery files
        // have been drained), matching the "already gone" convergence policy.
        return Ok(None);
    }

    // Reconcile against GitHub reality (Phase 4 sync) only when a state file
    // exists — reconcile needs the recorded repos/orgs, and a recovery-only
    // campaign is local by nature. The merged PRs carry the merge commit oid +
    // base branch the revert path needs.
    let (merged_prs, failed_orgs) = if state_existed {
        let state = manager.load(change_id)?.ok_or_else(|| {
            eyre::eyre!("Change state for {change_id} disappeared before reconcile")
        })?;
        // A change composed ENTIRELY of bare proposals has nothing pushed to any
        // remote, so there is nothing to reconcile against GitHub. Skip the
        // round-trip so `gx undo` of a bare proposal is provably local-only -
        // ZERO gh invocations (design `Proposed` undo arm). A mix (some applied,
        // some still proposed) still reconciles for the applied repos.
        let has_remote_work = state
            .repositories
            .values()
            .any(|r| r.status != RepoChangeStatus::Proposed);
        if has_remote_work {
            let Reconciliation {
                merged_prs,
                failed_orgs,
            } = reconcile(change_id, org, &state, config);
            (merged_prs, failed_orgs)
        } else {
            debug!(
                "plan_undo: {change_id} is all bare proposals; skipping remote reconcile (local-only undo)"
            );
            (Vec::new(), BTreeSet::new())
        }
    } else {
        (Vec::new(), BTreeSet::new())
    };

    // Freshest state (post-reconcile) for planning, or an empty stand-in for a
    // recovery-only campaign so `build_plan` produces only recovery-only entries.
    let state = match manager.load(change_id)? {
        Some(s) => s,
        None => ChangeState::new(change_id.to_string(), None),
    };

    let plan = build_plan(&state, &recoveries, &merged_prs, &failed_orgs);
    let actionable: Vec<UndoPlan> = plan.iter().filter(|&x| needs_action(x)).cloned().collect();

    Ok(Some(UndoPlanSet {
        plan,
        actionable,
        state_existed,
    }))
}

/// Execute the actionable subset of an undo plan in parallel, then fold the
/// outcomes into a fresh load of the change state and save (only when the
/// change existed). Never prints, never prompts - the caller (the CLI
/// wrapper today; an MCP `undo-execute` tool later) already confirmed (TTY,
/// `--yes`, or a verified token) before calling this.
pub fn execute_undo(
    plan_set: &UndoPlanSet,
    change_id: &str,
    config: &Config,
    parallel_jobs: usize,
    confirmation: Confirmation,
) -> Result<Vec<UndoOutcome>> {
    debug!(
        "execute_undo: change_id={change_id} actionable={} state_existed={} confirmation={confirmation:?}",
        plan_set.actionable.len(),
        plan_set.state_existed
    );
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    let outcomes: Vec<UndoOutcome> = pool.install(|| {
        plan_set
            .actionable
            .par_iter()
            .map(|p| undo_one(p, change_id, config))
            .collect()
    });

    // Change-level lock (Phase 7 [F6]): reload the freshest state, fold in
    // this run's outcomes, and save -- all as one atomic critical section --
    // so a concurrent `review sync`/`cleanup`/another `undo` on the SAME
    // change-id can never interleave and lose an update. The lock is held
    // only for this final load-mutate-save, not across the (possibly long)
    // campaign execution above, which never touches `changes/<id>.json`
    // directly. Skipped for a recovery-only campaign: there is no change-state
    // file to true up (its record was the recovery files, now drained), and
    // saving one would fabricate a spurious state file for a change that never
    // had one.
    if plan_set.state_existed {
        let manager = StateManager::new()?;
        let _change_lock = crate::lock::ChangeLock::acquire(change_id)
            .context("Failed to acquire change lock before saving undo results")?;
        let mut state = manager
            .load(change_id)?
            .ok_or_else(|| eyre::eyre!("Change state for {change_id} disappeared during undo"))?;
        finalize_state(&mut state, &outcomes);
        manager
            .save(&state)
            .context("Failed to save change state after undo")?;

        // Retention (design Data Model): the proposal dir is removed by `gx undo`.
        // Once the change is fully resolved (`Abandoned`), drop any proposal
        // artifacts - covers both a bare-proposal campaign (whose per-repo
        // CleanupProposal already removed them; this is an idempotent no-op) and
        // an APPLIED llm campaign whose branches/PRs were just reversed.
        if state.status == ChangeStatus::Abandoned {
            if let Err(e) = crate::create::manifest::remove_proposal_dir(change_id) {
                warn!("Failed to remove proposal artifacts for {change_id} after undo: {e}");
            }
        }
    }

    Ok(outcomes)
}

#[cfg(test)]
mod tests;
