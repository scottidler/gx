//! Campaign-level undo (`gx undo <change-id>`, F4).
//!
//! Where `gx rollback` restores a single repo's worktree from a recovery file
//! and NEVER touches a remote, `gx undo` owns everything remote: it reconciles
//! the recorded change state against GitHub, then per repo closes the PR,
//! deletes the pushed branch (remote and local), and drains any live recovery
//! file first. It never mutates a base branch and never force-pushes; merged
//! PRs are reported as requiring a revert (that revert path lands in Phase 6),
//! never silently skipped and never reversed by deleting shared history.
//!
//! Sources are the change-state file PLUS any recovery files carrying the same
//! change-id (covering a crash between push and state save, F12). Local repos
//! are resolved via the recorded `local_path`; a missing path is reported, not
//! skipped.

use crate::cli::Cli;
use crate::config::Config;
use crate::git;
use crate::github;
use crate::lock::RepoLock;
use crate::output::{display_review_results, StatusOptions};
use crate::repo::Repo;
use crate::review::{ReviewAction, ReviewResult};
use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus, StateManager};
use crate::transaction::{RecoveryState, Transaction};
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The campaign action for one reconciled repo, per the Architecture table.
/// Recovery-file draining is orthogonal (it runs first for every entry that
/// carries one) and is NOT encoded here.
#[derive(Debug, Clone, PartialEq)]
enum UndoAction {
    /// PR open: close it, then delete the remote and local branch.
    ClosePr { pr_number: u64 },
    /// Pushed with no open PR (or a closed PR): delete remote + local branch.
    DeleteRemoteAndLocal,
    /// Committed local only (recovery-derived, never pushed): delete local branch.
    DeleteLocal,
    /// PR merged: revert it via a `revert/<change-id>` PR (Phase 6 [F4]). The
    /// base branch is NEVER touched directly and undo NEVER force-pushes.
    RequiresRevert { pr_number: Option<u64> },
    /// Already gone (cleaned up): record and skip.
    AlreadyGone,
}

/// One repo's undo plan: the campaign action plus any live recovery files to
/// drain first.
#[derive(Debug, Clone)]
struct UndoPlan {
    slug: String,
    repo_path: Option<PathBuf>,
    branch: Option<String>,
    pr_number: Option<u64>,
    /// Reconciled per-repo status, `None` for a recovery-only (not-in-state) repo.
    status: Option<RepoChangeStatus>,
    action: UndoAction,
    /// Transaction ids of live recovery files for this repo, drained FIRST.
    recovery_tx_ids: Vec<String>,
    /// The merge commit oid of the landed PR, from the GitHub reconcile (Phase 6):
    /// drives the parent-count dispatch for the revert. `None` unless the repo's
    /// PR reconciled as `Merged`.
    merge_commit_oid: Option<String>,
    /// The base branch the merged PR landed on (from the reconcile). The revert
    /// branch is cut from this branch's head; `None` unless merged.
    base_ref_name: Option<String>,
}

/// Outcome of undoing one repo, used to render results and true up state.
#[derive(Debug, Clone)]
struct UndoOutcome {
    slug: String,
    pr_number: Option<u64>,
    kind: OutcomeKind,
}

#[derive(Debug, Clone, PartialEq)]
enum OutcomeKind {
    /// PR closed / branches deleted (and any recovery drained): mark cleaned up.
    Undone,
    /// Nothing to do (already gone): leave state untouched.
    Skipped,
    /// Merged PR reverted: a `revert/<change-id>` PR was opened (Phase 6 [F4]).
    /// Marks the row `RevertPrOpen`; the aggregate reaches `Abandoned` once every
    /// merged row is reverted.
    RevertPrOpened { pr_number: Option<u64> },
    /// A step failed; the error is reported and state is NOT advanced, so a
    /// re-run retries this repo.
    Failed(String),
}

/// The org/owner portion of a repo slug (`org/repo` -> `org`).
fn org_of(repo_slug: &str) -> &str {
    repo_slug.split('/').next().unwrap_or(repo_slug)
}

