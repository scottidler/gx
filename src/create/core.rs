//! Core of `gx create`: never prints, never prompts, takes explicit params.
//!
//! Split from `src/create.rs` (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 3): the CLI wrapper
//! in `create.rs` discovers/filters repos, shows the blast radius and prompts
//! (or honors `--yes`), then calls [`execute_create`] here with the resolved
//! repo list and a [`Confirmation`] already satisfied. This module is the
//! seam a future MCP `create-apply` tool calls into instead of the wrapper.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod apply;
pub mod manifest;
pub mod propose;

use crate::config::Config;
use crate::confirm::Confirmation;
use crate::diff;
use crate::file;
use crate::git;
use crate::github;
use crate::repo::Repo;
use crate::state::{ChangeState, StateManager};
use crate::transaction::{RollbackStep, Transaction};
use chrono::Local;
use eyre::{Context, Result};
use log::{debug, info, warn};
use manifest::{FileAction, ProposalManifest, ProposalOutcome};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Statistics for substitution operations
#[derive(Debug, Default, Clone)]
pub struct SubstitutionStats {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_no_matches: usize,
    pub files_no_change: usize,
    pub files_skipped_binary: usize,
    pub total_matches: usize,
}

#[derive(Debug, Clone)]
pub enum Change {
    Add(String, String),   // path, content
    Delete,                // delete matched files
    Sub(String, String),   // pattern, replacement
    Regex(String, String), // regex pattern, replacement
    /// An agent-generated change (the prompt). Handled by the fleet-level
    /// PROPOSE pass ([`propose::execute_propose`]), NOT by the per-repo
    /// `process_single_repo` pipeline: propose/present/confirm is a fleet
    /// barrier (design doc `2026-07-12-llm-propose-apply-and-mcp-server.md`,
    /// Chunk A). The per-repo match below rejects it defensively.
    Llm(String),
    /// The INTERNAL deterministic apply of a persisted proposal (design doc
    /// Chunk A, Apply pass). NEVER CLI-exposed and no [`CreateAction`] maps to
    /// it: it is constructed only by [`apply::execute_apply`] and rides the
    /// UNCHANGED `process_single_repo` pipeline (stash/switch/pull, then
    /// branch/commit/push/PR), so recovery, undo, locks, and F12 apply exactly
    /// as they do for `sub`/`regex`.
    ///
    /// Carries the proposal directory (blobs live under it) and the canonical
    /// manifest (`Arc` so the fleet-shared `&Change` clones cheaply). Per repo,
    /// `process_single_repo` looks up THIS repo's entry by slug, verifies the
    /// post-pull `base_sha` and each blob's sha256 under the `RepoLock`, then
    /// writes the full post-change bytes through the existing backup/write seam.
    Patchset {
        proposal_dir: PathBuf,
        manifest: Arc<ProposalManifest>,
    },
}

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub repo: Repo,
    pub change_id: String,
    pub action: CreateAction,
    pub files_affected: Vec<String>,
    pub substitution_stats: Option<SubstitutionStats>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// The branch the repo was on before the change (for state tracking).
    pub original_branch: Option<String>,
    /// The pre-commit HEAD of the base branch (the safe point), set once a
    /// commit lands. `None` for dry runs and pre-commit failures.
    pub base_sha: Option<String>,
    /// The per-repo diff, joined for display. Previously computed
    /// (`diff_parts`) and discarded (design doc Phase 3); a future MCP
    /// `change-get` tool (or a `--format json` mode) reads this instead of
    /// re-deriving it. `None` when nothing was diffed (an error before
    /// mutation started, or no files affected).
    pub diff: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CreateAction {
    DryRun, // No changes made (preview)

    Committed, // Changes committed to branch
    PrCreated, // PR created successfully
}

/// Generate a default change ID based on current timestamp
pub fn generate_change_id() -> String {
    let now = Local::now();
    let timestamp = now.format("%Y-%m-%dT%H-%M-%S").to_string();
    format!("GX-{timestamp}")
}

/// Join the per-file diff fragments collected during mutation into a single
/// display string, or `None` when nothing was diffed. Surfaces the diff that
/// was previously computed and discarded on [`CreateResult`] (design doc
/// Phase 3); never printed here - display is the CLI wrapper's job.
fn join_diff(diff_parts: &[String]) -> Option<String> {
    if diff_parts.is_empty() {
        None
    } else {
        Some(diff_parts.join("\n"))
    }
}

