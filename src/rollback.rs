use crate::cli::RollbackAction;
use crate::lock::RepoLock;
use crate::state::StateManager;
use crate::transaction::{Phase, RecoveryOutcome, RecoveryState, StepStatus, Transaction};
use chrono::{DateTime, Duration, Utc};
use colored::*;
use eyre::{Context, Result};
use log::{debug, error, info, warn};

/// Basic validation of a recovery state: the repo must still exist and be a git
/// repository. Returns `(errors, warnings)`.
fn validate_recovery_state(state: &RecoveryState) -> (Vec<String>, Vec<String>) {
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

/// Summarize step kinds for display.
fn step_kind(step: &crate::transaction::RollbackStep) -> &'static str {
    use crate::transaction::RollbackStep::*;
    match step {
        PopStash { .. } => "pop-stash",
        PopStashByMessage { .. } => "pop-stash-by-message",
        SwitchBranch { .. } => "switch-branch",
        DeleteLocalBranch { .. } => "delete-local-branch",
        LegacyDeleteRemoteBranch { .. } => "legacy-delete-remote-branch",
        ResetCommit { .. } => "reset-commit",
        RestoreBackup { .. } => "restore-backup",
        RemoveCreatedFile { .. } => "remove-created-file",
    }
}

/// Human label for a lifecycle phase.
fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Mutating => "mutating",
        Phase::Pushing => "pushing",
        Phase::Pushed => "pushed",
        Phase::Finalizing => "finalizing",
    }
}

/// Human label for a per-step journal status.
fn status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Applied => "applied",
        StepStatus::Done => "done",
        StepStatus::Failed => "failed",
        StepStatus::SkippedLegacy => "skipped-legacy",
    }
}

/// Handle rollback commands
pub fn handle_rollback(action: RollbackAction) -> Result<()> {
    match action {
        RollbackAction::List => list_recovery_states(),
        RollbackAction::Execute {
            transaction_id,
            force,
            yes,
        } => execute_recovery(&transaction_id, force, yes),
        RollbackAction::Validate { transaction_id } => validate_recovery(&transaction_id),
        RollbackAction::Cleanup {
            transaction_id,
            older_than,
        } => cleanup_recovery_states(transaction_id, older_than),
    }
}

/// List all available recovery states
fn list_recovery_states() -> Result<()> {
    let states = Transaction::list_recovery_states()?;

    if states.is_empty() {
        println!("{}", "📋 No recovery states found".green());
        return Ok(());
    }

    println!("{}", "📋 Available Recovery States:".bold().blue());
    println!();

    for state in &states {
        let created_time = DateTime::parse_from_rfc3339(&state.created_at)?.with_timezone(&Utc);
        let age = Utc::now().signed_duration_since(created_time);

        println!(
            "{}",
            format!("🔄 Transaction: {}", state.transaction_id).bold()
        );
        println!("   Change ID: {}", state.change_id);
        println!("   Repository: {}", state.repo_path.display());
        println!(
            "   Created: {} ({} ago)",
            created_time.format("%Y-%m-%d %H:%M:%S UTC"),
            format_duration(age)
        );
        println!("   Steps: {}", state.steps.len());

        if !state.steps.is_empty() {
            println!("   Step Types:");
            let mut type_counts = std::collections::BTreeMap::new();
            for entry in &state.steps {
                *type_counts.entry(step_kind(&entry.step)).or_insert(0) += 1;
            }
            for (kind, count) in type_counts {
                println!("     {} {}: {}", "•".blue(), kind, count);
            }
        }
        println!();
    }

    println!(
        "{}",
        format!("📊 Total recovery states: {}", states.len()).bold()
    );
    Ok(())
}

