//! Transaction engine for `gx create`.
//!
//! Rollback steps are *data* (`RollbackStep`), not closures: they are executed
//! by one interpreter ([`execute_step`]) and serialized verbatim as recovery
//! state. Recovery state is written **write-ahead** - the step that undoes an
//! operation is persisted before that operation runs - so a SIGKILL between an
//! operation and its already-persisted step cannot strand an unrecorded
//! mutation. Every step is idempotent, so re-running an interrupted recovery
//! converges (design: "Recovery Invariant").

use crate::file::atomic_write;
use crate::git;
use eyre::{Context, Result};
use log::{debug, error, trace, warn};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A single reversible step. Each carries enough state to be reversed correctly
/// without re-deriving it later, and must be idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RollbackStep {
    /// Re-apply a stash (by SHA) that gx created when stashing the user's WIP.
    PopStash { repo: PathBuf, stash_sha: String },
    /// Switch back to a branch (typically the user's original branch).
    SwitchBranch { repo: PathBuf, branch: String },
    /// Delete a local branch gx created. `branch_existed` records whether the
    /// branch pre-existed gx's run; if so, rollback must NOT delete it.
    DeleteLocalBranch {
        repo: PathBuf,
        branch: String,
        branch_existed: bool,
    },
    /// Delete a remote branch gx pushed.
    DeleteRemoteBranch { repo: PathBuf, branch: String },
    /// Reset HEAD back to the pre-commit SHA (a known target, not blind HEAD~1).
    ResetCommit { repo: PathBuf, expected_sha: String },
    /// Restore a file from its out-of-tree backup.
    RestoreBackup { backup: PathBuf, original: PathBuf },
    /// Remove a file gx created (for `gx add`).
    RemoveCreatedFile { path: PathBuf },
}

/// Per-step journal status. The interpreter rewrites the recovery file after
/// each transition so a crash never loses the record of what already ran.
///
/// - `Pending`: registered write-ahead, its reversal has not run yet.
/// - `Applied`: first beat of a two-beat step done, second beat pending (only
///   `PopStash` today: the stash was re-applied but not yet dropped).
/// - `Done`: the reversal completed.
/// - `Failed`: the reversal errored; `StepEntry.error` carries the message. The
///   file and backups are retained so a re-run can converge.
/// - `SkippedLegacy`: a retired step kind interpreted as a no-op (Phase 2's
///   `LegacyDeleteRemoteBranch`); counts as complete for artifact cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepStatus {
    #[default]
    Pending,
    Applied,
    Done,
    Failed,
    SkippedLegacy,
}

/// A single journaled rollback step: the step itself plus its current status and
/// (on failure) the error that stopped it. Serializes as
/// `{ "step": {...}, "status": "done" }`.
///
/// Deserialization is deliberately tolerant of the pre-journal file shape (a
/// bare `RollbackStep` with no wrapper), so recovery files written by an older
/// gx still load; a bare step is read as `Pending`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepEntry {
    pub step: RollbackStep,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl StepEntry {
    /// A freshly registered step: reversal not yet run.
    pub fn pending(step: RollbackStep) -> Self {
        StepEntry {
            step,
            status: StepStatus::Pending,
            error: None,
        }
    }
}

impl<'de> Deserialize<'de> for StepEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Accept both the journaled shape and a bare pre-journal `RollbackStep`.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Wrapped {
                step: RollbackStep,
                #[serde(default)]
                status: StepStatus,
                #[serde(default)]
                error: Option<String>,
            },
            Bare(RollbackStep),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Wrapped {
                step,
                status,
                error,
            } => StepEntry {
                step,
                status,
                error,
            },
            Repr::Bare(step) => StepEntry::pending(step),
        })
    }
}

/// Recovery state persisted to `$XDG_DATA_HOME/gx/recovery/<tx-id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryState {
    pub transaction_id: String,
    pub change_id: String,
    pub repo_path: PathBuf,
    pub created_at: String,
    pub steps: Vec<StepEntry>,
}

impl RecoveryState {
    /// True when any journaled step is in the `Failed` state.
    pub fn has_failed_steps(&self) -> bool {
        self.steps.iter().any(|s| s.status == StepStatus::Failed)
    }

