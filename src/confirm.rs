//! The confirmation seam every mutating core function takes instead of doing
//! its own TTY prompt (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, API Design > Core
//! signatures). A CLI wrapper prompts (or honors `--yes`) BEFORE calling a
//! core function and always passes [`Confirmation::AlreadyConfirmed`]; an MCP
//! caller instead passes back the token from a prior plan/propose call.
//!
//! Phase 3 introduces the type and threads it through every split core
//! (`create::core::execute_create`, `undo::core::execute_undo`,
//! `rollback::core::execute_recovery`) without giving `Token` real teeth: none
//! of those three flows persists a hashable plan/manifest yet, so a CLI
//! wrapper that already confirmed always passes `AlreadyConfirmed`. `Token`
//! exists now so every core signature is stable across the phases that DO add
//! a persisted, hashable plan (propose/apply in Phase 4/5, MCP tools in Phase
//! 9) - at that point the core gains a real check that the token's hash
//! matches the plan the caller was shown.
//!
//! This module ALSO owns the single fail-closed TTY confirm gate for the
//! irreversible finish-line ops ([`confirm_destructive`], design doc
//! `2026-07-12-gx-production-hardening.md`, Phase 3). It is the generalization
//! of the former per-command `confirm_purge`, shared by `review approve`,
//! `review delete`, and `cleanup`.

use eyre::{Context, Result};
use log::debug;

/// A finish-line operation that mutates GitHub/git irreversibly and therefore
/// sits behind [`confirm_destructive`]. The variant selects the blast-radius
/// wording shown in the prompt so the operator sees exactly what will happen -
/// in particular that `ReviewDelete` abandons UNMERGED work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestructiveOp {
    /// `gx review approve`: approve + squash-merge open PRs (irreversible merge).
    ReviewApprove,
    /// `gx review delete`: close open (UNMERGED) PRs and delete their branches.
    ReviewDelete,
    /// `gx cleanup`: force-delete local branches (`git branch -D`).
    Cleanup,
}

impl DestructiveOp {
    /// The human action phrase, parameterized by count, shown in the prompt and
    /// the fail-closed error. States the truth about what is destroyed (esp.
    /// `ReviewDelete`'s unmerged abandonment) so consent is informed.
    fn action_phrase(self, count: usize) -> String {
        match self {
            DestructiveOp::ReviewApprove => format!("approve and MERGE {count} open PR(s)"),
            DestructiveOp::ReviewDelete => {
                format!("CLOSE {count} open (UNMERGED) PR(s) and DELETE their branches")
            }
            DestructiveOp::Cleanup => format!("DELETE {count} local branch(es)"),
        }
    }
}

/// The single confirm gate for the irreversible finish-line ops, generalized
/// from the former `confirm_purge`. Behavior:
/// - `assume_yes` (`--yes`) -> `Ok(true)` without prompting.
/// - interactive TTY -> prompt showing the blast radius; `y`/`yes` -> `true`.
/// - non-interactive stdin without `--yes` -> `Err` naming `--yes` (FAIL
///   CLOSED): a scripted run can never silently perform an irreversible
///   mutation.
///
/// The caller prints the per-org/per-PR breakdown BEFORE calling this (as
/// `review approve`/`delete` already list every PR with its repo slug) and
/// gates the call on the count-vs-threshold check; this helper owns only the
/// final consent moment.
pub fn confirm_destructive(op: DestructiveOp, count: usize, assume_yes: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    debug!("confirm_destructive: op={op:?} count={count} assume_yes={assume_yes}");

    let phrase = op.action_phrase(count);

    if assume_yes {
        debug!("confirm_destructive: --yes supplied; proceeding without prompt (op={op:?})");
        return Ok(true);
    }

    if !std::io::stdin().is_terminal() {
        return Err(eyre::eyre!(
            "Refusing to {phrase} without confirmation on non-interactive stdin; pass --yes to proceed"
        ));
    }

    print!("This will {phrase}. This is IRREVERSIBLE. Proceed? (y/N): ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read confirmation from stdin")?;
    let answer = input.trim().to_lowercase();
    let proceed = answer == "y" || answer == "yes";
    debug!("confirm_destructive: op={op:?} proceed={proceed}");
    Ok(proceed)
}

/// Proof that a mutating operation has been confirmed to proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// A short hash binding the caller to a specific persisted plan/proposal
    /// manifest (the one the caller was shown before confirming). Not yet
    /// verified by any core in this phase - no plan/proposal manifest exists
    /// to hash against until Phase 4+ (propose/apply) and Phase 9 (MCP).
    Token(String),
    /// The caller already confirmed by another means (a CLI TTY prompt, or
    /// `--yes`) before calling this core function.
    AlreadyConfirmed,
}

/// What every CLI wrapper passes a core once its own gate (TTY prompt or
/// `--yes`) has already been satisfied. Honors `GX_TEST_CONFIRM_TOKEN` (inert
/// unless set - a test-only hook, matching `GX_CRASH_POINT` /
/// `GX_TEST_LOCK_DELAY_MS`) so a test can prove a `Token` actually threads
/// through a wrapper into its core unchanged, without any core doing its own
/// env lookup.
pub fn already_confirmed() -> Confirmation {
    match std::env::var("GX_TEST_CONFIRM_TOKEN") {
        Ok(hash) if !hash.is_empty() => Confirmation::Token(hash),
        _ => Confirmation::AlreadyConfirmed,
    }
}

#[cfg(test)]
mod tests;
