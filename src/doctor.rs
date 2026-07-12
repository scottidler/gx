//! `gx doctor`: check required tools and report (optionally purge) orphaned
//! recovery/backup artifacts left under `$XDG_DATA_HOME/gx` by interrupted runs
//! or deleted repos ([A2], [A24]).

use crate::config::xdg_data_dir;
use crate::state::{ChangeStatus, StateManager};
use crate::transaction::Transaction;
use chrono::{DateTime, Utc};
use eyre::Result;
use log::warn;
use std::path::PathBuf;
use std::process::Command;

const GIT_MIN_VERSION: &str = "2.20.0";
const GH_MIN_VERSION: &str = "2.0.0";
/// Recovery state older than this (and not matching a live repo) is orphaned.
const ARTIFACT_TTL_DAYS: i64 = 7;

/// Run the doctor command.
pub fn run_doctor(purge: bool) -> Result<()> {
    println!("REQUIRED TOOLS:");
    for (tool, min) in [("git", GIT_MIN_VERSION), ("gh", GH_MIN_VERSION)] {
        let status = check_tool_version(tool, min);
        println!(
            "  {} {:<3} {:>12}",
            status.status_icon, tool, status.version
        );
    }

    println!("\nLOG PATH:\n  {}", log_path().display());

    report_orphans(purge)?;
    report_proposal_orphans(purge)?;
    report_stuck_proposals()?;
    Ok(())
}

/// Report bare `Proposed` campaigns (ringer addendum #3): a change state whose
/// aggregate `ChangeStatus` is `Proposed` has persisted artifacts and change
/// state but was never applied OR undone. Distinct from
/// `report_proposal_orphans` (which finds a proposal dir with NO change state
/// at all) - this finds a change state that IS recorded but stuck at the
/// bare-proposal bucket, so an operator can see it and act (`gx apply` or
/// `gx undo`) instead of it sitting invisible in `status`/`review`.
fn report_stuck_proposals() -> Result<()> {
    let stuck = stuck_proposals(StateManager::new()?.list()?);

    println!("\nSTUCK PROPOSALS (proposed, never applied or undone):");
    if stuck.is_empty() {
        println!("  none");
        return Ok(());
    }
    for state in &stuck {
        println!(
            "  {} ({} repo(s), updated {})",
            state.change_id,
            state.repositories.len(),
            state.updated_at.to_rfc3339()
        );
    }
    println!("  (run `gx apply <change-id>` to apply, or `gx undo <change-id>` to discard)");
    Ok(())
}

/// Pure filter: every change state sitting at the bare-proposal aggregate
/// bucket, sorted by change-id. Extracted from [`report_stuck_proposals`] so
/// the "which campaigns are stuck" logic is testable without capturing stdout.
fn stuck_proposals(states: Vec<crate::state::ChangeState>) -> Vec<crate::state::ChangeState> {
    let mut stuck: Vec<_> = states
        .into_iter()
        .filter(|s| s.status == ChangeStatus::Proposed)
        .collect();
    stuck.sort_by(|a, b| a.change_id.cmp(&b.change_id));
    stuck
}

/// Report (and optionally purge) orphaned proposal directories: a
/// `proposals/<change-id>/` whose change-state file no longer exists (the change
/// was cleaned up / undone / never recorded), so the artifacts are dangling.
/// Retention normally removes these when a change reaches its cleaned-up
/// terminal (via `gx undo` / `gx cleanup`); this is the safety net for the ones
/// a crash left behind (design Data Model: `gx doctor` reports orphaned
/// proposal dirs).
fn report_proposal_orphans(purge: bool) -> Result<()> {
    let Some(base) = xdg_data_dir().map(|d| d.join("gx")) else {
        return Ok(());
    };
    let proposals = base.join("proposals");
    let changes = base.join("changes");

    let mut orphans = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&proposals) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(change_id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // Orphaned iff no change-state file references this change-id.
            if !changes.join(format!("{change_id}.json")).exists() {
                orphans.push(change_id);
            }
        }
    }
    orphans.sort();

    println!("\nORPHANED PROPOSALS:");
    if orphans.is_empty() {
        println!("  none");
        return Ok(());
    }
    for change_id in &orphans {
        println!("  {change_id} (no change state)");
        if purge {
            purge_proposal(&proposals.join(change_id));
        }
    }
    if !purge {
        println!("  (run `gx doctor --purge` to remove these via rkvr)");
    }
    Ok(())
}

