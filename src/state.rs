//! Change state tracking for GX operations
//!
//! Tracks which repositories were modified, branches created, and PRs opened
//! for each change-id to enable cleanup and status monitoring.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Schema version stamped on every change state file written by this gx.
///
/// Bumped 1 -> 2 for Phase 4 (design doc
/// `2026-07-12-llm-propose-apply-and-mcp-server.md`): the new
/// `RepoChangeStatus::Proposed` variant. Bumped 2 -> 3 for Phase 7 (ringer
/// addendum #3): the new `ChangeStatus::Proposed` aggregate variant. Bumped
/// 3 -> 4 for the production-hardening doc Phase 4: the new
/// `RepoChangeStatus::Skipped { reason }` variant (`review approve` skips a
/// non-mergeable PR). Combined with `deny_unknown_fields`, an OLDER gx reading
/// a state file that carries a newer variant fails loudly on the unknown enum
/// variant (fail closed, correct) rather than silently mis-loading it.
const CHANGE_STATE_VERSION: u32 = 4;

/// Default `version` for a change state file that predates the field (serde
/// fills this in for version-less files written by an older gx), matching
/// `RecoveryState`'s scheme ([`crate::transaction::RecoveryState`]).
fn default_version() -> u32 {
    CHANGE_STATE_VERSION
}

/// State of a change operation across repositories
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeState {
    /// Schema version. A version-less file from an older gx (the field predates
    /// this schema) deserializes to `default_version()` = `CHANGE_STATE_VERSION`.
    #[serde(default = "default_version")]
    pub version: u32,

    /// Unique change identifier (e.g., "GX-2024-01-15-abc123")
    pub change_id: String,

    /// Human-readable description of the change
    pub description: Option<String>,

    /// When the change was initiated
    pub created_at: DateTime<Utc>,

    /// When the change was last updated
    pub updated_at: DateTime<Utc>,

    /// Commit message used for this change
    pub commit_message: Option<String>,

    /// Repositories affected by this change (BTreeMap for deterministic
    /// serialization order).
    pub repositories: BTreeMap<String, RepoChangeState>,

    /// Overall status of the change
    pub status: ChangeStatus,
}

/// Status of an individual repository in a change
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoChangeState {
    /// Repository slug (e.g., "org/repo-name")
    pub repo_slug: String,

    /// Local path to the repository
    pub local_path: Option<String>,

    /// Branch name created for this change
    pub branch_name: String,

    /// Original branch before the change
    pub original_branch: Option<String>,

    /// The pre-commit HEAD of the base branch (the safe point `ResetCommit`
    /// already captures), recorded at commit time so `gx undo` and audits can
    /// always state exactly what the safe point was. `None` on files written
    /// before this field existed.
    #[serde(default)]
    pub base_sha: Option<String>,

    /// PR number if one was created
    pub pr_number: Option<u64>,

    /// PR URL if one was created
    pub pr_url: Option<String>,

    /// Current status of this repo's change
    pub status: RepoChangeStatus,

    /// Files modified in this repository
    pub files_modified: Vec<String>,

    /// Error message if something failed
    pub error: Option<String>,
}

/// Overall change status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChangeStatus {
    /// Change is in progress
    InProgress,
    /// Every repository has a persisted proposal (`RepoChangeStatus::Proposed`)
    /// and none has been applied yet (ringer addendum #3, Phase 7): a bare
    /// `gx create ... llm --propose` campaign, truthfully distinct from
    /// `InProgress` so it doesn't read as an active/stuck run.
    Proposed,
    /// All PRs created successfully
    PrsCreated,
    /// Some PRs merged
    PartiallyMerged,
    /// All PRs merged
    FullyMerged,
    /// Change was abandoned/deleted
    Abandoned,
    /// Change failed
    Failed,
}

/// Status of a single repository's change
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RepoChangeStatus {
    /// A proposal has been persisted (Phase 4) but not yet applied: artifacts
    /// live under `proposals/<change-id>/`, no branch/commit/PR exists. Ordered
    /// before `BranchCreated`, which is the next state once `gx apply` runs.
    Proposed,
    /// Branch created, no PR yet
    BranchCreated,
    /// PR created and open
    PrOpen,
    /// PR is in draft state
    PrDraft,
    /// PR merged successfully
    PrMerged,
    /// PR was closed without merging
    PrClosed,
    /// A revert PR has been opened for a previously merged PR (`gx undo`
    /// Phase 6 [F4]): the merged work is reversed via a revert PR, never by
    /// touching the base branch. This is the terminal undo state for a merged row.
    RevertPrOpen,
    /// Operation failed
    Failed,
    /// Local branch cleaned up
    CleanedUp,
    /// `review approve` SKIPPED this PR because it was not proven-mergeable
    /// (production-hardening doc, Phase 4). The PR is still open on GitHub --
    /// this is neither merged nor an error, so it must be recorded distinctly
    /// or the `error == None` state update would mis-record it as merged. The
    /// `reason` (e.g. mergeability not yet computed, or a merge conflict) is
    /// carried so the operator knows whether re-running resolves it.
    Skipped { reason: String },
}