/// Execute recovery for a specific transaction. Prints the phase-aware plan,
/// validates (unless `--force`), prompts for confirmation (unless `--yes`;
/// fail-closed on non-interactive stdin), then dispatches on the recorded phase.
/// This path NEVER mutates a remote: `mutating` reverses fully, `pushed`/
/// `finalizing` (and `pushing` with the branch already on the remote) keep the
/// pushed work and only restore the environment.
fn execute_recovery(transaction_id: &str, force: bool, yes: bool) -> Result<()> {
    info!("Starting recovery for transaction: {transaction_id} (force={force} yes={yes})");

    // Load the transaction state and print the plan.
    let state = Transaction::load_recovery_state(transaction_id)?;

    // Per-repo lock (Phase 7 [F6]): a second concurrent gx invocation must not
    // interleave a mutation on this repo with the recovery interpreter. Held
    // for the rest of this function (validate, confirm, execute).
    let _lock = RepoLock::acquire(&state.repo_path)
        .with_context(|| format!("Cannot execute recovery {transaction_id}"))?;

    print_recovery_plan(&state)?;

    // Validate before executing unless forced (`--force` == skip validation only).
    if !force {
        println!("{}", "🔍 Validating recovery operations...".yellow());
        let (errors, warnings) = validate_recovery_state(&state);

        if !errors.is_empty() {
            println!("{}", "❌ Validation failed:".red().bold());
            for error in &errors {
                println!("   {} {}", "•".red(), error.red());
            }
            println!();
            println!(
                "{}",
                "Use --force to skip validation and execute anyway.".yellow()
            );
            return Err(eyre::eyre!("Recovery validation failed"));
        }

        if !warnings.is_empty() {
            println!("{}", "🚨  Validation warnings:".yellow().bold());
            for warning in &warnings {
                println!("   {} {}", "•".yellow(), warning.yellow());
            }
            println!();
        }

        println!("{}", "✅ Validation passed".green());
        println!();
    }

    // Confirm before executing unless --yes; fail closed on non-interactive stdin.
    if !confirm_execute(transaction_id, yes)? {
        println!("{}", "Aborted; no changes made.".yellow());
        return Ok(());
    }

    // Execute the recovery, dispatching on phase inside the engine.
    println!("{}", "🔄 Executing rollback operations...".blue());
    let outcome = Transaction::execute_recovery(transaction_id)?;

    println!();
    match outcome {
        RecoveryOutcome::FullReverse => {
            println!("{}", "✅ Recovery completed successfully!".green().bold());
        }
        RecoveryOutcome::KeepWork { branch } => {
            println!(
                "{}",
                "✅ Environment restored; pushed work retained."
                    .green()
                    .bold()
            );
            match branch {
                Some(b) => println!("   Retained branch: {b}"),
                None => println!("   The pushed branch was retained."),
            }
            println!(
                "{}",
                "   Run `gx undo <change-id>` to reverse the pushed work (closes the PR first)."
                    .yellow()
            );
        }
    }
    Ok(())
}

/// Print the phase-aware recovery plan: phase, age, branch, and each step with
/// its journal status.
fn print_recovery_plan(state: &RecoveryState) -> Result<()> {
    let created = DateTime::parse_from_rfc3339(&state.created_at)?.with_timezone(&Utc);
    let age = Utc::now().signed_duration_since(created);

    println!(
        "{}",
        format!("🔄 Recovery plan for: {}", state.transaction_id)
            .bold()
            .blue()
    );
    println!("   Change ID: {}", state.change_id);
    println!("   Phase: {}", phase_label(state.phase));
    if let Some(branch) = &state.branch {
        println!("   Branch: {branch}");
    }
    println!(
        "   Created: {} ({} ago)",
        created.format("%Y-%m-%d %H:%M:%S UTC"),
        format_duration(age)
    );
    println!("   Steps: {}", state.steps.len());
    for entry in &state.steps {
        println!(
            "     {} {} [{}]",
            "•".blue(),
            step_kind(&entry.step),
            status_label(entry.status)
        );
    }
    println!();
    Ok(())
}