/// Remove an orphaned proposal directory via `rkvr` (never `rm`), matching the
/// recovery/backup purge path.
fn purge_proposal(dir: &std::path::Path) {
    if !dir.exists() {
        return;
    }
    match Command::new("rkvr").arg("rmrf").arg(dir).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => warn!(
            "rkvr failed to remove {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => warn!("Failed to run rkvr for {}: {}", dir.display(), e),
    }
}

/// The log file path, rendered from the same XDG source the logger uses.
pub fn log_path() -> PathBuf {
    xdg_data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gx")
        .join("logs")
        .join("gx.log")
}

struct ToolStatus {
    version: String,
    status_icon: String,
}

/// Check a tool's `--version` and whether it meets the minimum.
fn check_tool_version(tool: &str, min_version: &str) -> ToolStatus {
    match Command::new(tool).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = extract_version(&version_output);
            let meets = version_compare(version.trim_start_matches('v'), min_version);
            ToolStatus {
                version: if version.is_empty() {
                    "unknown".to_string()
                } else {
                    version
                },
                status_icon: if meets { "✅" } else { "🚨" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "❌".to_string(),
        },
    }
}

/// Extract a `x.y.z` version from `<tool> version x.y.z ...` output.
fn extract_version(output: &str) -> String {
    output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(2))
        .unwrap_or("unknown")
        .to_string()
}

/// Semantic-version comparison, padding the shorter side with zeros ([A25]).
fn version_compare(version: &str, min_version: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let mut v1 = parse(version);
    let mut v2 = parse(min_version);
    let len = v1.len().max(v2.len());
    v1.resize(len, 0);
    v2.resize(len, 0);
    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    true
}

/// Report (and optionally purge) orphaned recovery/backup artifacts.
fn report_orphans(purge: bool) -> Result<()> {
    let states = Transaction::list_recovery_states()?;
    let now = Utc::now();

    let mut orphans = Vec::new();
    let mut failed = Vec::new();
    for state in &states {
        let repo_gone = !state.repo_path.exists();
        let stale = DateTime::parse_from_rfc3339(&state.created_at)
            .map(|t| {
                now.signed_duration_since(t.with_timezone(&Utc)).num_days() > ARTIFACT_TTL_DAYS
            })
            .unwrap_or(false);
        if repo_gone || stale {
            // A stale / repo-gone file ages out as an orphan even if it carries
            // failed steps — nothing left to converge against.
            let reason = if repo_gone { "repo missing" } else { "stale" };
            orphans.push((state.transaction_id.clone(), reason));
        } else if state.has_failed_steps() {
            // Live and recent, but a rollback left failed steps: this is
            // retained evidence, NOT a purge candidate.
            failed.push((state.transaction_id.clone(), state.failed_step_count()));
        }
    }

    println!("\nRECOVERY (FAILED STEPS):");
    if failed.is_empty() {
        println!("  none");
    } else {
        for (tx_id, count) in &failed {
            println!("  {tx_id} ({count} failed step(s); re-run: gx rollback execute {tx_id})");
        }
    }

    println!("\nORPHANED ARTIFACTS:");
    if orphans.is_empty() {
        println!("  none");
        return Ok(());
    }

    for (tx_id, reason) in &orphans {
        println!("  {tx_id} ({reason})");
        if purge {
            purge_artifact(tx_id);
        }
    }
    if !purge {
        println!("  (run `gx doctor --purge` to remove these via rkvr)");
    }

    Ok(())
}

/// Remove a transaction's recovery file and backup dir via `rkvr` (never `rm`).
fn purge_artifact(tx_id: &str) {
    let Some(base) = xdg_data_dir().map(|d| d.join("gx")) else {
        return;
    };
    let recovery = base.join("recovery").join(format!("{tx_id}.json"));
    let backups = base.join("backups").join(tx_id);
    for path in [recovery, backups] {
        if path.exists() {
            match Command::new("rkvr").arg("rmrf").arg(&path).output() {
                Ok(out) if out.status.success() => {}
                Ok(out) => warn!(
                    "rkvr failed to remove {}: {}",
                    path.display(),
                    String::from_utf8_lossy(&out.stderr)
                ),
                Err(e) => warn!("Failed to run rkvr for {}: {}", path.display(), e),
            }
        }
    }
}

#[cfg(test)]
mod tests;
