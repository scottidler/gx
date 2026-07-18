//! The blocking bodies behind each MCP tool: discover/read/mutate via gx's
//! cores, then map their structured results into the wire types in
//! [`crate::mcp::schema`]. Every function here is synchronous and blocking (it calls
//! gx cores that shell out to git/gh and build rayon pools); the async tool
//! wrappers in [`crate::mcp::server`] run each under `tokio::task::spawn_blocking` so
//! the runtime is never blocked (rust.md: blocking work off the async runtime).
//!
//! Confirm-token protocol (design doc, Chunk B):
//! - `create_propose` returns the token `execute_propose` minted over the
//!   canonical `manifest.json`; `create_apply` demands it back and
//!   `execute_apply` refuses on mismatch (Phase 5's real machinery, reused).
//! - the undo plan is NOT persisted, so `undo_plan` computes a token over the
//!   reconciled plan and `undo_execute` recomputes it and refuses on mismatch
//!   (state changed between plan and execute).

use crate::confirm::Confirmation;
use crate::create::manifest::{FileAction, ProposalManifest, ProposalOutcome};
use crate::state::StateManager;
use crate::undo::core::UndoPlanSet;
use eyre::{bail, Result};
use local::config::Config;
use local::git::RemoteStatus;
use log::debug;
use std::env;
use std::path::Path;

use crate::mcp::schema::*;

/// Effective parallel job count (config `jobs`, else nproc, else 4).
fn jobs(config: &Config) -> usize {
    local::utils::get_jobs_from_config(config)
        .or_else(local::utils::get_nproc)
        .unwrap_or(4)
}

/// Effective repo-discovery max depth (config, else 2 as `gx status` uses).
fn max_depth(config: &Config) -> usize {
    local::utils::get_max_depth_from_config(config).unwrap_or(2)
}

/// Discover + filter the fleet under the server's CWD by the given patterns.
fn discover(config: &Config, patterns: &[String]) -> Result<Vec<local::repo::Repo>> {
    let start_dir = env::current_dir()?;
    let repos =
        local::repo::discover_repos(&start_dir, max_depth(config), &config.ignore_patterns())?;
    Ok(local::repo::filter_repos(repos, patterns))
}

/// Short human string for a repo's remote-tracking state.
fn remote_label(status: &RemoteStatus) -> String {
    match status {
        RemoteStatus::UpToDate => "up-to-date".to_string(),
        RemoteStatus::Ahead(n) => format!("ahead {n}"),
        RemoteStatus::Behind(n) => format!("behind {n}"),
        RemoteStatus::Diverged(a, b) => format!("diverged +{a}/-{b}"),
        RemoteStatus::NoRemote => "no-remote".to_string(),
        RemoteStatus::NoUpstream => "no-upstream".to_string(),
        RemoteStatus::DetachedHead => "detached".to_string(),
        RemoteStatus::Error(e) => format!("error: {e}"),
    }
}

// ------------------------------------------------------------------ read-only

pub fn repo_discover(config: &Config, patterns: &[String]) -> Result<Vec<RepoRef>> {
    debug!("logic::repo_discover: patterns={patterns:?}");
    let repos = discover(config, patterns)?;
    Ok(repos
        .into_iter()
        .map(|r| RepoRef {
            slug: r.slug,
            path: r.path.display().to_string(),
        })
        .collect())
}

pub fn status(
    config: &Config,
    patterns: &[String],
    fetch_remote: bool,
) -> Result<Vec<RepoStatusSummary>> {
    debug!("logic::status: patterns={patterns:?} fetch_remote={fetch_remote}");
    let repos = discover(config, patterns)?;
    // no_remote is the inverse of fetch_remote: local-only unless opted in.
    let no_remote = !fetch_remote;
    Ok(repos
        .iter()
        .map(|repo| {
            let rs = crate::git::get_repo_status_with_options(repo, false, no_remote);
            RepoStatusSummary {
                slug: repo.slug.clone(),
                branch: rs.branch,
                clean: rs.is_clean,
                remote: remote_label(&rs.remote_status),
                error: rs.error,
            }
        })
        .collect())
}

pub fn change_list() -> Result<Vec<ChangeSummary>> {
    debug!("logic::change_list");
    let states = StateManager::new()?.list()?;
    Ok(states
        .into_iter()
        .map(|s| ChangeSummary {
            change_id: s.change_id,
            status: format!("{:?}", s.status),
            description: s.description,
            repos: s.repositories.len(),
            updated_at: s.updated_at.to_rfc3339(),
        })
        .collect())
}