impl ChangeState {
    /// Create a new change state
    pub fn new(change_id: String, description: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            version: CHANGE_STATE_VERSION,
            change_id,
            description,
            created_at: now,
            updated_at: now,
            commit_message: None,
            repositories: BTreeMap::new(),
            status: ChangeStatus::InProgress,
        }
    }

    /// Add or update a repository in this change
    pub fn add_repository(&mut self, repo_slug: String, branch_name: String) {
        let state = RepoChangeState {
            repo_slug: repo_slug.clone(),
            local_path: None,
            branch_name,
            original_branch: None,
            base_sha: None,
            pr_number: None,
            pr_url: None,
            status: RepoChangeStatus::BranchCreated,
            files_modified: Vec::new(),
            error: None,
        };
        self.repositories.insert(repo_slug, state);
        self.updated_at = Utc::now();
    }

    /// Record a repository as having a persisted proposal (Phase 4). No branch,
    /// commit, or PR exists yet - the apply pass (Phase 5) advances it to
    /// `BranchCreated` and onward. `base_sha` is the pristine head the proposal
    /// was generated against; apply refuses if the repo has drifted past it.
    pub fn mark_proposed(
        &mut self,
        repo_slug: &str,
        base_sha: String,
        files_modified: Vec<String>,
        local_path: Option<String>,
    ) {
        let change_id = self.change_id.clone();
        self.add_repository(repo_slug.to_string(), change_id);
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::Proposed;
            repo.base_sha = Some(base_sha);
            repo.files_modified = files_modified;
            repo.local_path = local_path;
        }
        self.updated_at = Utc::now();
        self.update_overall_status();
    }

    /// Update PR info for a repository
    pub fn set_pr_info(&mut self, repo_slug: &str, pr_number: u64, pr_url: String, is_draft: bool) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.pr_number = Some(pr_number);
            repo.pr_url = Some(pr_url);
            repo.status = if is_draft {
                RepoChangeStatus::PrDraft
            } else {
                RepoChangeStatus::PrOpen
            };
            self.updated_at = Utc::now();
            self.update_overall_status();
        }
    }

    /// Mark a repository's PR as merged
    pub fn mark_merged(&mut self, repo_slug: &str) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::PrMerged;
            self.updated_at = Utc::now();
            self.update_overall_status();
        }
    }

    /// Mark a repository's PR as closed
    pub fn mark_closed(&mut self, repo_slug: &str) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::PrClosed;
            self.updated_at = Utc::now();
            self.update_overall_status();
        }
    }

    /// Mark a repository's PR as SKIPPED by `review approve` because it was not
    /// proven-mergeable (production-hardening doc, Phase 4). The PR stays open
    /// on GitHub; this records that gx deliberately did NOT merge it (and why),
    /// so the `error == None` outcome loop can't mis-record it as merged.
    pub fn mark_skipped(&mut self, repo_slug: &str, reason: String) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::Skipped { reason };
            self.updated_at = Utc::now();
            self.update_overall_status();
        }
    }

    /// Mark a repository's merged PR as reverted via an open revert PR (`gx undo`
    /// Phase 6 [F4]). The merged work is reversed by a revert PR, never by moving
    /// the base branch; this is the terminal undo state for a merged row.
    pub fn mark_revert_pr_open(&mut self, repo_slug: &str) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::RevertPrOpen;
            self.updated_at = Utc::now();
        }
    }

    /// Mark a repository as cleaned up
    pub fn mark_cleaned_up(&mut self, repo_slug: &str) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::CleanedUp;
            self.updated_at = Utc::now();
        }
    }

    /// Mark a repository as failed
    pub fn mark_failed(&mut self, repo_slug: &str, error: String) {
        if let Some(repo) = self.repositories.get_mut(repo_slug) {
            repo.status = RepoChangeStatus::Failed;
            repo.error = Some(error);
            self.updated_at = Utc::now();
            // F14: the aggregate must be able to reach `Failed`, or a campaign
            // where every repo failed sits at `InProgress` forever and never
            // ages out via cleanup.
            self.update_overall_status();
        }
    }

    /// Update overall status based on repository states. `Failed` is reachable
    /// (F14) when every repository failed; a mix of failed and successful
    /// repos still resolves via the merged/PR-created buckets below. `Proposed`
    /// (ringer addendum #3, Phase 7) is reachable only when EVERY repository is
    /// still a bare proposal - a mixed campaign (some applied, some still
    /// `Proposed` stragglers) falls through to whatever bucket its applied
    /// repos earn, matching the existing "mix resolves via the more-advanced
    /// bucket" pattern.
    fn update_overall_status(&mut self) {
        let total = self.repositories.len();
        if total == 0 {
            return;
        }

        let failed = self
            .repositories
            .values()
            .filter(|r| r.status == RepoChangeStatus::Failed)
            .count();

        let merged = self
            .repositories
            .values()
            .filter(|r| r.status == RepoChangeStatus::PrMerged)
            .count();

        let with_prs = self
            .repositories
            .values()
            .filter(|r| {
                r.status == RepoChangeStatus::PrOpen
                    || r.status == RepoChangeStatus::PrDraft
                    || r.status == RepoChangeStatus::PrMerged
                    || r.status == RepoChangeStatus::PrClosed
            })
            .count();

        let proposed = self
            .repositories
            .values()
            .filter(|r| r.status == RepoChangeStatus::Proposed)
            .count();

        if failed == total {
            self.status = ChangeStatus::Failed;
        } else if merged == total {
            self.status = ChangeStatus::FullyMerged;
        } else if merged > 0 {
            self.status = ChangeStatus::PartiallyMerged;
        } else if with_prs == total {
            self.status = ChangeStatus::PrsCreated;
        } else if proposed == total {
            self.status = ChangeStatus::Proposed;
        }
    }

    /// Get repositories that need cleanup (merged PRs with local branches)
    pub fn get_repos_needing_cleanup(&self) -> Vec<&RepoChangeState> {
        self.repositories
            .values()
            .filter(|r| {
                r.status == RepoChangeStatus::PrMerged || r.status == RepoChangeStatus::PrClosed
            })
            .filter(|r| r.status != RepoChangeStatus::CleanedUp)
            .collect()
    }

    /// Get open PRs
    pub fn get_open_prs(&self) -> Vec<&RepoChangeState> {
        self.repositories
            .values()
            .filter(|r| {
                r.status == RepoChangeStatus::PrOpen || r.status == RepoChangeStatus::PrDraft
            })
            .collect()
    }
}

