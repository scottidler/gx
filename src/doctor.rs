//! `gx doctor`: check required tools and report (optionally purge) orphaned
//! recovery/backup artifacts left under `$XDG_DATA_HOME/gx` by interrupted runs
//! or deleted repos ([A2], [A24]).

use crate::config::xdg_data_dir;
use crate::state::{ChangeStatus, StateManager};
use crate::transaction::Transaction;
use chrono::{DateTime, Utc};
use eyre::Result;
use log::{debug, warn};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

const GIT_MIN_VERSION: &str = "2.20.0";
const GH_MIN_VERSION: &str = "2.0.0";
/// Recovery state older than this (and not matching a live repo) is orphaned.
const ARTIFACT_TTL_DAYS: i64 = 7;

/// One required-tool version check.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCheck {
    pub name: String,
    pub version: String,
    /// Whether the found version meets the minimum (false if missing).
    pub ok: bool,
}

/// A recovery file whose repo is gone or that has aged past the TTL: a purge
/// candidate.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryOrphan {
    pub tx_id: String,
    pub reason: String,
}

/// A live, recent recovery file that a rollback left with failed steps:
/// retained evidence, NOT a purge candidate.
#[derive(Debug, Clone, Serialize)]
pub struct FailedRecovery {
    pub tx_id: String,
    pub failed_steps: usize,
}

/// A change stuck at the bare-proposal aggregate status (never applied/undone).
#[derive(Debug, Clone, Serialize)]
pub struct StuckProposal {
    pub change_id: String,
    pub repos: usize,
    pub updated_at: String,
}

/// The structured `gx doctor` report (design doc: read surfaces become cores
/// returning structured results). Both the CLI (`run_doctor`, which renders it)
/// and the `gx-mcp` `doctor` tool (which serializes it) consume this - the
/// gather logic lives here once, never duplicated across a print path and a
/// data path.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub tools: Vec<ToolCheck>,
    pub log_path: String,
    pub failed_recovery: Vec<FailedRecovery>,
    pub orphaned_artifacts: Vec<RecoveryOrphan>,
    pub orphaned_proposals: Vec<String>,
    pub stuck_proposals: Vec<StuckProposal>,
}

/// Gather the full structured doctor report WITHOUT printing or purging: the
/// read core behind both `run_doctor` and the MCP `doctor` tool.
pub fn collect_report() -> Result<DoctorReport> {
    debug!("collect_report: gathering doctor report");
    let tools = [("git", GIT_MIN_VERSION), ("gh", GH_MIN_VERSION)]
        .into_iter()
        .map(|(tool, min)| {
            let status = check_tool_version(tool, min);
            ToolCheck {
                name: tool.to_string(),
                version: status.version,
                ok: status.ok,
            }
        })
        .collect();

    let (failed_recovery, orphaned_artifacts) = gather_recovery()?;
    let orphaned_proposals = gather_proposal_orphans();
    let stuck_proposals = stuck_proposals(StateManager::new()?.list()?)
        .into_iter()
        .map(|s| StuckProposal {
            change_id: s.change_id,
            repos: s.repositories.len(),
            updated_at: s.updated_at.to_rfc3339(),
        })
        .collect();

    Ok(DoctorReport {
        tools,
        log_path: log_path().display().to_string(),
        failed_recovery,
        orphaned_artifacts,
        orphaned_proposals,
        stuck_proposals,
    })
}

/// Run the doctor command: render the structured report and (optionally) purge.
pub fn run_doctor(purge: bool) -> Result<()> {
    let report = collect_report()?;

    println!("REQUIRED TOOLS:");
    for tool in &report.tools {
        let icon = if tool.version == "not found" {
            "❌"
        } else if tool.ok {
            "✅"
        } else {
            "🚨"
        };
        println!("  {} {:<3} {:>12}", icon, tool.name, tool.version);
    }

    println!("\nLOG PATH:\n  {}", report.log_path);

    render_orphans(&report, purge);
    render_proposal_orphans(&report, purge);
    render_stuck_proposals(&report);
    Ok(())
}