/// Map a reconciled repo status + recorded PR number to a campaign action.
/// Pure and directly unit-testable.
fn classify_action(status: &RepoChangeStatus, pr_number: Option<u64>) -> UndoAction {
    match status {
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

/// True when a plan entry has real work: a non-`AlreadyGone` action, or a
/// recovery file to drain. `AlreadyGone` with no recovery is informational.
fn needs_action(plan: &UndoPlan) -> bool {
    !matches!(plan.action, UndoAction::AlreadyGone) || !plan.recovery_tx_ids.is_empty()
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
) -> Vec<UndoPlan> {
    debug!(
        "build_plan: change_id={} repos={} recoveries={} merged_prs={}",
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

        plans.push(UndoPlan {
            slug: repo_state.repo_slug.clone(),
            repo_path,
            branch: Some(repo_state.branch_name.clone()),
            pr_number: repo_state.pr_number.or_else(|| merged.map(|p| p.number)),
            status: Some(repo_state.status.clone()),
            action: classify_action(&repo_state.status, repo_state.pr_number),
            recovery_tx_ids,
            merge_commit_oid: merged.and_then(|p| p.merge_commit_oid.clone()),
            base_ref_name: merged.map(|p| p.base_ref_name.clone()),
        });
    }

    // Recovery-only repos: committed local only, never recorded in state.
    for (i, rec) in recoveries.iter().enumerate() {
        if used[i] {
            continue;
        }
        let slug = rec
            .repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        plans.push(UndoPlan {
            slug,
            repo_path: Some(rec.repo_path.clone()),
            branch: rec.branch.clone(),
            pr_number: None,
            status: None,
            action: UndoAction::DeleteLocal,
            recovery_tx_ids: vec![rec.transaction_id.clone()],
            merge_commit_oid: None,
            base_ref_name: None,
        });
    }

    plans
}

/// Human label for a plan entry's reconciled state column.
fn state_label(plan: &UndoPlan) -> &'static str {
    match &plan.status {
        None => "committed local only",
        Some(RepoChangeStatus::CleanedUp) => "already gone",
        Some(RepoChangeStatus::PrMerged) => "PR merged",
        Some(RepoChangeStatus::PrOpen) => "PR open",
        Some(RepoChangeStatus::PrDraft) => "PR open (draft)",
        Some(RepoChangeStatus::PrClosed) => "PR closed",
        Some(RepoChangeStatus::RevertPrOpen) => "revert PR open",
        Some(RepoChangeStatus::BranchCreated) => "pushed, no PR",
        Some(RepoChangeStatus::Failed) => "failed",
    }
}

/// Human label for a plan entry's action column.
fn action_label(plan: &UndoPlan) -> String {
    match &plan.action {
        UndoAction::ClosePr { pr_number } => {
            format!("close PR #{pr_number} -> delete remote branch -> delete local branch")
        }
        UndoAction::DeleteRemoteAndLocal => {
            "delete remote branch -> delete local branch".to_string()
        }
        UndoAction::DeleteLocal => "delete local branch".to_string(),
        UndoAction::RequiresRevert { pr_number } => match pr_number {
            Some(n) => format!("PR #{n} merged -> open revert PR (never touches base branch)"),
            None => "merged -> open revert PR (never touches base branch)".to_string(),
        },
        UndoAction::AlreadyGone => "already gone; skip".to_string(),
    }
}

/// Print the reconciled plan (repo | state | action), plus a recovery-drain
/// note for any entry that carries one.
fn print_plan(plan: &[UndoPlan], change_id: &str) {
    println!("Undo plan for {change_id}:");
    for p in plan {
        let drain = if p.recovery_tx_ids.is_empty() {
            String::new()
        } else {
            format!(
                "  (drain {} recovery file(s) first)",
                p.recovery_tx_ids.len()
            )
        };
        println!(
            "  {:<40} {:<22} {}{}",
            p.slug,
            state_label(p),
            action_label(p),
            drain
        );
    }
}

/// Prompt before undoing. Fails closed on non-interactive stdin (pass `--yes`).
fn confirm_undo(change_id: &str, count: usize, yes: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if yes {
        debug!("--yes supplied; skipping undo confirmation prompt");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(eyre::eyre!(
            "Refusing to undo {change_id} ({count} repositories) without confirmation on non-interactive stdin; pass --yes to proceed"
        ));
    }
    print!("Undo {change_id} across {count} repositories? (y/N): ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read confirmation from stdin")?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Reconcile the recorded change state against GitHub reality before planning,
/// reusing `gx review sync`'s core so merged/closed PRs are trued up. Orgs are
/// taken from the recorded repo slugs (or an explicit `--org`); a per-org fetch
/// failure is a warning, not a hard error, so an offline undo of local branches
/// still proceeds on the recorded state.
fn reconcile(
    change_id: &str,
    org: Option<&str>,
    state: &ChangeState,
    config: &Config,
) -> Vec<github::PrInfo> {
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
    for org in &orgs {
        match github::list_prs_by_change_id(org, change_id, config) {
            Ok(prs) => all_prs.extend(prs),
            Err(e) => warn!("Failed to list PRs from org '{org}' during reconcile: {e}"),
        }
    }

    if all_prs.is_empty() {
        debug!("reconcile: no PRs returned; leaving recorded state as-is");
        return all_prs;
    }

    match crate::review::sync_change_state(&all_prs, change_id) {
        Ok((merged, closed, status)) => {
            info!("Reconciled {change_id}: {merged} merged, {closed} closed (status {status:?})")
        }
        Err(e) => warn!("Failed to reconcile change state for {change_id}: {e}"),
    }

    // Return only the merged PRs; they carry the merge commit oid + base branch
    // the Phase 6 revert path needs.
    all_prs
        .into_iter()
        .filter(|p| p.state == github::PrState::Merged)
        .collect()
}

/// Delete the pushed branch: the remote branch (via the token-consistent gh
/// helper) when `remote` is set, then the local branch resolved through the
/// recorded path. A local branch that is already gone is a no-op (checked, not
/// error-sniffed); a recorded-but-missing path is reported, never skipped.
///
/// F13: the remote delete is pre-probed with `git ls-remote --exit-code`
/// before ever calling the gh API, so a never-pushed or already-deleted
/// remote branch (a 404 from `gh api ... DELETE`) is a no-op for this repo,
/// not a per-repo failure -- an item Phase 5 explicitly deferred to Phase 7.
fn delete_branches(plan: &UndoPlan, config: &Config, remote: bool) -> Result<(), String> {
    let branch = plan
        .branch
        .as_deref()
        .ok_or_else(|| "no branch recorded".to_string())?;

    if remote {
        let exists_remotely = match &plan.repo_path {
            Some(path) if crate::bare::is_git_path(path) => {
                git::remote_branch_exists_probe(path, branch)
                    .map_err(|e| format!("cannot verify remote branch {branch} (offline?): {e}"))?
            }
            // No local checkout to probe from: fall back to attempting the
            // delete (the prior behavior for this edge).
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
    }

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

/// Render per-repo outcomes with the same unified results UX as `review`.
fn render_results(outcomes: &[UndoOutcome], cli: &Cli) {
    let results: Vec<ReviewResult> = outcomes
        .iter()
        .map(|o| {
            let error = match &o.kind {
                OutcomeKind::Undone | OutcomeKind::Skipped | OutcomeKind::RevertPrOpened { .. } => {
                    None
                }
                OutcomeKind::Failed(msg) => Some(msg.clone()),
            };
            ReviewResult {
                repo: Repo::from_slug(o.slug.clone()),
                change_id: "UNDO".to_string(),
                pr_number: o.pr_number,
                action: ReviewAction::Deleted,
                error,
            }
        })
        .collect();

    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };
    display_review_results(&results, &opts);

    let undone = outcomes
        .iter()
        .filter(|o| o.kind == OutcomeKind::Undone)
        .count();
    let reverted = outcomes
        .iter()
        .filter(|o| matches!(o.kind, OutcomeKind::RevertPrOpened { .. }))
        .count();
    let failed = outcomes
        .iter()
        .filter(|o| matches!(o.kind, OutcomeKind::Failed(_)))
        .count();
    let skipped = outcomes
        .iter()
        .filter(|o| o.kind == OutcomeKind::Skipped)
        .count();

    println!(
        "\n📊 {} repositories: {undone} undone, {reverted} reverted (revert PR opened), {failed} failed, {skipped} skipped",
        outcomes.len()
    );
}

/// Process `gx undo <change-id>`: reconcile against GitHub, print the plan,
/// prompt (fail-closed on non-interactive stdin, `--yes`), then execute per
/// repo in parallel under the per-repo lock.
pub fn process_undo_command(
    cli: &Cli,
    config: &Config,
    change_id: &str,
    org: Option<&str>,
    yes: bool,
) -> Result<()> {
    info!("Starting undo for change ID: {change_id}");

    let manager = StateManager::new()?;
    let state = manager
        .load(change_id)?
        .ok_or_else(|| eyre::eyre!("No change state recorded for {change_id}; nothing to undo"))?;

    // Reconcile against GitHub reality first (Phase 4 sync), then reload. The
    // merged PRs carry the merge commit oid + base branch the revert path needs.
    // This planning copy is read-only from here on: the authoritative
    // load-mutate-save happens once more, under the change lock, right before
    // the final save below.
    let merged_prs = reconcile(change_id, org, &state, config);
    let state = manager
        .load(change_id)?
        .ok_or_else(|| eyre::eyre!("Change state for {change_id} disappeared during reconcile"))?;

    // Gather live recovery files for this change-id (F12: a pushed branch may
    // be recorded only in a recovery file, not the state).
    let recoveries: Vec<RecoveryState> = Transaction::list_recovery_states()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.change_id == change_id)
        .collect();

    let plan = build_plan(&state, &recoveries, &merged_prs);
    print_plan(&plan, change_id);

    let actionable: Vec<UndoPlan> = plan.into_iter().filter(needs_action).collect();
    if actionable.is_empty() {
        println!("Nothing to undo for {change_id}.");
        return Ok(());
    }

    if !confirm_undo(change_id, actionable.len(), yes)? {
        println!("Aborted; no changes made.");
        return Ok(());
    }

    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    let outcomes: Vec<UndoOutcome> = pool.install(|| {
        actionable
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
    // directly.
    {
        let _change_lock = crate::lock::ChangeLock::acquire(change_id)
            .context("Failed to acquire change lock before saving undo results")?;
        let mut state = manager
            .load(change_id)?
            .ok_or_else(|| eyre::eyre!("Change state for {change_id} disappeared during undo"))?;
        finalize_state(&mut state, &outcomes);
        manager
            .save(&state)
            .context("Failed to save change state after undo")?;
    }

    render_results(&outcomes, cli);
    Ok(())
}

#[cfg(test)]
mod tests;
