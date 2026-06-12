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
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
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

/// Recovery state persisted to `$XDG_DATA_HOME/gx/recovery/<tx-id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryState {
    pub transaction_id: String,
    pub change_id: String,
    pub repo_path: PathBuf,
    pub created_at: String,
    pub steps: Vec<RollbackStep>,
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

    /// Write the live step list to the recovery file (atomic). No-op when
    /// `persist` is false.
    fn persist_recovery_state(&self) -> Result<()> {
        if !self.persist {
            return Ok(());
        }
        let state = RecoveryState {
            transaction_id: self.transaction_id.clone(),
            change_id: self.change_id.clone(),
            repo_path: self.repo_path.clone(),
            created_at: self.created_at.clone(),
            steps: self.steps.clone(),
        };
        let path = recovery_file(&self.transaction_id)?;
        let json =
            serde_json::to_string_pretty(&state).context("Failed to serialize recovery state")?;
        atomic_write(&path, json.as_bytes())
            .with_context(|| format!("Failed to persist recovery state: {}", path.display()))?;
        Ok(())
    }

    /// Roll back every registered step in reverse order (continue on individual
    /// failures), then remove the recovery file and tx backup dir.
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

        let mut succeeded = 0;
        let mut failed = 0;
        for step in self.steps.iter().rev() {
            match execute_step(step) {
                Ok(()) => succeeded += 1,
                Err(e) => {
                    error!("Rollback step failed: {step:?} - {e}");
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            warn!("Rollback completed with {succeeded} successes and {failed} failures");
        } else {
            debug!("Rollback completed: {succeeded} steps");
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

    /// Execute recovery for a transaction: run its steps in reverse, then remove
    /// the recovery file (and tx backup dir).
    pub fn execute_recovery(transaction_id: &str) -> Result<()> {
        let state = Self::load_recovery_state(transaction_id)?;
        debug!(
            "execute_recovery: tx={transaction_id} steps={}",
            state.steps.len()
        );

        let mut succeeded = 0;
        let mut failed = 0;
        for step in state.steps.iter().rev() {
            match execute_step(step) {
                Ok(()) => succeeded += 1,
                Err(e) => {
                    error!("Recovery step failed: {step:?} - {e}");
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            warn!("Recovery completed with {succeeded} successes and {failed} failures");
        } else {
            debug!("Recovery completed: {succeeded} steps");
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