/// Prompt for confirmation before executing recovery. Fails closed on a
/// non-interactive stdin (pass `--yes` for automation).
fn confirm_execute(transaction_id: &str, yes: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if yes {
        debug!("--yes supplied; skipping rollback confirmation prompt");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(eyre::eyre!(
            "Refusing to execute recovery {transaction_id} without confirmation on non-interactive stdin; pass --yes to proceed"
        ));
    }
    print!("Execute this recovery? (y/N): ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read confirmation from stdin")?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Validate recovery operations without executing
fn validate_recovery(transaction_id: &str) -> Result<()> {
    info!("Validating recovery for transaction: {transaction_id}");

    let state = Transaction::load_recovery_state(transaction_id)?;

    println!(
        "{}",
        format!("🔍 Validating recovery for: {transaction_id}")
            .bold()
            .blue()
    );
    println!("   Created: {}", state.created_at);
    println!("   Steps: {}", state.steps.len());
    println!();

    let (errors, warnings) = validate_recovery_state(&state);

    if errors.is_empty() {
        println!(
            "{}",
            "✅ Validation passed - Recovery is safe to execute"
                .green()
                .bold()
        );
    } else {
        println!(
            "{}",
            "❌ Validation failed - Recovery may not be safe"
                .red()
                .bold()
        );
    }

    if !errors.is_empty() {
        println!();
        println!("{}", "Errors:".red().bold());
        for error in &errors {
            println!("   {} {}", "•".red(), error.red());
        }
    }

    if !warnings.is_empty() {
        println!();
        println!("{}", "Warnings:".yellow().bold());
        for warning in &warnings {
            println!("   {} {}", "•".yellow(), warning.yellow());
        }
    }

    if errors.is_empty() && warnings.is_empty() {
        println!();
        println!("{}", "No issues detected.".green());
    }

    Ok(())
}

/// Clean up recovery states
fn cleanup_recovery_states(
    transaction_id: Option<String>,
    older_than: Option<String>,
) -> Result<()> {
    if let Some(id) = transaction_id {
        // Clean up specific transaction
        println!("{}", format!("🧹 Cleaning up recovery state: {id}").blue());
        Transaction::cleanup_recovery_state_by_id(&id)?;
        println!("{}", "✅ Recovery state cleaned up successfully".green());
        return Ok(());
    }

    // Clean up based on age or all states
    let states = Transaction::list_recovery_states()?;

    if states.is_empty() {
        println!("{}", "📋 No recovery states to clean up".green());
        return Ok(());
    }

    let mut states_to_clean = Vec::new();

    if let Some(ref duration_str) = older_than {
        let cutoff_duration = parse_duration(duration_str)?;
        let cutoff_time = Utc::now() - cutoff_duration;

        for state in &states {
            let created_time = DateTime::parse_from_rfc3339(&state.created_at)?.with_timezone(&Utc);

            if created_time < cutoff_time {
                states_to_clean.push(state);
            }
        }

        if states_to_clean.is_empty() {
            println!(
                "{}",
                format!("📋 No recovery states older than {duration_str} found").green()
            );
            return Ok(());
        }

        println!(
            "{}",
            format!(
                "🧹 Cleaning up {} recovery states older than {}...",
                states_to_clean.len(),
                duration_str
            )
            .blue()
        );
    } else {
        // Clean up all states
        states_to_clean.extend(&states);
        println!(
            "{}",
            format!(
                "🧹 Cleaning up all {} recovery states...",
                states_to_clean.len()
            )
            .blue()
        );
    }

    let mut cleaned_count = 0;
    let mut failed_count = 0;

    for state in states_to_clean {
        match Transaction::cleanup_recovery_state_by_id(&state.transaction_id) {
            Ok(()) => {
                debug!("Cleaned up recovery state: {}", state.transaction_id);
                cleaned_count += 1;
            }
            Err(e) => {
                error!(
                    "Failed to clean up recovery state {}: {}",
                    state.transaction_id, e
                );
                failed_count += 1;
            }
        }
    }

    println!();
    if failed_count == 0 {
        println!(
            "{}",
            format!("✅ Successfully cleaned up {cleaned_count} recovery states")
                .green()
                .bold()
        );
    } else {
        println!(
            "{}",
            format!("🚨  Cleaned up {cleaned_count} recovery states, {failed_count} failed")
                .yellow()
                .bold()
        );
    }

    // Also clean up old change states if a duration was specified
    if let Some(duration_str) = older_than.as_ref() {
        if let Ok(cutoff_duration) = parse_duration(duration_str) {
            let days = cutoff_duration.num_days() as u64;
            if days > 0 {
                if let Ok(state_manager) = StateManager::new() {
                    match state_manager.cleanup_old(days) {
                        Ok(deleted) if deleted > 0 => {
                            println!(
                                "{}",
                                format!("🧹 Also cleaned up {deleted} old change states").blue()
                            );
                        }
                        Err(e) => {
                            warn!("Failed to cleanup old change states: {}", e);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
}

/// Format duration for human-readable display
fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.num_seconds();

    if total_seconds < 60 {
        format!("{total_seconds}s")
    } else if total_seconds < 3600 {
        format!("{}m", duration.num_minutes())
    } else if total_seconds < 86400 {
        format!("{}h", duration.num_hours())
    } else {
        format!("{}d", duration.num_days())
    }
}

/// Parse duration string (e.g., "7d", "24h", "30m")
fn parse_duration(duration_str: &str) -> Result<Duration> {
    if duration_str.is_empty() {
        return Err(eyre::eyre!("Duration string cannot be empty"));
    }

    let (number_part, unit_part) =
        if let Some(pos) = duration_str.chars().position(|c| c.is_alphabetic()) {
            duration_str.split_at(pos)
        } else {
            return Err(eyre::eyre!("Duration must include a unit (d, h, m, s)"));
        };

    let number: i64 = number_part
        .parse()
        .map_err(|_| eyre::eyre!("Invalid number in duration: {}", number_part))?;

    match unit_part.to_lowercase().as_str() {
        "s" | "sec" | "second" | "seconds" => Ok(Duration::seconds(number)),
        "m" | "min" | "minute" | "minutes" => Ok(Duration::minutes(number)),
        "h" | "hr" | "hour" | "hours" => Ok(Duration::hours(number)),
        "d" | "day" | "days" => Ok(Duration::days(number)),
        _ => Err(eyre::eyre!(
            "Invalid duration unit: {}. Use s, m, h, or d",
            unit_part
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::seconds(30)), "30s");
        assert_eq!(format_duration(Duration::minutes(5)), "5m");
        assert_eq!(format_duration(Duration::hours(2)), "2h");
        assert_eq!(format_duration(Duration::days(3)), "3d");
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::seconds(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::minutes(5));
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));

        assert!(parse_duration("").is_err());
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("30x").is_err());
    }
}