/// Report bare `Proposed` campaigns (ringer addendum #3): a change state whose
/// aggregate `ChangeStatus` is `Proposed` has persisted artifacts and change
/// state but was never applied OR undone. Distinct from
/// `report_proposal_orphans` (which finds a proposal dir with NO change state
/// at all) - this finds a change state that IS recorded but stuck at the
/// bare-proposal bucket, so an operator can see it and act (`gx apply` or
/// `gx undo`) instead of it sitting invisible in `status`/`review`.
fn render_stuck_proposals(report: &DoctorReport) {
    println!("\nSTUCK PROPOSALS (proposed, never applied or undone):");
    if report.stuck_proposals.is_empty() {
        println!("  none");
        return;
    }
    for state in &report.stuck_proposals {
        println!(
            "  {} ({} repo(s), updated {})",
            state.change_id, state.repos, state.updated_at
        );
    }
    println!("  (run `gx apply <change-id>` to apply, or `gx undo <change-id>` to discard)");
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
fn gather_proposal_orphans() -> Vec<String> {
    let Some(base) = xdg_data_dir().map(|d| d.join("gx")) else {
        return Vec::new();
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
    orphans
}

fn render_proposal_orphans(report: &DoctorReport, purge: bool) {
    let proposals = xdg_data_dir().map(|d| d.join("gx").join("proposals"));

    println!("\nORPHANED PROPOSALS:");
    if report.orphaned_proposals.is_empty() {
        println!("  none");
        return;
    }
    for change_id in &report.orphaned_proposals {
        println!("  {change_id} (no change state)");
        if purge {
            if let Some(proposals) = &proposals {
                purge_proposal(&proposals.join(change_id));
            }
        }
    }
    if !purge {
        println!("  (run `gx doctor --purge` to remove these via rkvr)");
    }
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
    /// Whether the found version meets the minimum. `false` when the tool is
    /// missing (`version == "not found"`), so the renderer distinguishes the
    /// missing (❌) from the too-old (🚨) case by the version string.
    ok: bool,
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
                ok: meets,
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            ok: false,
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

/// Gather orphaned recovery/backup artifacts and failed-step recovery files
/// (the data behind both the CLI render and the MCP `doctor` tool).
fn gather_recovery() -> Result<(Vec<FailedRecovery>, Vec<RecoveryOrphan>)> {
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
            orphans.push(RecoveryOrphan {
                tx_id: state.transaction_id.clone(),
                reason: reason.to_string(),
            });
        } else if state.has_failed_steps() {
            // Live and recent, but a rollback left failed steps: this is
            // retained evidence, NOT a purge candidate.
            failed.push(FailedRecovery {
                tx_id: state.transaction_id.clone(),
                failed_steps: state.failed_step_count(),
            });
        }
    }
    Ok((failed, orphans))
}

/// Render (and optionally purge) the recovery/artifact sections of the report.
fn render_orphans(report: &DoctorReport, purge: bool) {
    println!("\nRECOVERY (FAILED STEPS):");
    if report.failed_recovery.is_empty() {
        println!("  none");
    } else {
        for fr in &report.failed_recovery {
            println!(
                "  {} ({} failed step(s); re-run: gx rollback execute {})",
                fr.tx_id, fr.failed_steps, fr.tx_id
            );
        }
    }

    println!("\nORPHANED ARTIFACTS:");
    if report.orphaned_artifacts.is_empty() {
        println!("  none");
        return;
    }

    for orphan in &report.orphaned_artifacts {
        println!("  {} ({})", orphan.tx_id, orphan.reason);
        if purge {
            purge_artifact(&orphan.tx_id);
        }
    }
    if !purge {
        println!("  (run `gx doctor --purge` to remove these via rkvr)");
    }
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