/// Execute a `gx create` run across pre-filtered, pre-confirmed repos:
/// initialize state tracking, process each repo in parallel, and return the
/// structured results. Never prints and never prompts - the caller (the CLI
/// wrapper today; `apply::execute_apply` and an MCP `create-apply` tool) already
/// resolved which repos to target and confirmed (TTY, `--yes`, or a verified
/// token) before calling this.
///
/// **Locking contract:** for a committing run (`commit_message.is_some()`) the
/// CALLER must hold the [`crate::lock::ChangeLock`] for `change_id` across this
/// call. This core does not acquire it (so apply can hold ONE guard across its
/// whole RMW); a caller that mutates state without holding the lock breaks the
/// cross-process serialization guarantee.
#[allow(clippy::too_many_arguments)]
pub fn execute_create(
    repos: &[Repo],
    change_id: &str,
    files: &[String],
    change: &Change,
    commit_message: Option<&str>,
    pr: bool,
    draft: bool,
    config: &Config,
    parallel_jobs: usize,
    confirmation: Confirmation,
) -> Result<Vec<CreateResult>> {
    debug!(
        "execute_create: change_id={change_id} repos={} committing={} confirmation={confirmation:?}",
        repos.len(),
        commit_message.is_some()
    );

    // Change-level lock (Phase 7 [F6]): the CALLER holds it for the whole run so
    // another process's `changes/<id>.json` read-modify-write (`review sync`,
    // `cleanup`, `undo`, ...) can never interleave with this run's incremental
    // saves. This core does NOT acquire it - the CLI wrapper
    // (`create::process_create_command`) and `apply::execute_apply` acquire the
    // `ChangeLock` and let their guard outlive this synchronous call, exactly as
    // `rollback::core::execute_recovery` trusts its wrapper to hold the RepoLock
    // (Phase 3). This is what lets apply span the ENTIRE load->verify->apply->
    // state-write RMW under ONE guard (addendum disposition, doc lines 189 &
    // 737-739) without this core re-acquiring and fail-fasting against the
    // caller's own guard. The in-process `Mutex<ChangeState>` below still
    // serializes this run's own rayon workers against EACH OTHER.

    // Initialize state tracking if we're going to make changes (not dry run).
    let change_state = if commit_message.is_some() {
        let state = ChangeState::new(change_id.to_string(), commit_message.map(str::to_string));
        Some(Mutex::new(state))
    } else {
        None
    };
    // One state manager, shared for incremental saves after each repo ([A3]).
    //
    // F12 fail-closed (post-audit hardening): a committing run REQUIRES a durable
    // state store. Without one, a pushed branch could end up recorded in NEITHER
    // state nor recovery (finalize deletes the recovery file), so the guarantee
    // "a pushed branch is ALWAYS recorded in state OR recovery" cannot hold. Abort
    // NOW - before any repo is mutated or pushed - rather than downgrading to a
    // best-effort `None` that fails open.
    let state_manager = if commit_message.is_some() {
        Some(StateManager::new().map_err(|e| {
            eyre::eyre!(
                "Cannot start a committing gx run: the durable state store is unavailable ({e}). \
                 Refusing to mutate repositories without it (F12: a pushed branch must always be \
                 recorded in state or recovery)."
            )
        })?)
    } else {
        None
    };

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process repositories in parallel. The change-state save is now done
    // INSIDE `process_single_repo` (Phase 4 control-flow refactor, F12): a
    // pushed-safe-point save before `finalize()` runs, then a final save once
    // the whole result (including any PR) is known. This fold is display-only;
    // the `Mutex<ChangeState>` + `StateManager` are passed in, and each worker
    // locks briefly to update just its own repo's entry, same as before.
    let results: Vec<CreateResult> = pool.install(|| {
        repos
            .par_iter()
            .map(|repo| {
                process_single_repo(
                    repo,
                    change_id,
                    files,
                    change,
                    commit_message,
                    pr,
                    draft,
                    config,
                    change_state.as_ref(),
                    state_manager.as_ref(),
                )
            })
            .collect()
    });

    if let Some(state_mutex) = change_state {
        if let Ok(state) = state_mutex.into_inner() {
            if !state.repositories.is_empty() {
                info!("Saved change state for {}", state.change_id);
            }
        }
    }

    Ok(results)
}

/// Update change state based on create result
fn update_change_state(state: &mut ChangeState, result: &CreateResult, draft: bool) {
    // Only track if the operation actually did something
    match result.action {
        CreateAction::Committed | CreateAction::PrCreated => {
            // Add repository to state
            state.add_repository(result.repo.slug.clone(), result.change_id.clone());

            // Update local path and files modified
            if let Some(repo_state) = state.repositories.get_mut(&result.repo.slug) {
                repo_state.local_path = Some(result.repo.path.to_string_lossy().to_string());
                repo_state.files_modified = result.files_affected.clone();
                repo_state.original_branch = result.original_branch.clone();
            }

            // If PR was created, update PR info using the new set_pr_info method
            if matches!(result.action, CreateAction::PrCreated) {
                if let (Some(pr_number), Some(pr_url)) = (result.pr_number, result.pr_url.clone()) {
                    state.set_pr_info(&result.repo.slug, pr_number, pr_url, draft);
                }
            }
        }
        CreateAction::DryRun => {
            // Don't track dry runs
        }
    }
}

/// Build an error result in the DryRun (nothing committed) state.
fn dry_run_error(
    repo: &Repo,
    change_id: &str,
    error: String,
    diff_parts: &[String],
) -> CreateResult {
    CreateResult {
        repo: repo.clone(),
        change_id: change_id.to_string(),
        action: CreateAction::DryRun,
        files_affected: Vec::new(),
        substitution_stats: None,
        pr_number: None,
        pr_url: None,
        original_branch: None,
        base_sha: None,
        diff: join_diff(diff_parts),
        error: Some(error),
    }
}

