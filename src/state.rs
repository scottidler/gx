//! Change state tracking for GX operations
//!
//! Tracks which repositories were modified, branches created, and PRs opened
//! for each change-id to enable cleanup and status monitoring.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// State of a change operation across repositories
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeState {
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

    /// Repositories affected by this change
    pub repositories: HashMap<String, RepoChangeState>,

    /// Overall status of the change
    pub status: ChangeStatus,
}

/// Status of an individual repository in a change
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoChangeState {
    /// Repository slug (e.g., "org/repo-name")
    pub repo_slug: String,

    /// Local path to the repository
    pub local_path: Option<String>,

    /// Branch name created for this change
    pub branch_name: String,

    /// Original branch before the change
    pub original_branch: Option<String>,

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
    /// Operation failed
    Failed,
    /// Local branch cleaned up
    CleanedUp,
}

impl ChangeState {
    /// Create a new change state
    pub fn new(change_id: String, description: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            change_id,
            description,
            created_at: now,
            updated_at: now,
            commit_message: None,
            repositories: HashMap::new(),
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
            pr_number: None,
            pr_url: None,
            status: RepoChangeStatus::BranchCreated,
            files_modified: Vec::new(),
            error: None,
        };
        self.repositories.insert(repo_slug, state);
        self.updated_at = Utc::now();
    }

    /// Update PR info for a repository
    /// Note: Currently only used in tests. Will be integrated when github::create_pr
    /// is updated to return PR info (number, URL).
    #[allow(dead_code)]
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
        }
    }

    /// Update overall status based on repository states
    fn update_overall_status(&mut self) {
        let total = self.repositories.len();
        if total == 0 {
            return;
        }

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

        if merged == total {
            self.status = ChangeStatus::FullyMerged;
        } else if merged > 0 {
            self.status = ChangeStatus::PartiallyMerged;
        } else if with_prs == total {
            self.status = ChangeStatus::PrsCreated;
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

    /// Save a change state to disk
    pub fn save(&self, state: &ChangeState) -> Result<()> {
        let file_path = self.state_dir.join(format!("{}.json", state.change_id));
        let json =
            serde_json::to_string_pretty(state).context("Failed to serialize change state")?;
        fs::write(&file_path, json).context("Failed to write change state file")?;
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
                    Ok(content) => {
                        if let Ok(state) = serde_json::from_str::<ChangeState>(&content) {
                            states.push(state);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to read state file {}: {}", path.display(), e);
                    }
                }
            }
        }

        // Sort by created_at descending (newest first)
        states.sort_by(|a, b| b.created_at.cmp(&a.created_at));
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

    /// Clean up old states (older than specified days)
    pub fn cleanup_old(&self, days: u64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        let states = self.list()?;
        let mut deleted = 0;

        for state in states {
            // Only clean up fully merged or abandoned changes
            if (state.status == ChangeStatus::FullyMerged
                || state.status == ChangeStatus::Abandoned)
                && state.updated_at < cutoff
            {
                self.delete(&state.change_id)?;
                deleted += 1;
            }
        }

        Ok(deleted)
    }
}

/// Get the state directory path
fn get_state_dir() -> Result<PathBuf> {
    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| eyre::eyre!("Could not determine home directory"))?;

    Ok(PathBuf::from(home_dir).join(".gx").join("changes"))
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
}