    /// Count of steps currently in the `Failed` state.
    pub fn failed_step_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Failed)
            .count()
    }

    /// True when every step is `Done` or `SkippedLegacy` — the only condition
    /// under which the recovery file and backups may be removed.
    fn all_complete(&self) -> bool {
        self.steps
            .iter()
            .all(|s| matches!(s.status, StepStatus::Done | StepStatus::SkippedLegacy))
    }
}

/// Outcome of a successful [`Transaction::finalize`].
#[derive(Debug, Default, Clone)]
pub struct FinalizeOutcome {
    /// A stash existed and was applied + dropped cleanly.
    pub stash_restored: bool,
    /// The stash could not be re-applied (conflict); `(sha, message)`. The stash
    /// is preserved (not dropped) so the user can recover it manually.
    pub stash_error: Option<(String, String)>,
}

/// A transaction over a single repository's mutation.
pub struct Transaction {
    transaction_id: String,
    change_id: String,
    repo_path: PathBuf,
    created_at: String,
    steps: Vec<RollbackStep>,
    /// The user's original branch, restored on finalize.
    original_branch: Option<String>,
    /// The stash SHA (if gx stashed WIP), re-applied on finalize.
    stash_sha: Option<String>,
    /// Whether recovery state is persisted (true only for real, committing runs).
    persist: bool,
    finalized: bool,
}

// Global counter for unique transaction IDs.
static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(1);

impl Transaction {
    /// Create a transaction. `persist` controls whether recovery state is
    /// written to disk (true for committing runs, false for dry-runs that
    /// rollback immediately and never need crash recovery).
    pub fn new(repo_path: PathBuf, change_id: String, persist: bool) -> Self {
        let counter = TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = chrono::Utc::now().timestamp();
        let transaction_id = format!("gx-tx-{timestamp}-{counter}");
        Transaction {
            transaction_id,
            change_id,
            repo_path,
            created_at: chrono::Utc::now().to_rfc3339(),
            steps: Vec::new(),
            original_branch: None,
            stash_sha: None,
            persist,
            finalized: false,
        }
    }

    /// Record the user's original branch (restored on finalize/rollback).
    pub fn set_original_branch(&mut self, branch: String) {
        self.original_branch = Some(branch);
    }

    /// Record the stash SHA (re-applied on finalize/rollback).
    pub fn set_stash_sha(&mut self, sha: String) {
        self.stash_sha = Some(sha);
    }

    /// The out-of-tree backup path for a repo-relative file.
    pub fn backup_path_for(&self, relative: &Path) -> Result<PathBuf> {
        Ok(backups_dir()?.join(&self.transaction_id).join(relative))
    }

    /// Register a rollback step, persisting recovery state write-ahead (before
    /// the operation it reverses runs). Idempotent steps tolerate the operation
    /// having happened or not.
    pub fn push_step(&mut self, step: RollbackStep) -> Result<()> {
        debug!(
            "Transaction::push_step: tx={} step={:?}",
            self.transaction_id, step
        );
        self.steps.push(step);
        self.persist_recovery_state()?;
        Ok(())
    }

    /// Build a `RecoveryState` snapshot from the live step list. Freshly
    /// registered steps are `Pending`; the journaled interpreter promotes them
    /// as it runs.
    fn build_recovery_state(&self) -> RecoveryState {
        RecoveryState {
            transaction_id: self.transaction_id.clone(),
            change_id: self.change_id.clone(),
            repo_path: self.repo_path.clone(),
            created_at: self.created_at.clone(),
            steps: self.steps.iter().cloned().map(StepEntry::pending).collect(),
        }
    }

    /// Write the live step list to the recovery file (atomic). No-op when
    /// `persist` is false.
    fn persist_recovery_state(&self) -> Result<()> {
        if !self.persist {
            return Ok(());
        }
        write_recovery_state(&self.build_recovery_state())
    }