/// State manager for loading/saving change states
pub struct StateManager {
    state_dir: PathBuf,
}

impl StateManager {
    /// Create a new state manager
    pub fn new() -> Result<Self> {
        let state_dir = get_state_dir()?;
        fs::create_dir_all(&state_dir).context("Failed to create state directory")?;
        Ok(Self { state_dir })
    }

    /// Create a state manager with a custom directory (for testing)
    #[cfg(test)]
    pub fn with_dir(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Save a change state to disk (atomic, F8): a torn write here would hide
    /// or corrupt the whole campaign's bookkeeping, not just one file.
    pub fn save(&self, state: &ChangeState) -> Result<()> {
        // Test-only fault injection (inert unless GX_TEST_FAIL_STATE_SAVE is set),
        // same "compiled in, inert by default" shape as the lock-delay
        // (`GX_TEST_LOCK_DELAY_MS`) and crash (`GX_CRASH_POINT`) hooks. Lets an
        // e2e deterministically fail the pushed safe-point save AFTER a real push
        // to exercise the F12 retain-recovery fail-closed path.
        if std::env::var_os("GX_TEST_FAIL_STATE_SAVE").is_some() {
            return Err(eyre::eyre!(
                "GX_TEST_FAIL_STATE_SAVE: simulated state save failure"
            ));
        }
        let file_path = self.state_dir.join(format!("{}.json", state.change_id));
        let json =
            serde_json::to_string_pretty(state).context("Failed to serialize change state")?;
        local::file::atomic_write(&file_path, json.as_bytes())
            .context("Failed to write change state file")?;
        debug!("Saved change state to {}", file_path.display());
        Ok(())
    }

    /// Load a change state from disk
    pub fn load(&self, change_id: &str) -> Result<Option<ChangeState>> {
        let file_path = self.state_dir.join(format!("{change_id}.json"));
        if !file_path.exists() {
            return Ok(None);
        }

        let json = fs::read_to_string(&file_path).context("Failed to read change state file")?;
        let state: ChangeState =
            serde_json::from_str(&json).context("Failed to parse change state file")?;

        // Numeric fail-closed guard (belt-and-suspenders alongside the semantic
        // `deny_unknown_fields` + unknown-enum-variant protections): a state file
        // written by a NEWER gx (higher schema version) may carry fields or
        // encodings this gx cannot interpret. Refuse loudly, naming BOTH versions,
        // instead of silently mis-loading or emitting a cryptic serde error.
        if state.version > CHANGE_STATE_VERSION {
            return Err(eyre::eyre!(
                "change state {change_id} was written by a newer gx (state schema v{}); \
                 this gx supports v{CHANGE_STATE_VERSION} - upgrade gx",
                state.version
            ));
        }
        Ok(Some(state))
    }

    /// List all change states
    pub fn list(&self) -> Result<Vec<ChangeState>> {
        let mut states = Vec::new();

        if !self.state_dir.exists() {
            return Ok(states);
        }

        for entry in fs::read_dir(&self.state_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str::<ChangeState>(&content) {
                        Ok(state) => states.push(state),
                        Err(e) => {
                            warn!("Skipping unparsable state file {}: {}", path.display(), e);
                        }
                    },
                    Err(e) => {
                        warn!("Failed to read state file {}: {}", path.display(), e);
                    }
                }
            }
        }