pub fn change_get(change_id: &str, slug: Option<&str>) -> Result<ChangeDetail> {
    debug!("logic::change_get: change_id={change_id} slug={slug:?}");
    let state = StateManager::new()?.load(change_id)?.ok_or_else(|| {
        eyre::eyre!("no change state for {change_id} (never recorded, or already cleaned up)")
    })?;

    let repos = state
        .repositories
        .values()
        .map(|r| RepoChangeSummary {
            slug: r.repo_slug.clone(),
            status: format!("{:?}", r.status),
            branch: r.branch_name.clone(),
            pr_number: r.pr_number,
            pr_url: r.pr_url.clone(),
        })
        .collect();

    // Proposal diffs (full patches): change-get is the full-diff fetch (design
    // doc), unlike create-propose's summaries. Present iff a manifest exists.
    let dir = crate::create::manifest::proposal_dir(change_id)?;
    let proposal = if dir.join("manifest.json").exists() {
        let manifest = crate::create::manifest::load_manifest(&dir)?;
        Some(proposal_detail(&dir, &manifest, slug))
    } else {
        None
    };

    Ok(ChangeDetail {
        change_id: state.change_id,
        status: format!("{:?}", state.status),
        description: state.description,
        repos,
        proposal,
    })
}

fn proposal_detail(dir: &Path, manifest: &ProposalManifest, only: Option<&str>) -> ProposalDetail {
    let repos = manifest
        .repos
        .iter()
        .filter(|rp| only.is_none_or(|s| s == rp.slug))
        .map(|rp| {
            let patch_path = crate::create::manifest::patch_path(dir, &rp.slug);
            let patch = std::fs::read_to_string(&patch_path).ok();
            RepoProposalDetail {
                slug: rp.slug.clone(),
                outcome: outcome_label(rp.outcome),
                files: rp.files.iter().map(|f| f.path.clone()).collect(),
                patch,
            }
        })
        .collect();
    ProposalDetail {
        change_id: manifest.change_id.clone(),
        prompt: manifest.prompt.clone(),
        repos,
    }
}

pub fn review_status() -> Result<Vec<ReviewChange>> {
    debug!("logic::review_status");
    let states = StateManager::new()?.list()?;
    let mut out = Vec::new();
    for s in states {
        let repos: Vec<ReviewRepo> = s
            .repositories
            .values()
            .filter(|r| r.pr_number.is_some() || r.pr_url.is_some())
            .map(|r| ReviewRepo {
                slug: r.repo_slug.clone(),
                status: format!("{:?}", r.status),
                pr_number: r.pr_number,
                pr_url: r.pr_url.clone(),
            })
            .collect();
        // review-status is about PR-bearing campaigns: skip changes with no PRs.
        if !repos.is_empty() {
            out.push(ReviewChange {
                change_id: s.change_id,
                repos,
            });
        }
    }
    Ok(out)
}

pub fn doctor() -> Result<crate::doctor::DoctorReport> {
    debug!("logic::doctor");
    crate::doctor::collect_report()
}

// ------------------------------------------------------------------- mutating

pub fn create_propose(config: &Config, prompt: &str, patterns: &[String]) -> Result<ProposeOut> {
    debug!(
        "logic::create_propose: patterns={patterns:?} prompt_len={}",
        prompt.len()
    );
    let repos = discover(config, patterns)?;
    if repos.is_empty() {
        bail!("no repositories matched the patterns; nothing to propose");
    }
    let change_id = crate::create::generate_change_id();
    let summary = crate::create::core::propose::execute_propose(
        &repos,
        &change_id,
        prompt,
        config,
        jobs(config),
    )?;

    let repos = summary
        .repos
        .iter()
        .map(|rp| {
            let (mut added, mut modified, mut deleted) = (0usize, 0usize, 0usize);
            for f in &rp.files {
                match f.action {
                    FileAction::Add => added += 1,
                    FileAction::Modify => modified += 1,
                    FileAction::Delete => deleted += 1,
                }
            }
            RepoProposeSummary {
                slug: rp.slug.clone(),
                outcome: outcome_label(rp.outcome),
                files: rp.files.iter().map(|f| f.path.clone()).collect(),
                files_changed: rp.files.len(),
                added,
                modified,
                deleted,
                error: rp.error.clone(),
            }
        })
        .collect();

    Ok(ProposeOut {
        change_id: summary.change_id,
        token: summary.token,
        proposed: summary.proposed,
        empty: summary.empty,
        failed: summary.failed,
        repos,
    })
}