    /// Roll back every registered step in reverse order, journaling per-step
    /// status. Individual failures do NOT abort the pass. Artifacts (recovery
    /// file + backup dir) are removed ONLY when every step reached `Done`; a
    /// failed step retains them so a `gx rollback execute` re-run can converge.
    pub fn rollback(&mut self) {
        if self.finalized {
            debug!("Transaction already finalized, skipping rollback");
            return;
        }
        error!(
            "Rolling back transaction {} ({} steps)",
            self.transaction_id,
            self.steps.len()
        );

        let mut state = self.build_recovery_state();
        let run = run_recovery_journaled(&mut state, self.persist);
        if run.failed > 0 {
            warn!(
                "Rollback completed with {} successes and {} failures",
                run.succeeded, run.failed
            );
        } else {
            debug!("Rollback completed: {} steps", run.succeeded);
        }

        // A dry-run transaction (persist=false) has no on-disk artifacts to
        // preserve, so it always finishes by clearing state. A committing run
        // that failed a step keeps its evidence and points at the re-run.
        if self.persist && !state.all_complete() {
            error!("{}", incomplete_report(&state, &self.transaction_id));
            return;
        }

        self.steps.clear();
        self.cleanup_artifacts();
    }

    /// Success path: restore the user's environment (switch back to the original
    /// branch, re-apply + drop the stash), then clear steps and delete the
    /// recovery file and backups. Does NOT undo the committed/pushed work.
    pub fn finalize(&mut self) -> Result<FinalizeOutcome> {
        debug!("Transaction::finalize: tx={}", self.transaction_id);
        let mut outcome = FinalizeOutcome::default();

        if let Some(branch) = &self.original_branch {
            git::switch_branch(&self.repo_path, branch)
                .with_context(|| format!("Failed to switch back to original branch {branch}"))?;
        }

        if let Some(sha) = &self.stash_sha {
            match git::stash_apply_sha(&self.repo_path, sha) {
                Ok(()) => {
                    // Apply succeeded: drop the stash. A drop failure is not fatal.
                    if let Err(e) = git::stash_drop_by_sha(&self.repo_path, sha) {
                        warn!("Applied stash {sha} but failed to drop it: {e}");
                    }
                    outcome.stash_restored = true;
                }
                Err(e) => {
                    // Conflict: do NOT drop. Leave the apply result in place on the
                    // original branch so the collision is visible (design Q2).
                    warn!("Failed to re-apply stash {sha}: {e}");
                    outcome.stash_error = Some((sha.clone(), e.to_string()));
                }
            }
        }

        self.finalized = true;
        self.steps.clear();
        self.cleanup_artifacts();
        Ok(outcome)
    }

    /// Remove the recovery file and this transaction's backup directory.
    fn cleanup_artifacts(&self) {
        if let Ok(path) = recovery_file(&self.transaction_id) {
            if path.exists() {
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!("Failed to remove recovery file {}: {}", path.display(), e);
                }
            }
        }
        if let Ok(dir) = backups_dir() {
            let tx_dir = dir.join(&self.transaction_id);
            if tx_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&tx_dir) {
                    warn!("Failed to remove backup dir {}: {}", tx_dir.display(), e);
                }
            }
        }
    }

    // ---- Recovery (gx rollback) ----

    /// Load a recovery state by transaction id.
    pub fn load_recovery_state(transaction_id: &str) -> Result<RecoveryState> {
        let path = recovery_file(transaction_id)?;
        if !path.exists() {
            return Err(eyre::eyre!("Recovery state not found: {transaction_id}"));
        }
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read recovery state: {}", path.display()))?;
        let state: RecoveryState = serde_json::from_str(&json)
            .with_context(|| format!("Failed to parse recovery state: {}", path.display()))?;
        Ok(state)
    }

    /// List all available recovery states (newest first). Unparsable files are
    /// logged and skipped.
    pub fn list_recovery_states() -> Result<Vec<RecoveryState>> {
        let dir = recovery_dir()?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut states = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<RecoveryState>(&content) {
                    Ok(state) => states.push(state),
                    Err(e) => warn!(
                        "Skipping unparsable recovery file {}: {}",
                        path.display(),
                        e
                    ),
                },
                Err(e) => warn!("Failed to read recovery file {}: {}", path.display(), e),
            }
        }
        states.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(states)
    }

    /// Execute recovery for a transaction: run its steps in reverse, journaling
    /// per-step status to the recovery file after each transition. `Done` steps
    /// are skipped, so a re-run only retries `Pending`/`Failed` steps (and, for
    /// `PopStash`, only the drop when the apply already journaled `Applied`).
    ///
    /// The recovery file and backup dir are removed ONLY when every step
    /// completed. If any step failed, both are retained and this returns an
    /// error naming the failed steps and the re-run command.
    pub fn execute_recovery(transaction_id: &str) -> Result<()> {
        let mut state = Self::load_recovery_state(transaction_id)?;
        debug!(
            "execute_recovery: tx={transaction_id} steps={}",
            state.steps.len()
        );

        let run = run_recovery_journaled(&mut state, true);
        if run.failed > 0 {
            warn!(
                "Recovery completed with {} successes and {} failures",
                run.succeeded, run.failed
            );
        } else {
            debug!("Recovery completed: {} steps", run.succeeded);
        }

        if !state.all_complete() {
            return Err(eyre::eyre!("{}", incomplete_report(&state, transaction_id)));
        }

        Self::cleanup_recovery_state_by_id(transaction_id)?;
        if let Ok(dir) = backups_dir() {
            let _ = std::fs::remove_dir_all(dir.join(transaction_id));
        }
        Ok(())
    }

    /// Remove a recovery file by transaction id.
    pub fn cleanup_recovery_state_by_id(transaction_id: &str) -> Result<()> {
        let path = recovery_file(transaction_id)?;
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove recovery file: {}", path.display()))?;
            debug!("Removed recovery file: {}", path.display());
        }
        Ok(())
    }
}