        // Sort by created_at descending (newest first)
        states.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(states)
    }

    /// Delete a change state
    pub fn delete(&self, change_id: &str) -> Result<()> {
        let file_path = self.state_dir.join(format!("{change_id}.json"));
        if file_path.exists() {
            fs::remove_file(&file_path).context("Failed to delete change state file")?;
            debug!("Deleted change state: {change_id}");
        }
        Ok(())
    }

    /// Clean up old states (older than specified days).
    ///
    /// Each `changes/<id>.json` deletion is taken UNDER that change's
    /// [`ChangeLock`](crate::lock::ChangeLock) (post-audit hardening): Phase 7
    /// requires every change-state read-modify-write to hold the change lock, and
    /// a delete is the ultimate mutation. Without it, `cleanup_old` could delete a
    /// file out from under a concurrent `undo`/`review sync`/create save. If the
    /// lock is held by a live process, that change is SKIPPED (a `warn!`), never
    /// deleted under contention.
    ///
    /// The pre-lock [`list`](Self::list) snapshot is used ONLY to pick candidates
    /// cheaply; it predates the per-change lock and can be stale (a racing
    /// `undo`/`review sync`/create save may revive a change between the listing
    /// and the delete). The authoritative decision is made in
    /// [`cleanup_if_stale`](Self::cleanup_if_stale), which reloads the change
    /// UNDER its lock and re-evaluates the predicate on the FRESH copy — closing
    /// the TOCTOU where a stale snapshot would delete a just-updated file.
    pub fn cleanup_old(&self, days: u64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let snapshot = self.list()?;
        let mut deleted = 0;

        for state in snapshot {
            // Cheap pre-filter on the (possibly stale) snapshot: only lock the
            // candidates it flags. The fresh-copy re-check happens under the lock.
            if !is_cleanup_candidate(&state, cutoff) {
                continue;
            }
            if self.cleanup_if_stale(&state.change_id, cutoff)? {
                deleted += 1;
            }
        }

        Ok(deleted)
    }

    /// Under the change lock, RELOAD the change file and delete it only if the
    /// FRESH copy still qualifies for cleanup ([`is_cleanup_candidate`]).
    ///
    /// The pre-lock snapshot in [`cleanup_old`](Self::cleanup_old) can be stale:
    /// a racing `undo`/`review sync`/create-save may have updated and released
    /// the change file AFTER the listing but BEFORE this lock, reviving it. This
    /// re-check on the reloaded copy is what makes lock + reload + re-check + delete
    /// one critical section, so a revived change is never deleted from a stale
    /// snapshot. A held lock, an already-gone file, or a revived change is
    /// SKIPPED (returns `Ok(false)`), never deleted.
    fn cleanup_if_stale(&self, change_id: &str, cutoff: DateTime<Utc>) -> Result<bool> {
        let _lock = match crate::lock::ChangeLock::acquire(change_id) {
            Ok(lock) => lock,
            Err(e) => {
                warn!("Skipping cleanup of change {change_id} - its change lock is held: {e}");
                return Ok(false);
            }
        };
        // Reload UNDER the lock and re-decide on the fresh copy (TOCTOU fix).
        match self.load(change_id)? {
            Some(fresh) => {
                if is_cleanup_candidate(&fresh, cutoff) {
                    self.delete(change_id)?;
                    debug!("Cleaned up aged-out change {change_id}");
                    // `_lock` drops here, releasing the change lock after the
                    // file is gone.
                    Ok(true)
                } else {
                    warn!(
                        "Skipping cleanup of change {change_id} - it was revived since the pre-lock listing (status {:?}, updated {})",
                        fresh.status, fresh.updated_at
                    );
                    Ok(false)
                }
            }
            None => {
                debug!("Change {change_id} already gone before cleanup; skipping");
                Ok(false)
            }
        }
    }
}

/// Whether a change qualifies for aging out: a terminal status (fully merged,
/// abandoned, or failed — F14 made `Failed` reachable, so a failed campaign
/// must age out too) whose last update is older than the cutoff.
fn is_cleanup_candidate(state: &ChangeState, cutoff: DateTime<Utc>) -> bool {
    matches!(
        state.status,
        ChangeStatus::FullyMerged | ChangeStatus::Abandoned | ChangeStatus::Failed
    ) && state.updated_at < cutoff
}

/// Get the state directory path (`$XDG_DATA_HOME/gx/changes`), migrating from
/// the legacy `~/.gx/changes` on first use ([A22], design Q4).
fn get_state_dir() -> Result<PathBuf> {
    let new_dir = local::config::xdg_data_dir()
        .ok_or_else(|| eyre::eyre!("Could not determine data dir (set HOME or XDG_DATA_HOME)"))?
        .join("gx")
        .join("changes");

    if !new_dir.exists() {
        migrate_legacy_state(&new_dir);
    }

    Ok(new_dir)
}