/// Process create command for a single repository with comprehensive rollback.
///
/// Order (design Architecture): lock → stash -u → switch to head → pull →
/// mutate → branch → stage → commit → push → finalize → create PR. Rollback
/// steps are persisted write-ahead via the typed `Transaction`.
///
/// The change-state save is a NAMED control-flow refactor (Phase 4 [F12], panel
/// finding): it happens IN HERE now, not in the caller's outer fold, at two
/// points. First, a safe-point save right after the push (`Phase::Pushed`) but
/// BEFORE `finalize()` runs - `finalize()` deletes the recovery file, so this
/// guarantees a pushed branch is recorded in state OR recovery in every crash
/// window, never neither. Second, a final save once the whole result (including
/// any PR) is known, replacing what the caller's rayon fold used to do.
#[allow(clippy::too_many_arguments)]
fn process_single_repo(
    repo: &Repo,
    change_id: &str,
    file_patterns: &[String],
    change: &Change,
    commit_message: Option<&str>,
    pr: bool,
    draft: bool,
    config: &Config,
    change_state: Option<&Mutex<ChangeState>>,
    state_manager: Option<&StateManager>,
) -> CreateResult {
    debug!(
        "process_single_repo: repo={} change_id={change_id}",
        repo.name
    );
    let repo_path = &repo.path;
    let committing = commit_message.is_some();
    let mut diff_parts: Vec<String> = Vec::new();

    // Test-only fault injection (inert unless GX_TEST_FORCE_REPO_ERROR names
    // this repo), same "compiled in, inert by default" shape as
    // `GX_TEST_FAIL_STATE_SAVE` (`state.rs`). Lets an e2e deterministically
    // fail exactly one repo in a multi-repo run to exercise the Phase 1
    // airtight-reporting path (non-zero exit + `--report` naming the failure)
    // without needing to fabricate a real git failure.
    if std::env::var("GX_TEST_FORCE_REPO_ERROR").as_deref() == Ok(repo.name.as_str()) {
        return dry_run_error(
            repo,
            change_id,
            "GX_TEST_FORCE_REPO_ERROR: simulated repo failure".to_string(),
            &diff_parts,
        );
    }

    // Test-only fault injection (inert unless GX_TEST_PANIC_WORKER names this
    // repo): panics the rayon worker processing this repo, to exercise the
    // Phase 1 panic hook (an ERROR diagnostic line rather than a bare abort).
    if std::env::var("GX_TEST_PANIC_WORKER").as_deref() == Ok(repo.name.as_str()) {
        panic!(
            "GX_TEST_PANIC_WORKER: simulated worker panic for {}",
            repo.name
        );
    }

    // Per-repo lock: a second concurrent gx invocation must not interleave
    // stash/branch operations on this repo (design Q5).
    let _lock = match crate::lock::RepoLock::acquire(repo_path) {
        Ok(lock) => lock,
        Err(e) => {
            return dry_run_error(
                repo,
                change_id,
                format!("Repository is locked: {e}"),
                &diff_parts,
            )
        }
    };

    let mut transaction = Transaction::new(repo_path.clone(), change_id.to_string(), committing);
    let mut files_affected = Vec::new();

    // 1. Determine the original branch; guard against detached HEAD ([A30]).
    let original_branch = match git::get_current_branch_name(repo_path) {
        Ok(branch) if branch.is_empty() => {
            return dry_run_error(
                repo,
                change_id,
                "Repository is in detached HEAD state; check out a branch first".to_string(),
                &diff_parts,
            );
        }
        Ok(branch) => branch,
        Err(e) => {
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to get current branch: {e}"),
                &diff_parts,
            );
        }
    };
    transaction.set_original_branch(original_branch.clone());

    // 2. Stash uncommitted work (including untracked, -u) so the worktree is a
    //    pristine checkout of HEAD during mutation. status --porcelain counts
    //    untracked (??) entries, so the dirty predicate already includes them.
    match git::has_uncommitted_changes(repo_path) {
        Ok(true) => {
            let message = format!("GX auto-stash for {change_id}");
            // Write-ahead (F5): register the stash-restore step keyed by message
            // BEFORE the stash exists, so a crash in the window between creating
            // the stash and learning its SHA still records the WIP to restore.
            if let Err(e) =
                transaction.push_step(crate::transaction::RollbackStep::PopStashByMessage {
                    repo: repo_path.clone(),
                    message: message.clone(),
                })
            {
                transaction.rollback();
                return dry_run_error(
                    repo,
                    change_id,
                    format!("Failed to persist recovery: {e}"),
                    &diff_parts,
                );
            }
            match git::stash_save_with_untracked(repo_path, &message) {
                Ok(sha) => {
                    transaction.set_stash_sha(sha.clone());
                    // Swap the placeholder for the SHA-keyed step now that the
                    // stash exists (the SHA survives concurrent stash mutation).
                    if let Err(e) =
                        transaction.swap_last_step(crate::transaction::RollbackStep::PopStash {
                            repo: repo_path.clone(),
                            stash_sha: sha,
                        })
                    {
                        transaction.rollback();
                        return dry_run_error(
                            repo,
                            change_id,
                            format!("Failed to persist recovery: {e}"),
                            &diff_parts,
                        );
                    }
                    // Crash hook (Phase 8): the stash exists and its restore step
                    // is persisted (phase `mutating`); an abort here must recover
                    // to a byte-identical worktree with the branch never created.
                    crate::crash::maybe_crash("after-stash");
                }
                Err(e) => {
                    // The stash was never created; roll back to clear the
                    // placeholder (it resolves to a harmless no-op).
                    transaction.rollback();
                    return dry_run_error(
                        repo,
                        change_id,
                        format!("Failed to stash changes: {e}"),
                        &diff_parts,
                    );
                }
            }
        }
        Ok(false) => {}
        Err(e) => {
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to check repository status: {e}"),
                &diff_parts,
            );
        }
    }

    // 3. Switch to the head branch if we are not already on it. A failure here
    //    (F10) is a hard per-repo error: swallowing it would silently mutate
    //    whatever branch the user happened to be on.
    let head = match git::get_head_branch(repo_path) {
        Ok(head) => head,
        Err(e) => {
            transaction.rollback();
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to determine head branch: {e}"),
                &diff_parts,
            );
        }
    };
    // Write-ahead: ALWAYS register the switch-back to the user's original branch,
    // even in the common `head == original_branch` case where no switch-to-head
    // is needed. Keep-work recovery (`pushed`/`finalizing`) restores the
    // environment by executing SwitchBranch/PopStash steps ONLY; without this
    // step, a keep-work recovery after a push/finalize crash would strand the
    // user on the GX branch instead of returning them to their original branch
    // (finalize's own switch-back never runs on a crash). In full reverse this
    // step is a harmless no-op: DeleteLocalBranch already force-switches off the
    // GX branch to head, which equals the original branch in the common case.
    if let Err(e) = transaction.push_step(crate::transaction::RollbackStep::SwitchBranch {
        repo: repo_path.clone(),
        branch: original_branch.clone(),
    }) {
        transaction.rollback();
        return dry_run_error(
            repo,
            change_id,
            format!("Failed to persist recovery: {e}"),
            &diff_parts,
        );
    }
    if head != original_branch {
        if let Err(e) = git::switch_branch(repo_path, &head) {
            transaction.rollback();
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to switch to head branch: {e}"),
                &diff_parts,
            );
        }
    }

    // 4. Pull latest changes.
    if let Err(e) = git::pull_latest_changes(repo_path) {
        transaction.rollback();
        return dry_run_error(
            repo,
            change_id,
            format!("Failed to pull latest changes: {e}"),
            &diff_parts,
        );
    }

    // 5. Apply the change (each registers its undo step write-ahead).
    let mut substitution_stats = None;
    let change_result = match change {
        Change::Add(path, content) => apply_add_change(
            repo_path,
            path,
            content,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ),
        Change::Delete => apply_delete_change(
            repo_path,
            file_patterns,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ),
        Change::Sub(pattern, replacement) => apply_substitution_change(
            repo_path,
            file_patterns,
            pattern,
            replacement,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        )
        .map(|stats| substitution_stats = Some(stats)),
        Change::Regex(pattern, replacement) => apply_regex_change(
            repo_path,
            file_patterns,
            pattern,
            replacement,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        )
        .map(|stats| substitution_stats = Some(stats)),
        // A fleet-level barrier, never applied per-repo here (design Chunk A):
        // the propose pass handles `Change::Llm` at orchestration level. Reaching
        // this arm is an internal routing bug; fail loudly rather than silently.
        Change::Llm(_) => Err(eyre::eyre!(
            "internal error: Change::Llm must go through the propose pass, not process_single_repo"
        )),
        // The deterministic apply of a persisted proposal (design Chunk A). Runs
        // AFTER the pipeline's stash/switch/pull above, so the drift check inside
        // sees the post-pull head. Everything downstream (branch/commit/push/PR)
        // is the unchanged pipeline.
        Change::Patchset {
            proposal_dir,
            manifest,
        } => apply_patchset_change(
            repo_path,
            proposal_dir,
            manifest,
            &repo.slug,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ),
    };

    if let Err(e) = change_result {
        transaction.rollback();
        let mut result = dry_run_error(
            repo,
            change_id,
            format!("Failed to apply changes: {e}"),
            &diff_parts,
        );
        result.substitution_stats = substitution_stats;
        return result;
    }

    // No files affected, or dry run: roll back (restores worktree, branch, stash).
    if files_affected.is_empty() || !committing {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: if committing {
                Vec::new()
            } else {
                files_affected
            },
            substitution_stats,
            pr_number: None,
            pr_url: None,
            original_branch: Some(original_branch.clone()),
            base_sha: None,
            diff: join_diff(&diff_parts),
            error: None,
        };
    }

    let commit_message = commit_message.unwrap_or_default();

    // 6. branch → stage → commit → push (each undo persisted write-ahead).
    let base_sha = match commit_changes_with_rollback(
        repo_path,
        change_id,
        commit_message,
        &files_affected,
        &mut transaction,
    ) {
        Ok(base_sha) => base_sha,
        Err(e) => {
            transaction.rollback();
            let mut result = dry_run_error(
                repo,
                change_id,
                format!("Failed to commit changes: {e}"),
                &diff_parts,
            );
            result.substitution_stats = substitution_stats;
            return result;
        }
    };

    // 6b. Pushed safe-point save (F12, control-flow refactor): record the
    //     branch in change state NOW, before finalize() runs and deletes the
    //     recovery file. A crash anywhere from here on - including mid-finalize
    //     - leaves this repo recorded in state even after recovery is gone.
    let pushed_saved = record_pushed_state(
        change_state,
        state_manager,
        repo,
        change_id,
        &original_branch,
        &files_affected,
        &base_sha,
    );

    // 6c. F12 fail-closed (post-audit hardening): the recovery file may be
    //     deleted (which finalize() does) ONLY when the pushed safe point was
    //     durably saved, so the invariant "recovery file deleted => state
    //     contains this repo" holds. If the save FAILED, do NOT finalize:
    //     restore the working tree (branch + stash) but RETAIN the recovery file
    //     as the record of the pushed branch, and report the repo Committed with
    //     an error naming the retained recovery path. `gx undo`'s recovery-file
    //     sweep (F12) then reverses the shared work; `gx rollback execute` on the
    //     retained `pushed`-phase file restores only the environment (keep-work).
    if !pushed_saved {
        let recovery_hint = transaction
            .recovery_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("recovery/{}", change_id));
        let mut error = format!(
            "Committed and pushed, but the durable safe-point save FAILED; recovery file RETAINED at {recovery_hint}. Reverse the pushed work with `gx undo {change_id}`, or restore-only with `gx rollback execute`."
        );
        // Restore the environment WITHOUT deleting the retained recovery file.
        if let Err(e) = transaction.finalize_retaining_recovery() {
            error = format!("{error} (environment restore also failed: {e})");
        }
        // Deliberately NOT calling record_final_state: it must never delete the
        // recovery file, and re-attempting the save (still failing) would only
        // mask the retained-recovery outcome. The recovery file is the record.
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::Committed,
            files_affected,
            substitution_stats,
            pr_number: None,
            pr_url: None,
            original_branch: Some(original_branch.clone()),
            base_sha: Some(base_sha),
            diff: join_diff(&diff_parts),
            error: Some(error),
        };
    }

    // 7. Finalize BEFORE creating the PR: switch back to the original branch and
    //    re-apply the stash. A finalize error (e.g. cannot restore branch) keeps
    //    the recovery file for manual resolution and is reported as Committed.
    let finalize_outcome = match transaction.finalize() {
        Ok(outcome) => outcome,
        Err(e) => {
            let result = CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::Committed,
                files_affected,
                substitution_stats,
                pr_number: None,
                pr_url: None,
                original_branch: Some(original_branch.clone()),
                base_sha: Some(base_sha),
                diff: join_diff(&diff_parts),
                error: Some(format!("Committed and pushed, but finalize failed: {e}")),
            };
            record_final_state(change_state, state_manager, &result, draft);
            return result;
        }
    };

    // 8. Create the PR against the (already-restored) remote. A PR failure is
    //    surfaced on the result, not swallowed ([A4]; Phase 5 refines).
    let (action, pr_number, pr_url, mut error) = if pr {
        match create_pull_request(repo, change_id, commit_message, draft, config) {
            Ok(result) => (
                CreateAction::PrCreated,
                Some(result.number),
                Some(result.url),
                None,
            ),
            Err(e) => (
                CreateAction::Committed,
                None,
                None,
                Some(format!("PR creation failed: {e}")),
            ),
        }
    } else {
        (CreateAction::Committed, None, None, None)
    };

    // A stash-restore conflict is surfaced (design Q2): committed, but the user's
    // WIP could not be re-applied; the stash is preserved for manual recovery.
    if let Some((sha, msg)) = finalize_outcome.stash_error {
        let stash_err = format!(
            "stash-restore-failed: could not re-apply stash {sha} ({msg}); recover with `git stash apply {sha}`"
        );
        error = Some(match error {
            Some(existing) => format!("{existing}; {stash_err}"),
            None => stash_err,
        });
    }

    let result = CreateResult {
        repo: repo.clone(),
        change_id: change_id.to_string(),
        action,
        files_affected,
        substitution_stats,
        pr_number,
        pr_url,
        original_branch: Some(original_branch.clone()),
        base_sha: Some(base_sha),
        diff: join_diff(&diff_parts),
        error,
    };
    record_final_state(change_state, state_manager, &result, draft);
    result
}

