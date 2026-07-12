//! The `gx apply <change-id>` APPLY pass (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 5 + "Chunk A" apply):
//! convert a persisted proposal into the INTERNAL deterministic
//! [`Change::Patchset`](super::Change::Patchset) and drive it through the
//! UNCHANGED `process_single_repo` pipeline (stash/switch/pull, post-pull drift
//! + per-blob hash refusals, then branch/commit/push/PR).
//!
//! This is the core `gx apply` calls into (mirroring `gx undo`): it owns the
//! `ChangeLock`, loads the manifest + recomputes the confirm token from the RAW
//! on-disk bytes, verifies a caller-supplied token, resolves the `Proposed`
//! repos from change state, applies, and reconciles the resulting state so a
//! drifted/failed repo STAYS `Proposed` with its error (a normal partial-apply
//! outcome; the remedy is a fresh propose for the stragglers). Never prints -
//! the CLI wrapper (`create::process_apply_command`, refined by Phase 6's
//! present gate) and the future MCP `create-apply` tool render.

use super::manifest::{self, ProposalOutcome};
use super::{execute_create, Change, CreateAction, CreateResult};
use crate::config::Config;
use crate::confirm::Confirmation;
use crate::repo::Repo;
use crate::state::{ChangeState, RepoChangeStatus, StateManager};
use eyre::{Context, Result};
use log::{debug, info, warn};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The outcome of an apply pass, for the caller (CLI wrapper today; MCP
/// `create-apply` later) to render.
#[derive(Debug)]
pub struct ApplyReport {
    pub change_id: String,
    /// The confirm token recomputed from the on-disk `manifest.json` bytes.
    pub token: String,
    /// The per-repo apply results (the same `CreateResult` shape a `gx create`
    /// produces, since apply rides the identical pipeline).
    pub results: Vec<CreateResult>,
    /// Repos that committed/pushed (a PR-creation or stash error still counts as
    /// applied: the branch DID land).
    pub applied: usize,
    /// Repos that drifted or failed BEFORE committing; each stays `Proposed`.
    pub drifted_or_failed: usize,
}

/// Apply the persisted proposal for `change_id`. See the module doc for the
/// full contract; refuses loudly (nothing written) on a missing proposal, a
/// token mismatch, a post-pull `base_sha` drift, or a tampered blob.
pub fn execute_apply(
    change_id: &str,
    commit_message: Option<&str>,
    pr: Option<&crate::cli::PR>,
    config: &Config,
    parallel_jobs: usize,
    confirmation: Confirmation,
) -> Result<ApplyReport> {
    debug!("execute_apply: change_id={change_id} confirmation={confirmation:?}");

    // 1. Proposal artifacts must exist; a missing manifest is a loud error
    //    NAMING the expected path (design apply-pass semantics).
    let dir = manifest::proposal_dir(change_id)?;
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(eyre::eyre!(
            "no proposal to apply for {change_id}: expected manifest at {}",
            manifest_path.display()
        ));
    }
    let manifest = manifest::load_manifest(&dir)?;
    let token = manifest::recompute_token(&dir)?;

    // 2. Token gate (the seam Phase 6's present gate + Phase 9's MCP confirm
    //    ride): a caller-supplied Token must match the manifest's CURRENT hash,
    //    so a proposal altered between present and apply is refused.
    //    AlreadyConfirmed (CLI TTY confirm / --yes) proceeds.
    if let Confirmation::Token(supplied) = &confirmation {
        if supplied != &token {
            return Err(eyre::eyre!(
                "confirm token mismatch for {change_id}: the proposal changed since it was presented (expected {token}); re-present and apply"
            ));
        }
    }

    // 3. Resolve the repos to apply: every repo the change state records as
    //    `Proposed`. A repo with no usable local path is a loud per-repo skip
    //    (warned), never silently mis-applied.
    let manager = StateManager::new()?;
    let state = manager.load(change_id)?.ok_or_else(|| {
        eyre::eyre!(
            "no change state for {change_id}; nothing to apply (already applied or cleaned up?)"
        )
    })?;

    let mut repos: Vec<Repo> = Vec::new();
    for repo_state in state.repositories.values() {
        if repo_state.status != RepoChangeStatus::Proposed {
            continue;
        }
        let Some(path) = repo_state.local_path.as_ref() else {
            warn!(
                "execute_apply: {} has no recorded local path; skipping",
                repo_state.repo_slug
            );
            continue;
        };
        match Repo::new(PathBuf::from(path)) {
            Ok(mut repo) => {
                // The recorded slug is authoritative (matches the manifest key
                // process_single_repo looks up); trust it over re-derivation.
                repo.slug = repo_state.repo_slug.clone();
                repos.push(repo);
            }
            Err(e) => warn!(
                "execute_apply: cannot open {} ({e}); skipping",
                repo_state.repo_slug
            ),
        }
    }
    if repos.is_empty() {
        return Err(eyre::eyre!(
            "no repositories in a Proposed state for {change_id}; nothing to apply"
        ));
    }
    debug!("execute_apply: applying {} proposed repo(s)", repos.len());

    // 4. Commit message: the caller's, else the recorded prompt.
    let msg = commit_message
        .map(str::to_string)
        .unwrap_or_else(|| manifest.prompt.clone());

    // 5. Ride the UNCHANGED create pipeline via the internal Change::Patchset.
    let change = Change::Patchset {
        proposal_dir: dir.clone(),
        manifest: Arc::new(manifest.clone()),
    };
    let results = execute_create(
        &repos,
        change_id,
        &[],
        &change,
        Some(&msg),
        pr,
        config,
        parallel_jobs,
        confirmation,
    )?;

    // 6. Reconcile state. `execute_create` saves a FRESH change state holding
    //    only the repos that COMMITTED (Committed/PrCreated). A repo that
    //    drifted/failed before committing is a normal partial-apply outcome and
    //    must STAY `Proposed` with its error recorded, or it is silently lost
    //    from state (and, worse, `gx undo` could then miss it). Re-mark those.
    let mut applied = 0usize;
    let mut drifted_or_failed = 0usize;
    let mut stragglers: BTreeMap<String, String> = BTreeMap::new(); // slug -> error
    for r in &results {
        let committed = matches!(r.action, CreateAction::Committed | CreateAction::PrCreated);
        if committed {
            applied += 1;
        } else {
            drifted_or_failed += 1;
            stragglers.insert(
                r.repo.slug.clone(),
                r.error
                    .clone()
                    .unwrap_or_else(|| "apply produced no commit".to_string()),
            );
        }
    }

    if !stragglers.is_empty() {
        // Held for the whole load-mutate-save so a concurrent op on this
        // change-id cannot interleave (same discipline as execute_create/undo).
        let _change_lock = crate::lock::ChangeLock::acquire(change_id).with_context(|| {
            format!("Failed to acquire change lock to record apply stragglers for {change_id}")
        })?;
        let mut fresh = manager.load(change_id)?.unwrap_or_else(|| {
            ChangeState::new(change_id.to_string(), Some(manifest.prompt.clone()))
        });
        for rp in &manifest.repos {
            if rp.outcome != ProposalOutcome::Proposed {
                continue;
            }
            let Some(err) = stragglers.get(&rp.slug) else {
                continue;
            };
            let files = rp.files.iter().map(|f| f.path.clone()).collect();
            let local_path = repo_local_path(&state, &rp.slug);
            fresh.mark_proposed(&rp.slug, rp.base_sha.clone(), files, local_path);
            if let Some(repo) = fresh.repositories.get_mut(&rp.slug) {
                repo.error = Some(err.clone());
            }
        }
        manager
            .save(&fresh)
            .context("Failed to save reconciled apply state")?;
    }

    info!("execute_apply: change_id={change_id} applied={applied} drifted_or_failed={drifted_or_failed}");
    Ok(ApplyReport {
        change_id: change_id.to_string(),
        token,
        results,
        applied,
        drifted_or_failed,
    })
}

/// The recorded local path for a slug in the pre-apply change state, so a
/// re-marked straggler keeps its checkout pointer.
fn repo_local_path(state: &ChangeState, slug: &str) -> Option<String> {
    state
        .repositories
        .get(slug)
        .and_then(|r| r.local_path.clone())
}

#[cfg(test)]
mod tests;