/// One-time migration: copy any legacy `~/.gx/changes/*.json` into the new XDG
/// location, then rename the whole `~/.gx` aside (not deleted) as a backup.
fn migrate_legacy_state(new_dir: &std::path::Path) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let legacy_gx = home.join(".gx");
    let legacy_changes = legacy_gx.join("changes");
    if !legacy_changes.exists() {
        return;
    }

    if let Err(e) = fs::create_dir_all(new_dir) {
        warn!("Migration: failed to create {}: {}", new_dir.display(), e);
        return;
    }

    let entries = match fs::read_dir(&legacy_changes) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "Migration: failed to read {}: {}",
                legacy_changes.display(),
                e
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(name) = path.file_name() {
                if let Err(e) = fs::copy(&path, new_dir.join(name)) {
                    warn!("Migration: failed to copy {}: {}", path.display(), e);
                }
            }
        }
    }

    // Rename the legacy dir aside as a one-time backup (not deleted).
    let stamp = Utc::now().format("%Y%m%d%H%M%S");
    let backup = home.join(format!(".gx.migrated-{stamp}"));
    match fs::rename(&legacy_gx, &backup) {
        Ok(()) => info!(
            "Migrated change state from {} to {}; legacy dir backed up at {}",
            legacy_changes.display(),
            new_dir.display(),
            backup.display()
        ),
        Err(e) => warn!(
            "Migrated change state to {} but failed to rename legacy {}: {}",
            new_dir.display(),
            legacy_gx.display(),
            e
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manager() -> (StateManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::with_dir(temp_dir.path().to_path_buf());
        (manager, temp_dir)
    }

    #[test]
    fn test_change_state_new() {
        let state = ChangeState::new("GX-2024-01-15".to_string(), Some("Test change".to_string()));
        assert_eq!(state.change_id, "GX-2024-01-15");
        assert_eq!(state.description, Some("Test change".to_string()));
        assert_eq!(state.status, ChangeStatus::InProgress);
        assert!(state.repositories.is_empty());
    }

    #[test]
    fn test_add_repository() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());

        assert_eq!(state.repositories.len(), 1);
        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(repo.branch_name, "GX-test");
        assert_eq!(repo.status, RepoChangeStatus::BranchCreated);
    }

    #[test]
    fn test_set_pr_info() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.set_pr_info(
            "org/repo",
            123,
            "https://github.com/org/repo/pull/123".to_string(),
            false,
        );

        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(repo.pr_number, Some(123));
        assert_eq!(repo.status, RepoChangeStatus::PrOpen);
    }

    #[test]
    fn test_set_pr_info_draft() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.set_pr_info(
            "org/repo",
            123,
            "https://github.com/org/repo/pull/123".to_string(),
            true,
        );

        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(repo.status, RepoChangeStatus::PrDraft);
    }

    #[test]
    fn test_mark_merged() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.set_pr_info(
            "org/repo",
            123,
            "https://github.com/org/repo/pull/123".to_string(),
            false,
        );
        state.mark_merged("org/repo");

        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(repo.status, RepoChangeStatus::PrMerged);
        assert_eq!(state.status, ChangeStatus::FullyMerged);
    }

    #[test]
    fn test_mark_proposed_sets_truthful_aggregate_status() {
        // Ringer addendum #3: a bare (unapplied) proposal must report a
        // truthful aggregate `ChangeStatus::Proposed`, not the stale default
        // `InProgress` a never-undone proposal was stuck at before this fix
        // (`mark_proposed` never called `update_overall_status`).
        let mut state = ChangeState::new("GX-proposed".to_string(), None);
        state.mark_proposed(
            "org/repo1",
            "deadbeef".to_string(),
            vec!["README.md".to_string()],
            Some("/tmp/org/repo1".to_string()),
        );
        assert_eq!(
            state.repositories.get("org/repo1").unwrap().status,
            RepoChangeStatus::Proposed
        );
        assert_eq!(
            state.status,
            ChangeStatus::Proposed,
            "an all-Proposed campaign must resolve to ChangeStatus::Proposed, not InProgress"
        );

        // A mixed campaign (one repo proposed, one already merged) must NOT
        // wrongly report `Proposed` for the whole change - it resolves via the
        // existing partial bucket. This is the bite: without the `proposed ==
        // total` guard (as opposed to `proposed > 0`), this would misfire.
        state.mark_proposed(
            "org/repo2",
            "cafebabe".to_string(),
            vec![],
            Some("/tmp/org/repo2".to_string()),
        );
        state.set_pr_info(
            "org/repo2",
            1,
            "https://github.com/org/repo2/pull/1".to_string(),
            false,
        );
        state.mark_merged("org/repo2");
        assert_eq!(
            state.status,
            ChangeStatus::PartiallyMerged,
            "a mix of Proposed + merged must not misreport Proposed"
        );
    }

    #[test]
    fn test_mark_skipped_records_distinct_status_not_merged() {
        // Production-hardening Phase 4: a PR skipped by `review approve` for
        // non-mergeability must be recorded as `Skipped { reason }` - NOT
        // merged. Bite: before the variant, the `error == None` outcome loop
        // would `mark_merged` a skipped PR; here it stays open-and-skipped.
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.set_pr_info(
            "org/repo",
            7,
            "https://github.com/org/repo/pull/7".to_string(),
            false,
        );
        state.mark_skipped("org/repo", "mergeability not yet computed".to_string());

        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(
            repo.status,
            RepoChangeStatus::Skipped {
                reason: "mergeability not yet computed".to_string()
            }
        );
        assert_ne!(
            repo.status,
            RepoChangeStatus::PrMerged,
            "a skipped PR must never be recorded as merged"
        );
    }

    #[test]
    fn test_skipped_status_roundtrips_through_serde() {
        // The struct variant serializes/deserializes intact (schema v4).
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.mark_skipped("org/repo", "merge conflict with base branch".to_string());

        let json = serde_json::to_string(&state).unwrap();
        let back: ChangeState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.repositories.get("org/repo").unwrap().status,
            RepoChangeStatus::Skipped {
                reason: "merge conflict with base branch".to_string()
            }
        );
    }

    #[test]
    fn test_update_overall_status_partial() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo1".to_string(), "GX-test".to_string());
        state.add_repository("org/repo2".to_string(), "GX-test".to_string());

        state.set_pr_info(
            "org/repo1",
            1,
            "https://github.com/org/repo1/pull/1".to_string(),
            false,
        );
        state.set_pr_info(
            "org/repo2",
            2,
            "https://github.com/org/repo2/pull/2".to_string(),
            false,
        );

        assert_eq!(state.status, ChangeStatus::PrsCreated);

        state.mark_merged("org/repo1");
        assert_eq!(state.status, ChangeStatus::PartiallyMerged);

        state.mark_merged("org/repo2");
        assert_eq!(state.status, ChangeStatus::FullyMerged);
    }

    #[test]
    fn test_get_repos_needing_cleanup() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo1".to_string(), "GX-test".to_string());
        state.add_repository("org/repo2".to_string(), "GX-test".to_string());
        state.add_repository("org/repo3".to_string(), "GX-test".to_string());

        // repo1: merged
        state.repositories.get_mut("org/repo1").unwrap().status = RepoChangeStatus::PrMerged;
        // repo2: still open
        state.repositories.get_mut("org/repo2").unwrap().status = RepoChangeStatus::PrOpen;
        // repo3: closed
        state.repositories.get_mut("org/repo3").unwrap().status = RepoChangeStatus::PrClosed;

        let needing_cleanup = state.get_repos_needing_cleanup();
        assert_eq!(needing_cleanup.len(), 2);
    }

    #[test]
    fn test_get_open_prs() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo1".to_string(), "GX-test".to_string());
        state.add_repository("org/repo2".to_string(), "GX-test".to_string());
        state.add_repository("org/repo3".to_string(), "GX-test".to_string());

        state.repositories.get_mut("org/repo1").unwrap().status = RepoChangeStatus::PrOpen;
        state.repositories.get_mut("org/repo2").unwrap().status = RepoChangeStatus::PrDraft;
        state.repositories.get_mut("org/repo3").unwrap().status = RepoChangeStatus::PrMerged;

        let open_prs = state.get_open_prs();
        assert_eq!(open_prs.len(), 2);
    }

    #[test]
    fn test_mark_failed() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.mark_failed("org/repo", "Network error".to_string());

        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(repo.status, RepoChangeStatus::Failed);
        assert_eq!(repo.error, Some("Network error".to_string()));
    }

    #[test]
    fn test_save_and_load() {
        let (manager, _temp) = create_test_manager();

        let mut state = ChangeState::new("test-change".to_string(), Some("Test".to_string()));
        state.add_repository("org/repo".to_string(), "GX-test".to_string());

        manager.save(&state).unwrap();

        let loaded = manager.load("test-change").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.change_id, "test-change");
        assert_eq!(loaded.repositories.len(), 1);
    }

    #[test]
    fn test_load_nonexistent() {
        let (manager, _temp) = create_test_manager();
        let loaded = manager.load("nonexistent").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_load_refuses_newer_schema_version_naming_both_versions() {
        // Numeric fail-closed guard (audit fix #4): a state file stamped with a
        // schema version HIGHER than this gx supports must be refused loudly,
        // naming BOTH the file's version and the version this gx supports -
        // never silently mis-loaded. Bite check: remove the `version >
        // CHANGE_STATE_VERSION` guard in `load` and this test fails (the newer
        // file loads as if it were current).
        let (manager, _temp) = create_test_manager();
        let mut state = ChangeState::new("GX-from-the-future".to_string(), None);
        state.version = 99; // a version no build of this gx has ever emitted
        manager.save(&state).unwrap();

        let err = manager
            .load("GX-from-the-future")
            .expect_err("a newer-schema state file must fail to load")
            .to_string();
        assert!(
            err.contains("v99") && err.contains(&format!("v{CHANGE_STATE_VERSION}")),
            "the refusal must name BOTH versions (file v99, supported v{CHANGE_STATE_VERSION}): {err}"
        );
        assert!(
            err.contains("newer gx"),
            "the refusal must point at a newer gx: {err}"
        );
    }

    #[test]
    fn test_load_accepts_current_schema_version() {
        // The guard is strictly `>`: the current version (and anything older,
        // including version-less files that default to current) must still load.
        let (manager, _temp) = create_test_manager();
        let state = ChangeState::new("GX-current".to_string(), None);
        assert_eq!(state.version, CHANGE_STATE_VERSION);
        manager.save(&state).unwrap();
        assert!(
            manager.load("GX-current").unwrap().is_some(),
            "a state at the current schema version must load"
        );
    }

    #[test]
    fn test_list_states() {
        let (manager, _temp) = create_test_manager();

        for i in 0..3 {
            let state = ChangeState::new(format!("change-{i}"), None);
            manager.save(&state).unwrap();
        }

        let states = manager.list().unwrap();
        assert_eq!(states.len(), 3);
    }

    #[test]
    fn test_list_empty_dir() {
        let (manager, _temp) = create_test_manager();
        let states = manager.list().unwrap();
        assert!(states.is_empty());
    }

    #[test]
    fn test_delete_state() {
        let (manager, _temp) = create_test_manager();

        let state = ChangeState::new("to-delete".to_string(), None);
        manager.save(&state).unwrap();

        assert!(manager.load("to-delete").unwrap().is_some());

        manager.delete("to-delete").unwrap();
        assert!(manager.load("to-delete").unwrap().is_none());
    }

    #[test]
    fn test_delete_nonexistent() {
        let (manager, _temp) = create_test_manager();
        // Should not error
        manager.delete("nonexistent").unwrap();
    }

    #[test]
    fn test_mark_cleaned_up() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.repositories.get_mut("org/repo").unwrap().status = RepoChangeStatus::PrMerged;

        state.mark_cleaned_up("org/repo");

        let repo = state.repositories.get("org/repo").unwrap();
        assert_eq!(repo.status, RepoChangeStatus::CleanedUp);
    }

    #[test]
    fn test_deny_unknown_fields() {
        // A state file with an unexpected field must fail to parse, not silently
        // drop data ([A22]).
        let json = r#"{
            "change_id": "x",
            "description": null,
            "created_at": "2026-06-11T00:00:00Z",
            "updated_at": "2026-06-11T00:00:00Z",
            "commit_message": null,
            "repositories": {},
            "status": "InProgress",
            "bogus_field": 1
        }"#;
        let parsed: Result<ChangeState, _> = serde_json::from_str(json);
        assert!(parsed.is_err(), "unknown field must be rejected");
    }

    #[test]
    fn test_repositories_serialize_in_sorted_order() {
        // BTreeMap gives deterministic, sorted key order in the JSON ([A22]).
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/zebra".to_string(), "GX-test".to_string());
        state.add_repository("org/alpha".to_string(), "GX-test".to_string());
        state.add_repository("org/mango".to_string(), "GX-test".to_string());

        let json = serde_json::to_string(&state).unwrap();
        let alpha = json.find("org/alpha").unwrap();
        let mango = json.find("org/mango").unwrap();
        let zebra = json.find("org/zebra").unwrap();
        assert!(alpha < mango && mango < zebra, "keys must serialize sorted");
    }

    #[test]
    fn test_original_branch_is_recorded() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state
            .repositories
            .get_mut("org/repo")
            .unwrap()
            .original_branch = Some("main".to_string());

        let json = serde_json::to_string(&state).unwrap();
        let back: ChangeState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.repositories.get("org/repo").unwrap().original_branch,
            Some("main".to_string())
        );
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut state = ChangeState::new("test".to_string(), Some("Description".to_string()));
        state.commit_message = Some("Test commit".to_string());
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.set_pr_info(
            "org/repo",
            42,
            "https://github.com/org/repo/pull/42".to_string(),
            false,
        );

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: ChangeState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.change_id, state.change_id);
        assert_eq!(deserialized.description, state.description);
        assert_eq!(deserialized.commit_message, state.commit_message);
        assert_eq!(deserialized.repositories.len(), state.repositories.len());
    }

    #[test]
    fn test_version_defaults_for_versionless_file() {
        // Phase 4 [F11]: a change state file written before `version` existed
        // must still load, defaulting to 1, matching RecoveryState's scheme.
        let json = r#"{
            "change_id": "x",
            "description": null,
            "created_at": "2026-06-11T00:00:00Z",
            "updated_at": "2026-06-11T00:00:00Z",
            "commit_message": null,
            "repositories": {},
            "status": "InProgress"
        }"#;
        let state: ChangeState = serde_json::from_str(json).expect("version-less file must load");
        assert_eq!(state.version, CHANGE_STATE_VERSION);
    }

    #[test]
    fn test_new_change_state_stamps_current_version() {
        let state = ChangeState::new("test".to_string(), None);
        assert_eq!(state.version, CHANGE_STATE_VERSION);
    }

    #[test]
    fn test_base_sha_defaults_for_repo_state_without_it() {
        // Phase 4 [F11]: a repo entry written before `base_sha` existed must
        // still load, defaulting to `None`.
        let json = r#"{
            "repo_slug": "org/repo",
            "local_path": null,
            "branch_name": "GX-test",
            "original_branch": null,
            "pr_number": null,
            "pr_url": null,
            "status": "BranchCreated",
            "files_modified": [],
            "error": null
        }"#;
        let repo: RepoChangeState =
            serde_json::from_str(json).expect("base_sha-less repo state must load");
        assert_eq!(repo.base_sha, None);
    }

    #[test]
    fn test_base_sha_roundtrips() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());
        state.repositories.get_mut("org/repo").unwrap().base_sha = Some("deadbeefcafe".to_string());

        let json = serde_json::to_string(&state).unwrap();
        let back: ChangeState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.repositories.get("org/repo").unwrap().base_sha,
            Some("deadbeefcafe".to_string())
        );
    }

    #[test]
    fn test_mark_failed_updates_aggregate_to_failed_when_all_repos_fail() {
        // F14: `Failed` was previously unreachable at the aggregate level; a
        // campaign where every repo failed must resolve to `ChangeStatus::Failed`
        // so it can age out via cleanup instead of sitting at `InProgress` forever.
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo1".to_string(), "GX-test".to_string());
        state.add_repository("org/repo2".to_string(), "GX-test".to_string());

        state.mark_failed("org/repo1", "boom".to_string());
        assert_eq!(
            state.status,
            ChangeStatus::InProgress,
            "one of two repos failed; aggregate should not yet be Failed"
        );

        state.mark_failed("org/repo2", "boom again".to_string());
        assert_eq!(
            state.status,
            ChangeStatus::Failed,
            "every repo failed; aggregate must reach Failed"
        );
    }

    #[test]
    fn test_mark_closed_updates_aggregate() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo1".to_string(), "GX-test".to_string());
        state.add_repository("org/repo2".to_string(), "GX-test".to_string());
        state.set_pr_info(
            "org/repo1",
            1,
            "https://github.com/org/repo1/pull/1".to_string(),
            false,
        );
        state.set_pr_info(
            "org/repo2",
            2,
            "https://github.com/org/repo2/pull/2".to_string(),
            false,
        );

        state.mark_closed("org/repo1");
        state.mark_closed("org/repo2");

        // Both closed, none merged: PrsCreated bucket (with_prs counts Closed
        // too), never Failed - closed is a distinct outcome from failed.
        assert_eq!(state.status, ChangeStatus::PrsCreated);
    }

    /// Run `f` with `XDG_DATA_HOME` pointed at a fresh temp dir (serialized by
    /// the shared `ENV_LOCK`). Needed for any test whose code path acquires a
    /// `ChangeLock`, which resolves its lock dir from `xdg_data_dir()`; without
    /// this it would write lock files into the real `$HOME/.local/share`.
    fn with_xdg_data_home<F: FnOnce(&std::path::Path)>(f: F) {
        let guard = local::test_utils::env_lock();
        let prior = std::env::var("XDG_DATA_HOME").ok();
        let tmp = TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };
        f(tmp.path());
        match prior {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        drop(guard);
    }

    #[test]
    fn test_cleanup_old_includes_failed_states() {
        // F14 bite-proof: before this phase, `Failed` was unreachable so a
        // failed campaign never satisfied cleanup_old's filter and sat around
        // forever. Now it must age out like FullyMerged/Abandoned. (XDG-isolated
        // because cleanup_old now takes each change's ChangeLock.)
        with_xdg_data_home(|_| {
            let (manager, _temp) = create_test_manager();

            let mut state = ChangeState::new("old-failed".to_string(), None);
            state.add_repository("org/repo".to_string(), "GX-test".to_string());
            state.mark_failed("org/repo", "boom".to_string());
            assert_eq!(state.status, ChangeStatus::Failed);
            state.updated_at = Utc::now() - chrono::Duration::days(30);
            manager.save(&state).unwrap();

            let deleted = manager.cleanup_old(7).unwrap();
            assert_eq!(deleted, 1);
            assert!(manager.load("old-failed").unwrap().is_none());
        });
    }

    #[test]
    fn test_cleanup_old_skips_locked_change() {
        // FIX 3 (post-audit hardening): cleanup_old must take each change's
        // ChangeLock before deleting. A change whose lock is HELD by a live
        // process (simulated here by holding the guard in-test, so the lock file
        // names our own alive pid) is SKIPPED, not deleted; once released a
        // re-run converges and deletes it.
        //
        // Break-the-code proof: reverting cleanup_old to an unconditional
        // `self.delete(...)` makes the first cleanup return 1 and remove the
        // file, so the `deleted == 0` / `path.exists()` assertions fail.
        with_xdg_data_home(|data_home| {
            // Use StateManager::new() so the state dir AND the lock dir both live
            // under the isolated XDG_DATA_HOME.
            let manager = StateManager::new().unwrap();

            let mut state = ChangeState::new("GX-locked".to_string(), None);
            state.status = ChangeStatus::Abandoned;
            state.updated_at = Utc::now() - chrono::Duration::days(100);
            manager.save(&state).unwrap();

            let path = data_home.join("gx").join("changes").join("GX-locked.json");
            assert!(path.exists(), "state file must exist before cleanup");

            // Hold the change lock, as another live process's RMW would.
            let held = crate::lock::ChangeLock::acquire("GX-locked").unwrap();

            let deleted = manager.cleanup_old(30).unwrap();
            assert_eq!(deleted, 0, "a locked change must NOT be deleted");
            assert!(path.exists(), "the locked change file must survive cleanup");

            // Release the lock; a second cleanup converges and removes it.
            // Phase 5 flock-fix: bounded-poll the reacquire-after-drop (see
            // `lock::tests::test_lock_reacquirable_after_holder_drops` for why:
            // under `otto ci`'s full parallel test load, a `close`'s flock
            // release is occasionally not yet visible to an
            // immediately-following `open`+`try_lock` -- timing/harness, not a
            // production race). The logical assertion (once unlocked, the
            // aged-out change converges to deleted) is unchanged.
            drop(held);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut deleted2 = 0;
            while std::time::Instant::now() < deadline {
                deleted2 = manager.cleanup_old(30).unwrap();
                if deleted2 == 1 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                deleted2, 1,
                "once unlocked, the aged-out change must converge to deleted within the window"
            );
            assert!(!path.exists(), "the change file is deleted once unlocked");
        });
    }

    #[test]
    fn test_cleanup_old_skips_revived_change_on_reload() {
        // FIX A (second post-audit hardening): cleanup_old decides candidacy from
        // a pre-lock `list()` snapshot, but the authoritative delete must re-check
        // the FRESH copy UNDER the change lock. This models the TOCTOU race: the
        // snapshot flagged a change as aged-out (terminal + old), but a racing
        // `undo`/`review sync`/create-save revived it to a NON-terminal status
        // before cleanup acquired the lock. Reloading under the lock catches that
        // and must NOT delete the just-updated file.
        //
        // Break-the-code proof: reverting `cleanup_if_stale` to lock + delete
        // (dropping the reload + `is_cleanup_candidate` re-check on the fresh
        // copy) deletes the revived change, so the "must survive" assertion fails.
        with_xdg_data_home(|data_home| {
            let manager = StateManager::new().unwrap();
            let cutoff = Utc::now() - chrono::Duration::days(7);

            // The on-disk file as it looks AFTER the race revived it: a fresh,
            // non-terminal status (what the pre-lock snapshot did NOT see).
            let mut revived = ChangeState::new("GX-revived".to_string(), None);
            revived.add_repository("org/repo".to_string(), "GX-revived".to_string());
            revived.status = ChangeStatus::InProgress;
            revived.updated_at = Utc::now();
            manager.save(&revived).unwrap();

            let path = data_home.join("gx").join("changes").join("GX-revived.json");
            assert!(path.exists(), "state file must exist before cleanup");

            // cleanup_old flagged this change from a STALE snapshot; the fresh
            // reload under the lock must veto the delete.
            let deleted = manager.cleanup_if_stale("GX-revived", cutoff).unwrap();
            assert!(
                !deleted,
                "a revived (non-terminal) change must NOT be deleted"
            );
            assert!(
                path.exists(),
                "the revived change file must survive cleanup"
            );

            // Positive control: a genuinely aged-out terminal change IS deleted by
            // the same reload-and-recheck path.
            let mut aged = ChangeState::new("GX-aged".to_string(), None);
            aged.status = ChangeStatus::Abandoned;
            aged.updated_at = Utc::now() - chrono::Duration::days(30);
            manager.save(&aged).unwrap();
            let aged_path = data_home.join("gx").join("changes").join("GX-aged.json");
            assert!(aged_path.exists());

            let deleted_aged = manager.cleanup_if_stale("GX-aged", cutoff).unwrap();
            assert!(deleted_aged, "an aged-out terminal change must be deleted");
            assert!(!aged_path.exists(), "the aged-out change file is removed");
        });
    }
}