/// Record the just-pushed branch in change state (the F12 safe point): saved
/// BEFORE `finalize()` runs, so a crash during finalize (which deletes the
/// recovery file) still leaves this repo recorded in at least one store.
///
/// Returns `true` only when the pushed safe point was DURABLY saved to disk;
/// `false` on any failure (no state store, poisoned mutex, save error). The
/// caller must NOT delete the recovery file (must not finalize) when this
/// returns `false` - that is the F12 fail-closed guarantee (recovery file
/// deleted => state contains this repo).
#[must_use]
fn record_pushed_state(
    change_state: Option<&Mutex<ChangeState>>,
    state_manager: Option<&StateManager>,
    repo: &Repo,
    change_id: &str,
    original_branch: &str,
    files_affected: &[String],
    base_sha: &str,
) -> bool {
    debug!(
        "record_pushed_state: repo={} change_id={change_id} base_sha={base_sha}",
        repo.slug
    );
    let Some(state_mutex) = change_state else {
        warn!(
            "No change state for {}; pushed safe point not recorded (recovery retained)",
            repo.slug
        );
        return false;
    };
    let Ok(mut state) = state_mutex.lock() else {
        warn!(
            "Change state mutex poisoned; skipping pushed safe-point save for {}",
            repo.slug
        );
        return false;
    };
    state.add_repository(repo.slug.clone(), change_id.to_string());
    if let Some(repo_state) = state.repositories.get_mut(&repo.slug) {
        repo_state.local_path = Some(repo.path.to_string_lossy().to_string());
        repo_state.files_modified = files_affected.to_vec();
        repo_state.original_branch = Some(original_branch.to_string());
        repo_state.base_sha = Some(base_sha.to_string());
    }
    let Some(manager) = state_manager else {
        warn!(
            "No state manager for {}; pushed safe point not durably saved (recovery retained)",
            repo.slug
        );
        return false;
    };
    match manager.save(&state) {
        Ok(()) => true,
        Err(e) => {
            warn!(
                "Failed to save pushed safe-point state for {}: {e}",
                repo.slug
            );
            false
        }
    }
}

