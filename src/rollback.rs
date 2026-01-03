use crate::cli::RollbackAction;
use crate::state::StateManager;
use crate::transaction::{validate_rollback_operations, Transaction};
use chrono::{DateTime, Duration, Utc};
use colored::*;
use eyre::Result;
use log::{debug, error, info, warn};

/// Handle rollback commands
pub fn handle_rollback(action: RollbackAction) -> Result<()> {
    match action {
        RollbackAction::List => list_recovery_states(),
        RollbackAction::Execute {
            transaction_id,
            force,
        } => execute_recovery(&transaction_id, force),
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
        println!(
            "   Created: {} ({} ago)",
            created_time.format("%Y-%m-%d %H:%M:%S UTC"),
            format_duration(age)
        );
        println!("   Operations: {}", state.operation_count);
        println!("   Rollback Actions: {}", state.rollback_actions.len());
        println!("   Rollback Points: {}", state.rollback_points.len());

        if !state.rollback_actions.is_empty() {
            println!("   Action Types:");
            let mut type_counts = std::collections::HashMap::new();
            for action in &state.rollback_actions {
                *type_counts
                    .entry(format!("{:?}", action.operation_type))
                    .or_insert(0) += 1;
            }
            for (op_type, count) in type_counts {
                println!("     {} {}: {}", "•".blue(), op_type, count);
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

/// Execute recovery for a specific transaction
fn execute_recovery(transaction_id: &str, force: bool) -> Result<()> {
    info!("Starting recovery for transaction: {transaction_id}");

    // Load the transaction state
    let state = Transaction::load_recovery_state(transaction_id)?;

    println!(
        "{}",
        format!("🔄 Executing recovery for: {transaction_id}")
            .bold()
            .blue()
    );
    println!("   Created: {}", state.created_at);
    println!("   Operations: {}", state.operation_count);
    println!("   Rollback Actions: {}", state.rollback_actions.len());
    println!();

    // Validate before executing unless forced
    if !force {
        println!("{}", "🔍 Validating recovery operations...".yellow());
        let validation = validate_rollback_operations(&state.rollback_actions);

        if !validation.is_valid() {
            println!("{}", "❌ Validation failed:".red().bold());
            for error in &validation.errors {
                println!("   {} {}", "•".red(), error.red());
            }
            println!();
            println!(
                "{}",
                "Use --force to skip validation and execute anyway.".yellow()
            );
            return Err(eyre::eyre!("Recovery validation failed"));
        }

        if !validation.warnings.is_empty() {
            println!("{}", "⚠️  Validation warnings:".yellow().bold());
            for warning in &validation.warnings {
                println!("   {} {}", "•".yellow(), warning.yellow());
            }
            println!();
        }

        println!("{}", "✅ Validation passed".green());
        println!();
    }

    // Execute the recovery
    println!("{}", "🔄 Executing rollback operations...".blue());
    Transaction::execute_recovery(transaction_id)?;

    println!();
    println!("{}", "✅ Recovery completed successfully!".green().bold());
    Ok(())
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
    println!("   Operations: {}", state.operation_count);
    println!("   Rollback Actions: {}", state.rollback_actions.len());
    println!();

    let validation = validate_rollback_operations(&state.rollback_actions);

    // Display validation results
    if validation.is_valid() {
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

    if !validation.errors.is_empty() {
        println!();
        println!("{}", "Errors:".red().bold());
        for error in &validation.errors {
            println!("   {} {}", "•".red(), error.red());
        }
    }

    if !validation.warnings.is_empty() {
        println!();
        println!("{}", "Warnings:".yellow().bold());
        for warning in &validation.warnings {
            println!("   {} {}", "•".yellow(), warning.yellow());
        }
    }

    if validation.errors.is_empty() && validation.warnings.is_empty() {
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
            format!("⚠️  Cleaned up {cleaned_count} recovery states, {failed_count} failed")
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