/// Execute a single rollback step. All steps are idempotent.
pub fn execute_step(step: &RollbackStep) -> Result<()> {
    debug!("execute_step: {step:?}");
    match step {
        RollbackStep::PopStash { repo, stash_sha } => {
            git::stash_apply_sha(repo, stash_sha)?;
            // Best-effort drop after a clean apply.
            if let Err(e) = git::stash_drop_by_sha(repo, stash_sha) {
                warn!("Applied stash {stash_sha} but failed to drop it: {e}");
            }
            Ok(())
        }
        RollbackStep::SwitchBranch { repo, branch } => git::switch_branch(repo, branch),
        RollbackStep::DeleteLocalBranch {
            repo,
            branch,
            branch_existed,
        } => {
            if *branch_existed {
                debug!("Branch {branch} pre-existed gx's run; not deleting");
                return Ok(());
            }
            // If we're currently on the branch, get off it first (force, to
            // tolerate any uncommitted state) before deleting.
            if let Ok(current) = git::get_current_branch_name(repo) {
                if current == *branch {
                    let head = git::get_head_branch(repo).unwrap_or_else(|_| "main".to_string());
                    if let Err(e) = git::force_switch_branch(repo, &head) {
                        warn!("Failed to switch off {branch} before delete: {e}");
                    }
                }
            }
            // Idempotent: deleting an absent branch is fine.
            match git::delete_local_branch(repo, branch) {
                Ok(()) => Ok(()),
                Err(e) => {
                    debug!("delete_local_branch({branch}) returned: {e} (treating as done)");
                    Ok(())
                }
            }
        }
        RollbackStep::DeleteRemoteBranch { repo, branch } => {
            git::delete_remote_branch(repo, branch)
        }
        RollbackStep::ResetCommit { repo, expected_sha } => {
            git::reset_hard_to_sha(repo, expected_sha)
        }
        RollbackStep::RestoreBackup { backup, original } => {
            crate::file::restore_backup(backup, original)
        }
        RollbackStep::RemoveCreatedFile { path } => {
            if path.exists() {
                std::fs::remove_file(path).with_context(|| {
                    format!("Failed to remove created file: {}", path.display())
                })?;
            }
            Ok(())
        }
    }
}

/// Summary of one journaled reverse-execution pass.
struct RecoveryRun {
    succeeded: usize,
    failed: usize,
}

/// Write a recovery state snapshot to its file (atomic).
fn write_recovery_state(state: &RecoveryState) -> Result<()> {
    let path = recovery_file(&state.transaction_id)?;
    let json = serde_json::to_string_pretty(state).context("Failed to serialize recovery state")?;
    atomic_write(&path, json.as_bytes())
        .with_context(|| format!("Failed to persist recovery state: {}", path.display()))?;
    Ok(())
}