/// Fold a finished repo's result into change state and save. This is now the
/// ONLY place a finished repo's outcome is saved (the caller's outer rayon
/// fold is display-only, Phase 4 control-flow refactor). Re-records `base_sha`
/// since `update_change_state` -> `add_repository` resets the entry.
fn record_final_state(
    change_state: Option<&Mutex<ChangeState>>,
    state_manager: Option<&StateManager>,
    result: &CreateResult,
    draft: bool,
) {
    debug!(
        "record_final_state: repo={} action={:?}",
        result.repo.slug, result.action
    );
    let Some(state_mutex) = change_state else {
        return;
    };
    let Ok(mut state) = state_mutex.lock() else {
        warn!(
            "Change state mutex poisoned; skipping final state save for {}",
            result.repo.slug
        );
        return;
    };
    update_change_state(&mut state, result, draft);
    if let Some(repo_state) = state.repositories.get_mut(&result.repo.slug) {
        repo_state.base_sha = result.base_sha.clone();
    }
    if matches!(
        result.action,
        CreateAction::Committed | CreateAction::PrCreated
    ) {
        if let Some(manager) = state_manager {
            if let Err(e) = manager.save(&state) {
                warn!("Failed to save change state: {e}");
            }
        }
    }
}

/// Apply add change (create new file)
fn apply_add_change(
    repo_path: &Path,
    file_path: &str,
    content: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<()> {
    // Validate and resolve the path; `gx add` is the one write path that does
    // not flow through FileSet, so it enforces the same policy directly ([A32]).
    let full_path = file::validate_new_file_path(repo_path, file_path)?;

    // Check if file already exists
    if full_path.exists() {
        return Err(eyre::eyre!("File already exists: {}", file_path));
    }

    // Write-ahead: register removal of the created file before creating it.
    transaction.push_step(crate::transaction::RollbackStep::RemoveCreatedFile {
        path: full_path.clone(),
    })?;

    // Create file and generate diff
    let (_, diff) = file::create_file_with_content(&full_path, content, 3)?;

    files_affected.push(file_path.to_string());
    diff_parts.push(format!(
        "  A {}\n{}",
        file_path,
        crate::utils::indent(&diff, 4)
    ));

    Ok(())
}

/// Apply delete change (remove matching files)
fn apply_delete_change(
    repo_path: &Path,
    file_patterns: &[String],
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<()> {
    // Find tracked files matching all patterns (deduped + sorted).
    let all_files = file::FileSet::matching_any(repo_path, file_patterns)?;

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Read content for diff; skip non-UTF-8 (binary) files ([A21]).
        let Some(content) = file::read_utf8_or_skip(&full_path)? else {
            continue;
        };

        // Out-of-tree backup, then write-ahead register the restore before delete.
        let backup_path = transaction.backup_path_for(&file_path)?;
        let mode = file::create_backup(&full_path, &backup_path)?;
        transaction.push_step(crate::transaction::RollbackStep::RestoreBackup {
            backup: backup_path,
            original: full_path.clone(),
            mode,
        })?;

        // Delete file
        file::delete_file(&full_path)?;

        let diff = diff::generate_diff(&content, "", 3);
        files_affected.push(file_path.to_string_lossy().to_string());
        diff_parts.push(format!(
            "  D {}\n{}",
            file_path.display(),
            crate::utils::indent(&diff, 4)
        ));
    }

    Ok(())
}

/// Apply substitution change
fn apply_substitution_change(
    repo_path: &Path,
    file_patterns: &[String],
    pattern: &str,
    replacement: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<SubstitutionStats> {
    let mut stats = SubstitutionStats::default();

    // Find tracked files matching all patterns (deduped + sorted).
    let all_files = file::FileSet::matching_any(repo_path, file_patterns)?;
    stats.files_scanned = all_files.len();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Try to apply substitution
        match file::apply_substitution_to_file(&full_path, pattern, replacement, 3)? {
            diff::SubstitutionResult::Changed {
                content: updated_content,
                diff,
                matches,
            } => {
                // Out-of-tree backup, then write-ahead register the restore.
                let backup_path = transaction.backup_path_for(&file_path)?;
                let mode = file::create_backup(&full_path, &backup_path)?;
                transaction.push_step(crate::transaction::RollbackStep::RestoreBackup {
                    backup: backup_path,
                    original: full_path.clone(),
                    mode,
                })?;

                // Write updated content
                file::write_file_content(&full_path, &updated_content)?;

                files_affected.push(file_path.to_string_lossy().to_string());
                diff_parts.push(format!(
                    "  M {}\n{}",
                    file_path.display(),
                    crate::utils::indent(&diff, 4)
                ));

                stats.files_changed += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::NoMatches => {
                debug!(
                    "No matches found for pattern '{}' in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_matches += 1;
            }
            diff::SubstitutionResult::NoChange { matches } => {
                debug!(
                    "Pattern '{}' matched but no changes resulted in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_change += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::SkippedBinary => {
                stats.files_skipped_binary += 1;
            }
        }
    }

    Ok(stats)
}

/// Apply regex change
fn apply_regex_change(
    repo_path: &Path,
    file_patterns: &[String],
    pattern: &str,
    replacement: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<SubstitutionStats> {
    let mut stats = SubstitutionStats::default();

    // Find tracked files matching all patterns (deduped + sorted).
    let all_files = file::FileSet::matching_any(repo_path, file_patterns)?;
    stats.files_scanned = all_files.len();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Try to apply regex substitution
        match file::apply_regex_to_file(&full_path, pattern, replacement, 3)? {
            diff::SubstitutionResult::Changed {
                content: updated_content,
                diff,
                matches,
            } => {
                // Out-of-tree backup, then write-ahead register the restore.
                let backup_path = transaction.backup_path_for(&file_path)?;
                let mode = file::create_backup(&full_path, &backup_path)?;
                transaction.push_step(crate::transaction::RollbackStep::RestoreBackup {
                    backup: backup_path,
                    original: full_path.clone(),
                    mode,
                })?;

                // Write updated content
                file::write_file_content(&full_path, &updated_content)?;

                files_affected.push(file_path.to_string_lossy().to_string());
                diff_parts.push(format!(
                    "  M {}\n{}",
                    file_path.display(),
                    crate::utils::indent(&diff, 4)
                ));

                stats.files_changed += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::NoMatches => {
                debug!(
                    "No matches found for regex pattern '{}' in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_matches += 1;
            }
            diff::SubstitutionResult::NoChange { matches } => {
                debug!(
                    "Regex pattern '{}' matched but no changes resulted in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_change += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::SkippedBinary => {
                stats.files_skipped_binary += 1;
            }
        }
    }

    Ok(stats)
}

/// Apply a persisted proposal for ONE repo (`Change::Patchset`, design Chunk A
/// apply). Runs under the per-repo `RepoLock` the caller already holds, AFTER
/// the pipeline's stash/switch/pull, so `get_head_sha` sees the post-pull head.
///
/// Two loud, nothing-written refusals guard the write (both panel must-fixes):
/// 1. **Post-pull drift**: `HEAD != base_sha` means the repo advanced past the
///    proposal's base (pull can legitimately move head, which is why the check
///    sits here). The proposal no longer describes this tree; re-propose.
/// 2. **Blob tamper**: every add/modify blob is re-hashed and size-checked
///    against the manifest BEFORE anything is written, so a proposal artifact
///    altered after review is refused with the worktree untouched.
///
/// Only after every blob verifies does it write through the EXISTING seam:
/// register `RestoreBackup`/`RemoveCreatedFile` write-ahead, then delete (for
/// `Delete`) or write the full post-change bytes with the proposal's mode (for
/// `Add`/`Modify`). gx never applies hunks to the real worktree - the diff is
/// for humans, the blobs are for apply.
fn apply_patchset_change(
    repo_path: &Path,
    proposal_dir: &Path,
    manifest: &ProposalManifest,
    slug: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<()> {
    debug!(
        "apply_patchset_change: slug={slug} proposal_dir={} repo={}",
        proposal_dir.display(),
        repo_path.display()
    );

    // This repo's entry in the canonical manifest.
    let rp = manifest
        .repos
        .iter()
        .find(|r| r.slug == slug)
        .ok_or_else(|| eyre::eyre!("no proposal entry for {slug} in the manifest"))?;
    if rp.outcome != ProposalOutcome::Proposed {
        return Err(eyre::eyre!(
            "proposal for {slug} is not appliable (outcome {:?})",
            rp.outcome
        ));
    }

    // (1) Post-pull drift refusal: touches NOTHING (no blob written yet; the
    //     caller rolls the stash/switch back to a byte-identical worktree).
    let head = git::get_head_sha(repo_path)?;
    if head != rp.base_sha {
        return Err(eyre::eyre!(
            "repo drifted since proposal; re-propose (proposal base {} != current head {head})",
            rp.base_sha
        ));
    }

    // (2) Verify every add/modify blob AND confirm every path is worktree-
    //     confined, both under the RepoLock and BEFORE writing any of them, so a
    //     tampered blob OR an escaping path is refused with nothing written. The
    //     confirm token binds CONTENT (per-blob sha256), not the PATH: a
    //     proposal manifest carrying `../escape`, an absolute path, or a `.git/`
    //     write must still be refused at the one write seam that enforces the
    //     policy (`file::validate_new_file_path`, design "the one write path that
    //     enforces the policy directly"). Read the verified bytes into memory
    //     here; the write loop below only touches the worktree once EVERY entry
    //     passes (path + blob), so a single rejecting path is a loud per-repo
    //     refusal with nothing written.
    let mut verified: Vec<(&manifest::FileEntry, PathBuf, Option<Vec<u8>>)> =
        Vec::with_capacity(rp.files.len());
    for entry in &rp.files {
        // Path confinement first: reject absolute paths, `..` traversal, `.git/`
        // writes, and symlink-escape for BOTH the write and the delete branch.
        let full = file::validate_new_file_path(repo_path, &entry.path).with_context(|| {
            format!(
                "refusing to apply {slug}: unsafe path in proposal: {}",
                entry.path
            )
        })?;
        match entry.action {
            FileAction::Delete => verified.push((entry, full, None)),
            FileAction::Add | FileAction::Modify => {
                let want = entry.sha256.as_deref().ok_or_else(|| {
                    eyre::eyre!(
                        "proposal file {} has no sha256 (corrupt manifest)",
                        entry.path
                    )
                })?;
                let blob = manifest::blob_path(proposal_dir, slug, &entry.path);
                let bytes = std::fs::read(&blob)
                    .with_context(|| format!("Failed to read proposal blob: {}", blob.display()))?;
                let got = crate::hash::sha256_hex(&bytes);
                if got != want {
                    return Err(eyre::eyre!(
                        "blob hash mismatch for {} (manifest {want}, on-disk {got}); refusing to apply a tampered proposal",
                        entry.path
                    ));
                }
                if bytes.len() as u64 != entry.size {
                    return Err(eyre::eyre!(
                        "blob size mismatch for {} (manifest {}, on-disk {})",
                        entry.path,
                        entry.size,
                        bytes.len()
                    ));
                }
                verified.push((entry, full, Some(bytes)));
            }
        }
    }

    // All paths confined and all blobs verified: mutate through the existing
    // backup/write seam using the validated absolute path.
    for (entry, full, bytes) in verified {
        let rel = Path::new(&entry.path);
        match entry.action {
            FileAction::Delete => {
                if !full.exists() {
                    return Err(eyre::eyre!(
                        "cannot delete {}: file absent at the proposal base (unexpected drift)",
                        entry.path
                    ));
                }
                let backup_path = transaction.backup_path_for(rel)?;
                let mode = file::create_backup(&full, &backup_path)?;
                transaction.push_step(RollbackStep::RestoreBackup {
                    backup: backup_path,
                    original: full.clone(),
                    mode,
                })?;
                file::delete_file(&full)?;
                diff_parts.push(format!("  D {}", entry.path));
            }
            FileAction::Add => {
                transaction.push_step(RollbackStep::RemoveCreatedFile { path: full.clone() })?;
                let bytes = bytes.expect("add entry carries verified bytes");
                file::write_bytes_with_git_mode(&full, &bytes, &entry.mode)?;
                diff_parts.push(format!("  A {}", entry.path));
            }
            FileAction::Modify => {
                if !full.exists() {
                    return Err(eyre::eyre!(
                        "cannot modify {}: file absent at the proposal base (unexpected drift)",
                        entry.path
                    ));
                }
                let backup_path = transaction.backup_path_for(rel)?;
                let mode = file::create_backup(&full, &backup_path)?;
                transaction.push_step(RollbackStep::RestoreBackup {
                    backup: backup_path,
                    original: full.clone(),
                    mode,
                })?;
                let bytes = bytes.expect("modify entry carries verified bytes");
                file::write_bytes_with_git_mode(&full, &bytes, &entry.mode)?;
                diff_parts.push(format!("  M {}", entry.path));
            }
        }
        files_affected.push(entry.path.clone());
    }

    debug!(
        "apply_patchset_change: slug={slug} applied {} file(s)",
        files_affected.len()
    );
    Ok(())
}

/// Create the gx branch, stage, commit, and push - registering each undo step
/// write-ahead. The success-path branch restoration and stash pop are handled by
/// `Transaction::finalize`, not here. Returns the pre-commit HEAD (the safe
/// point `ResetCommit` already captures), so the caller can record `base_sha`
/// (F11/F12) at the pushed-state safe point before `finalize()` runs.
fn commit_changes_with_rollback(
    repo_path: &Path,
    change_id: &str,
    commit_message: &str,
    files_affected: &[String],
    transaction: &mut Transaction,
) -> Result<String> {
    use crate::transaction::Phase;

    // Whether the branch pre-existed gx's run (so rollback won't delete it).
    let branch_existed = git::branch_exists_locally(repo_path, change_id).unwrap_or(false);

    // Record the GX branch name so recovery (phase reporting, the `pushing`
    // probe, `gx undo`) need not re-derive it.
    transaction.set_branch(change_id.to_string());

    // Write-ahead: register branch deletion before creating the branch.
    transaction.push_step(RollbackStep::DeleteLocalBranch {
        repo: repo_path.to_path_buf(),
        branch: change_id.to_string(),
        branch_existed,
    })?;
    git::create_branch(repo_path, change_id)
        .with_context(|| format!("Failed to create or switch to branch: {change_id}"))?;
    // Crash hook (Phase 8): the GX branch exists and its delete step is
    // persisted (phase `mutating`); recovery full-reverses, remote branch absent.
    crate::crash::maybe_crash("after-branch");

    // Record the pre-commit HEAD so rollback resets to a known target, and
    // register the reset write-ahead before committing.
    let expected_sha = git::get_head_sha(repo_path)?;
    transaction.push_step(RollbackStep::ResetCommit {
        repo: repo_path.to_path_buf(),
        expected_sha: expected_sha.clone(),
    })?;

    // Stage only the specific files we modified - never "git add .".
    git::add_files(repo_path, files_affected).context("Failed to stage files")?;
    git::commit_changes(repo_path, commit_message).context("Failed to commit changes")?;
    // Crash hook (Phase 8): the commit is on the GX branch and the reset step is
    // persisted (phase `mutating`); recovery full-reverses, remote branch absent.
    crate::crash::maybe_crash("after-commit");

    // Stamp `pushing` write-ahead: a kill after this stamp but before the push
    // completes is classified at recovery time by a read-only ls-remote probe.
    // Rollback no longer registers a remote-delete step - `gx undo` owns remote
    // reversal, so nothing on the rollback path can ever delete a pushed branch.
    transaction.set_phase(Phase::Pushing)?;
    // Crash hook (Phase 8): `pushing` is stamped but the push has NOT run; the
    // ls-remote probe finds the branch absent and dispatches a full reverse.
    crate::crash::maybe_crash("before-push");
    git::push_branch(repo_path, change_id).context("Failed to push branch")?;
    // Stamp `pushed`: the branch is now shared; recovery keeps the work.
    transaction.set_phase(Phase::Pushed)?;
    // Crash hook (Phase 8): the branch is pushed and `pushed` is stamped;
    // recovery keeps the shared work (remote branch retained).
    crate::crash::maybe_crash("after-push");

    Ok(expected_sha)
}

/// Create a pull request for the changes
/// Returns the PR number and URL on success
fn create_pull_request(
    repo: &Repo,
    change_id: &str,
    commit_message: &str,
    draft: bool,
    config: &Config,
) -> Result<github::CreatePrResult> {
    let repo_slug = &repo.slug;
    let base = resolve_base_branch(repo, config);
    let result = github::create_pr(repo_slug, change_id, commit_message, &base, draft, config)
        .with_context(|| format!("Failed to create PR for {repo_slug}"))?;
    info!(
        "Created PR #{} for repository: {} - {}",
        result.number, repo_slug, result.url
    );
    Ok(result)
}

/// Resolve the repo's default base branch: prefer the local head branch, then
/// the GitHub API's default_branch, falling back to `main` with a warning - a
/// lookup failure must never drop the PR ([A4]).
fn resolve_base_branch(repo: &Repo, config: &Config) -> String {
    if let Ok(branch) = git::get_head_branch(&repo.path) {
        return branch;
    }
    let org = repo.slug.split('/').next().unwrap_or("");
    if let Ok(token) = github::read_token(org, config) {
        if let Ok(branch) = github::get_default_branch(&repo.slug, &token) {
            return branch;
        }
    }
    warn!(
        "Could not resolve default branch for {}; falling back to main",
        repo.slug
    );
    "main".to_string()
}

#[cfg(test)]
mod tests;
