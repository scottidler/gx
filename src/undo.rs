//! CLI wrapper for `gx undo <change-id>`: prints the reconciled plan, prompts
//! (or honors `--yes`), then calls into [`core::execute_undo`] and renders
//! the outcomes. All terminal output (`println!`/`print!`) lives here; the
//! core never prints or prompts (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 3).

// `pub` since Phase 9: `gx-mcp` calls the undo cores directly
// (`gx::undo::core::{plan_undo, execute_undo}`) for the `undo-plan` /
// `undo-execute` MCP tools. First cross-crate consumer; private before Phase 8.
pub mod core;

pub use core::{OutcomeKind, UndoAction, UndoOutcome, UndoPlan};

use crate::cli::Cli;
use crate::config::Config;
use crate::output::{display_review_results, StatusOptions};
use crate::repo::Repo;
use crate::review::{ReviewAction, ReviewResult};
use crate::state::RepoChangeStatus;
use eyre::{Context, Result};
use log::debug;

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
        // A bare (unapplied) proposal: undo is local-only (delete artifacts).
        Some(RepoChangeStatus::Proposed) => "proposed",
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
        UndoAction::UnverifiedOffline => {
            "merge state unverified offline; skipped (re-run `gx undo` online)".to_string()
        }
        UndoAction::AlreadyGone => "already gone; skip".to_string(),
        UndoAction::CleanupProposal => {
            "bare proposal; delete proposal artifacts (local only, no remote)".to_string()
        }
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

/// Render per-repo outcomes with the same unified results UX as `review`.
fn render_results(outcomes: &[UndoOutcome], cli: &Cli) {
    let results: Vec<ReviewResult> = outcomes
        .iter()
        .map(|o| {
            let error = match &o.kind {
                OutcomeKind::Undone | OutcomeKind::Skipped | OutcomeKind::RevertPrOpened { .. } => {
                    None
                }
                OutcomeKind::Unverified(msg) | OutcomeKind::Failed(msg) => Some(msg.clone()),
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
    let unverified = outcomes
        .iter()
        .filter(|o| matches!(o.kind, OutcomeKind::Unverified(_)))
        .count();
    let skipped = outcomes
        .iter()
        .filter(|o| o.kind == OutcomeKind::Skipped)
        .count();

    println!(
        "\n📊 {} repositories: {undone} undone, {reverted} reverted (revert PR opened), {failed} failed, {unverified} unverified (offline), {skipped} skipped",
        outcomes.len()
    );
}

/// Process `gx undo <change-id>`: build the reconciled plan, print it, prompt
/// (fail-closed on non-interactive stdin, `--yes`), then execute.
pub fn process_undo_command(
    cli: &Cli,
    config: &Config,
    change_id: &str,
    org: Option<&str>,
    yes: bool,
) -> Result<()> {
    log::info!("Starting undo for change ID: {change_id}");

    let Some(plan_set) = core::plan_undo(change_id, org, config)? else {
        println!("Nothing to undo for {change_id}.");
        return Ok(());
    };

    print_plan(&plan_set.plan, change_id);

    if plan_set.actionable.is_empty() {
        println!("Nothing to undo for {change_id}.");
        return Ok(());
    }

    if !confirm_undo(change_id, plan_set.actionable.len(), yes)? {
        println!("Aborted; no changes made.");
        return Ok(());
    }

    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // The wrapper already confirmed (TTY prompt above, or --yes); the core
    // never prompts, so it always receives an already-satisfied confirmation.
    let outcomes = core::execute_undo(
        &plan_set,
        change_id,
        config,
        parallel_jobs,
        crate::confirm::already_confirmed(),
    )?;

    render_results(&outcomes, cli);
    Ok(())
}