/// Set a step's journal status (and error), rewriting the recovery file when
/// `persist`. A journal-write failure is logged but never aborts recovery — the
/// reversal itself is what matters; the worst case is a re-run repeats a `Done`
/// step, which every step tolerates.
fn set_status(
    state: &mut RecoveryState,
    index: usize,
    status: StepStatus,
    error: Option<String>,
    persist: bool,
) {
    state.steps[index].status = status;
    state.steps[index].error = error;
    if persist {
        if let Err(e) = write_recovery_state(state) {
            warn!(
                "Failed to journal recovery state for {}: {}",
                state.transaction_id, e
            );
        }
    }
}

/// Run the recovery steps in reverse, journaling per-step status as it goes.
///
/// `Done`/`SkippedLegacy` steps are skipped (a re-run does not repeat them).
/// `PopStash` is two-beat: `git stash apply` -> journal `Applied` -> drop ->
/// journal `Done`, so a crash after apply retries only the drop; a step already
/// at `Applied` skips the apply entirely. All other steps run through
/// [`execute_step`] and journal `Done`/`Failed`.
fn run_recovery_journaled(state: &mut RecoveryState, persist: bool) -> RecoveryRun {
    debug!(
        "run_recovery_journaled: tx={} steps={} persist={persist}",
        state.transaction_id,
        state.steps.len()
    );
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for i in (0..state.steps.len()).rev() {
        match state.steps[i].status {
            StepStatus::Done | StepStatus::SkippedLegacy => {
                trace!("run_recovery_journaled: skipping completed step {i}");
                continue;
            }
            _ => {}
        }

        let step = state.steps[i].step.clone();
        match &step {
            RollbackStep::PopStash { repo, stash_sha } => {
                // Beat 1 (apply): skipped when the journal already says Applied.
                if state.steps[i].status != StepStatus::Applied {
                    if let Err(e) = git::stash_apply_sha(repo, stash_sha) {
                        error!("Recovery step failed: {step:?} - {e}");
                        set_status(state, i, StepStatus::Failed, Some(e.to_string()), persist);
                        failed += 1;
                        continue;
                    }
                    set_status(state, i, StepStatus::Applied, None, persist);
                }
                // Beat 2 (drop): best-effort. A stash already gone (dropped by a
                // prior run) still converges to Done.
                if let Err(e) = git::stash_drop_by_sha(repo, stash_sha) {
                    warn!("Applied stash {stash_sha} but failed to drop it: {e}");
                }
                set_status(state, i, StepStatus::Done, None, persist);
                succeeded += 1;
            }
            other => match execute_step(other) {
                Ok(()) => {
                    set_status(state, i, StepStatus::Done, None, persist);
                    succeeded += 1;
                }
                Err(e) => {
                    error!("Recovery step failed: {other:?} - {e}");
                    set_status(state, i, StepStatus::Failed, Some(e.to_string()), persist);
                    failed += 1;
                }
            },
        }
    }

    RecoveryRun { succeeded, failed }
}

/// A human-readable report of an incomplete rollback: exactly which steps failed
/// and the command to converge. Used both as the `execute_recovery` error and
/// the create-path `error!` line.
fn incomplete_report(state: &RecoveryState, transaction_id: &str) -> String {
    let mut lines = vec![format!(
        "Rollback for {transaction_id} did not complete; recovery file and backups retained."
    )];
    for entry in &state.steps {
        if entry.status == StepStatus::Failed {
            let err = entry.error.as_deref().unwrap_or("unknown error");
            lines.push(format!("  failed: {:?} - {err}", entry.step));
        }
    }
    lines.push(format!(
        "Re-run to converge: gx rollback execute {transaction_id}"
    ));
    lines.join("\n")
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new(PathBuf::from("."), String::new(), false)
    }
}

/// `$XDG_DATA_HOME/gx/recovery`.
fn recovery_dir() -> Result<PathBuf> {
    Ok(gx_data_dir()?.join("recovery"))
}

/// `$XDG_DATA_HOME/gx/backups`.
fn backups_dir() -> Result<PathBuf> {
    Ok(gx_data_dir()?.join("backups"))
}

fn recovery_file(transaction_id: &str) -> Result<PathBuf> {
    Ok(recovery_dir()?.join(format!("{transaction_id}.json")))
}

fn gx_data_dir() -> Result<PathBuf> {
    crate::config::xdg_data_dir()
        .map(|d| d.join("gx"))
        .ok_or_else(|| eyre::eyre!("Could not determine data dir (set HOME or XDG_DATA_HOME)"))
}

#[cfg(test)]
mod tests;
