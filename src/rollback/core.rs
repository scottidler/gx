//! Core of `gx rollback execute`: never prints, never prompts.
//!
//! Split from `src/rollback.rs` (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 3): the CLI
//! wrapper loads the recovery state, holds the per-repo lock for the whole
//! flow, prints the plan and validation results, and prompts (or honors
//! `--yes`) - all BEFORE calling [`execute_recovery`] here, which runs the
//! actual recovery engine (`Transaction::execute_recovery`). `gx rollback`
//! restores a single repo's worktree from a recovery file and NEVER touches a
//! remote (`gx undo` owns remote reversal).
//!
//! Not exposed over MCP (the design doc marks recovery repair a human
//! surface), but split the same way as `create`/`undo` for architectural
//! consistency and so the `Confirmation` seam is uniform across every
//! mutating flow.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use crate::confirm::Confirmation;
use crate::transaction::{RecoveryOutcome, RecoveryState, Transaction};
use eyre::Result;
use log::debug;

/// Basic validation of a recovery state: the repo must still exist and be a
/// git repository. Returns `(errors, warnings)`. Pure; never prints.
pub fn validate_recovery_state(state: &RecoveryState) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let repo = &state.repo_path;
    if !repo.exists() {
        errors.push(format!(
            "Repository path no longer exists: {}",
            repo.display()
        ));
    } else if !crate::bare::is_git_path(repo) {
        // Layout-aware: a flat repo (`.git` dir), a linked worktree (`.git`
        // pointer file), or a bare container all count as a git repository.
        errors.push(format!("Not a git repository: {}", repo.display()));
    }

    if state.steps.is_empty() {
        warnings.push("Recovery state has no steps".to_string());
    }

    (errors, warnings)
}

/// Run the recovery engine for `transaction_id`. Never prints and never
/// prompts - the caller (the CLI wrapper) already loaded the state, held the
/// per-repo lock, printed the plan and validation results, and confirmed
/// (TTY, `--yes`) before calling this; `confirmation` records that.
pub fn execute_recovery(
    transaction_id: &str,
    confirmation: Confirmation,
) -> Result<RecoveryOutcome> {
    debug!("execute_recovery: transaction_id={transaction_id} confirmation={confirmation:?}");
    Transaction::execute_recovery(transaction_id)
}

#[cfg(test)]
mod tests;