pub fn create_apply(config: &Config, change_id: &str, token: &str) -> Result<ApplyOut> {
    debug!(
        "logic::create_apply: change_id={change_id} token_len={}",
        token.len()
    );
    // MCP passes the round-tripped token; execute_apply refuses on mismatch
    // (missing/stale token, or a manifest changed since propose). PR creation
    // is deliberately NOT exposed here: the design's MCP create-apply signature
    // is {change-id, token} (see deviations); a driver opens PRs out-of-band.
    let report = crate::create::core::apply::execute_apply(
        change_id,
        None,
        false,
        false,
        config,
        jobs(config),
        Confirmation::Token(token.to_string()),
    )?;

    let repos = report
        .results
        .iter()
        .map(|r| RepoApplyOut {
            slug: r.repo.slug.clone(),
            status: format!("{:?}", r.action),
            pr_url: r.pr_url.clone(),
            error: r.error.clone(),
        })
        .collect();

    Ok(ApplyOut {
        change_id: report.change_id,
        applied: report.applied,
        drifted_or_failed: report.drifted_or_failed,
        repos,
    })
}

/// Confirm token over a reconciled undo plan. The plan is NOT persisted, so
/// both `undo_plan` (mint) and `undo_execute` (recompute + compare) derive it
/// from the plan's actionable entries; any state change between the two calls
/// shifts the hash and `undo_execute` refuses. Canonicalized: actionable
/// entries sorted by slug, each `slug|status|action|pr`.
pub fn undo_plan_token(plan: &UndoPlanSet) -> String {
    let mut entries: Vec<_> = plan.actionable.iter().collect();
    entries.sort_by(|a, b| a.slug.cmp(&b.slug));
    let mut canon = String::new();
    for e in entries {
        canon.push_str(&format!(
            "{}|{:?}|{:?}|{:?}\n",
            e.slug, e.status, e.action, e.pr_number
        ));
    }
    let hex = local::hash::sha256_hex(canon.as_bytes());
    hex.chars()
        .take(crate::create::manifest::TOKEN_HEX_LEN)
        .collect()
}

pub fn undo_plan(config: &Config, change_id: &str) -> Result<UndoPlanOut> {
    debug!("logic::undo_plan: change_id={change_id}");
    let Some(plan_set) = crate::undo::core::plan_undo(change_id, None, config)? else {
        // Nothing recorded and no recovery files: an empty, tokenless plan.
        return Ok(UndoPlanOut {
            change_id: change_id.to_string(),
            token: String::new(),
            actionable: 0,
            plan: Vec::new(),
        });
    };
    let token = undo_plan_token(&plan_set);
    let plan = plan_set
        .plan
        .iter()
        .map(|p| UndoPlanEntry {
            slug: p.slug.clone(),
            action: format!("{:?}", p.action),
            pr_number: p.pr_number,
            status: p.status.as_ref().map(|s| format!("{s:?}")),
        })
        .collect();
    Ok(UndoPlanOut {
        change_id: change_id.to_string(),
        token,
        actionable: plan_set.actionable.len(),
        plan,
    })
}

pub fn undo_execute(config: &Config, change_id: &str, token: &str) -> Result<UndoExecuteOut> {
    debug!(
        "logic::undo_execute: change_id={change_id} token_len={}",
        token.len()
    );
    let Some(plan_set) = crate::undo::core::plan_undo(change_id, None, config)? else {
        bail!("nothing to undo for {change_id} (no change state and no recovery files)");
    };
    // Recompute the plan token and refuse if it changed since undo-plan: the
    // state (or GitHub reconcile) moved between planning and executing.
    let current = undo_plan_token(&plan_set);
    if token != current {
        bail!(
            "undo plan for {change_id} changed since it was planned (state changed); \
             re-run undo-plan (expected token {current})"
        );
    }
    let outcomes = crate::undo::core::execute_undo(
        &plan_set,
        change_id,
        config,
        jobs(config),
        Confirmation::AlreadyConfirmed,
    )?;
    let repos = outcomes
        .iter()
        .map(|o| UndoOutcomeOut {
            slug: o.slug.clone(),
            outcome: format!("{:?}", o.kind),
            pr_number: o.pr_number,
        })
        .collect();
    Ok(UndoExecuteOut {
        change_id: change_id.to_string(),
        repos,
    })
}

fn outcome_label(outcome: ProposalOutcome) -> String {
    match outcome {
        ProposalOutcome::Proposed => "proposed",
        ProposalOutcome::Empty => "empty",
        ProposalOutcome::Failed => "failed",
    }
    .to_string()
}
