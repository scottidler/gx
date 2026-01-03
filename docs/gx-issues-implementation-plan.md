# GX Issues Implementation Plan

Based on the January 2025 evaluation, this document provides a detailed implementation plan to resolve all identified issues and bring `gx` to production-ready status.

---

## Priority Matrix

| Issue | Severity | Priority |
|-------|----------|----------|
| PR JSON Parsing Stubbed | **Critical** | 🔴 P0 |
| Change State Tracking | High | 🟠 P1 |
| Local Branch Cleanup | High | 🟠 P1 |
| Emoji Width Calculation | Low | 🟢 P2 |
| Retry Logic for Network | Medium | 🟡 P2 |
| Dry-Run Flag | Medium | 🟡 P2 |

---

## Issue 1: PR Listing JSON Parsing (P0 - Critical)

### Problem

```rust:226:236:src/github.rs
/// Parse JSON output from gh pr list
fn parse_pr_list_json(json_output: &str) -> Result<Vec<PrInfo>> {
    // For now, we'll use a simple JSON parsing approach
    // In a production system, you'd want to use serde_json
    let prs = Vec::new();

    // This is a simplified parser - in reality you'd use serde_json
    // For now, just return empty list to avoid complex JSON parsing
    debug!("PR list JSON: {json_output}");

    Ok(prs)
}
```

**Impact**: `gx review ls`, `gx review clone`, `gx review approve`, `gx review delete` all return empty results.

### Solution

`serde_json` is already in `Cargo.toml`. Implement proper deserialization.

### Implementation

```rust
// src/github.rs

use serde::Deserialize;

/// Raw PR data from GitHub CLI JSON output
#[derive(Debug, Deserialize)]
struct GhPrListItem {
    number: u64,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    author: GhAuthor,
    state: String,
    url: String,
    repository: GhRepository,
}

#[derive(Debug, Deserialize)]
struct GhAuthor {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

/// Parse JSON output from gh pr list
fn parse_pr_list_json(json_output: &str) -> Result<Vec<PrInfo>> {
    if json_output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let gh_prs: Vec<GhPrListItem> = serde_json::from_str(json_output)
        .context("Failed to parse PR list JSON")?;

    let prs = gh_prs
        .into_iter()
        .map(|gh_pr| PrInfo {
            repo_slug: gh_pr.repository.name_with_owner,
            number: gh_pr.number,
            title: gh_pr.title,
            branch: gh_pr.head_ref_name,
            author: gh_pr.author.login,
            state: match gh_pr.state.to_lowercase().as_str() {
                "open" => PrState::Open,
                _ => PrState::Closed,
            },
            url: gh_pr.url,
        })
        .collect();

    Ok(prs)
}
```

### Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pr_list_json_empty() {
        let result = parse_pr_list_json("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_pr_list_json_empty_array() {
        let result = parse_pr_list_json("[]").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_pr_list_json_single_pr() {
        let json = r#"[{
            "number": 123,
            "title": "GX-2024-01-15: Update configs",
            "headRefName": "GX-2024-01-15",
            "author": {"login": "testuser"},
            "state": "OPEN",
            "url": "https://github.com/org/repo/pull/123",
            "repository": {"nameWithOwner": "org/repo"}
        }]"#;

        let result = parse_pr_list_json(json).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].number, 123);
        assert_eq!(result[0].branch, "GX-2024-01-15");
        assert_eq!(result[0].repo_slug, "org/repo");
        assert_eq!(result[0].state, PrState::Open);
    }

    #[test]
    fn test_parse_pr_list_json_multiple_prs() {
        let json = r#"[
            {"number": 1, "title": "PR 1", "headRefName": "branch1", "author": {"login": "user1"}, "state": "OPEN", "url": "url1", "repository": {"nameWithOwner": "org/repo1"}},
            {"number": 2, "title": "PR 2", "headRefName": "branch2", "author": {"login": "user2"}, "state": "CLOSED", "url": "url2", "repository": {"nameWithOwner": "org/repo2"}}
        ]"#;

        let result = parse_pr_list_json(json).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].state, PrState::Open);
        assert_eq!(result[1].state, PrState::Closed);
    }
}
```

### Files to Modify

- `src/github.rs` - Replace stubbed function with real implementation

---

## Issue 2: Change State Tracking (P1)

### Problem

No tracking of which branches/PRs were created by GX operations, making cleanup difficult.

### Solution

Create a state management module that tracks changes at `~/.gx/changes/{change-id}.json`.

### Implementation

#### New File: `src/state.rs`

```rust
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

    /// Update overall status based on repository states
    fn update_overall_status(&mut self) {
        let total = self.repositories.len();
        let merged = self.repositories.values()
            .filter(|r| r.status == RepoChangeStatus::PrMerged)
            .count();

        if merged == total && total > 0 {
            self.status = ChangeStatus::FullyMerged;
        } else if merged > 0 {
            self.status = ChangeStatus::PartiallyMerged;
        }
    }

    /// Get repositories that need cleanup (merged PRs with local branches)
    pub fn get_repos_needing_cleanup(&self) -> Vec<&RepoChangeState> {
        self.repositories
            .values()
            .filter(|r| {
                r.status == RepoChangeStatus::PrMerged
                    || r.status == RepoChangeStatus::PrClosed
            })
            .filter(|r| r.status != RepoChangeStatus::CleanedUp)
            .collect()
    }

    /// Get open PRs
    pub fn get_open_prs(&self) -> Vec<&RepoChangeState> {
        self.repositories
            .values()
            .filter(|r| {
                r.status == RepoChangeStatus::PrOpen
                    || r.status == RepoChangeStatus::PrDraft
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
        fs::create_dir_all(&state_dir)
            .context("Failed to create state directory")?;
        Ok(Self { state_dir })
    }

    /// Save a change state to disk
    pub fn save(&self, state: &ChangeState) -> Result<()> {
        let file_path = self.state_dir.join(format!("{}.json", state.change_id));
        let json = serde_json::to_string_pretty(state)
            .context("Failed to serialize change state")?;
        fs::write(&file_path, json)
            .context("Failed to write change state file")?;
        debug!("Saved change state to {}", file_path.display());
        Ok(())
    }

    /// Load a change state from disk
    pub fn load(&self, change_id: &str) -> Result<Option<ChangeState>> {
        let file_path = self.state_dir.join(format!("{}.json", change_id));
        if !file_path.exists() {
            return Ok(None);
        }

        let json = fs::read_to_string(&file_path)
            .context("Failed to read change state file")?;
        let state: ChangeState = serde_json::from_str(&json)
            .context("Failed to parse change state file")?;
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
        let file_path = self.state_dir.join(format!("{}.json", change_id));
        if file_path.exists() {
            fs::remove_file(&file_path)
                .context("Failed to delete change state file")?;
            debug!("Deleted change state: {}", change_id);
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

impl Default for StateManager {
    fn default() -> Self {
        Self::new().expect("Failed to create state manager")
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
        let manager = StateManager {
            state_dir: temp_dir.path().to_path_buf(),
        };
        (manager, temp_dir)
    }

    #[test]
    fn test_change_state_new() {
        let state = ChangeState::new(
            "GX-2024-01-15".to_string(),
            Some("Test change".to_string()),
        );
        assert_eq!(state.change_id, "GX-2024-01-15");
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
    fn test_save_and_load() {
        let (manager, _temp) = create_test_manager();

        let mut state = ChangeState::new("test-change".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-test".to_string());

        manager.save(&state).unwrap();

        let loaded = manager.load("test-change").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.change_id, "test-change");
        assert_eq!(loaded.repositories.len(), 1);
    }

    #[test]
    fn test_list_states() {
        let (manager, _temp) = create_test_manager();

        for i in 0..3 {
            let state = ChangeState::new(format!("change-{}", i), None);
            manager.save(&state).unwrap();
        }

        let states = manager.list().unwrap();
        assert_eq!(states.len(), 3);
    }

    #[test]
    fn test_update_overall_status() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("org/repo1".to_string(), "GX-test".to_string());
        state.add_repository("org/repo2".to_string(), "GX-test".to_string());

        assert_eq!(state.status, ChangeStatus::InProgress);

        state.mark_merged("org/repo1");
        assert_eq!(state.status, ChangeStatus::PartiallyMerged);

        state.mark_merged("org/repo2");
        assert_eq!(state.status, ChangeStatus::FullyMerged);
    }
}
```

### Integration with Create Command

Modify `src/create.rs` to use the state manager:

```rust
// At start of create operation
let state_manager = StateManager::new()?;
let mut change_state = ChangeState::new(change_id.clone(), Some(commit_message.clone()));

// After creating branch in each repo
change_state.add_repository(repo.slug.clone(), branch_name.clone());

// After creating PR
change_state.set_pr_info(&repo.slug, pr_number, pr_url, is_draft);

// Save state after each significant operation
state_manager.save(&change_state)?;
```

### Files to Modify/Create

- **New**: `src/state.rs` - State management module
- **Modify**: `src/lib.rs` - Add `pub mod state;`
- **Modify**: `src/create.rs` - Integrate state tracking
- **Modify**: `src/review.rs` - Use state for PR operations

---

## Issue 3: Local Branch Cleanup Command (P1)

### Problem

When PRs are merged, local `GX-*` branches remain, cluttering the workspace.

### Solution

Add `gx cleanup <change-id>` command that:
1. Reads change state
2. Identifies merged PRs
3. Deletes local branches
4. Optionally deletes remote branches if not auto-deleted

### Implementation

#### Add to `src/cli.rs`

```rust
#[derive(Debug, Subcommand)]
pub enum Commands {
    // ... existing commands ...

    /// Clean up branches after PR merge
    #[command(after_help = "CLEANUP LEGEND:
  🧹  Local branch deleted     🌐  Remote branch deleted
  ⏭️  Already cleaned         ⚠️  Still has open PR
  ❌  Cleanup failed           📊  Summary stats

EXAMPLES:
  gx cleanup GX-2024-01-15           # Clean up specific change
  gx cleanup --all                    # Clean up all merged changes
  gx cleanup --list                   # List changes needing cleanup")]
    Cleanup {
        /// Change ID to clean up (optional if --all or --list)
        #[arg(value_name = "CHANGE_ID")]
        change_id: Option<String>,

        /// Clean up all merged changes
        #[arg(long, conflicts_with = "change_id")]
        all: bool,

        /// List changes that can be cleaned up
        #[arg(long, conflicts_with = "change_id", conflicts_with = "all")]
        list: bool,

        /// Also delete remote branches (if not auto-deleted)
        #[arg(long)]
        include_remote: bool,

        /// Force cleanup even if PR status is unknown
        #[arg(long)]
        force: bool,
    },
}
```

#### New File: `src/cleanup.rs`

```rust
//! Branch cleanup after PR merge

use crate::cli::Cli;
use crate::config::Config;
use crate::git;
use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus, StateManager};
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;

pub struct CleanupResult {
    pub change_id: String,
    pub repos_cleaned: usize,
    pub repos_skipped: usize,
    pub repos_failed: usize,
    pub errors: Vec<String>,
}

/// Process cleanup command
pub fn process_cleanup_command(
    cli: &Cli,
    config: &Config,
    change_id: Option<&str>,
    all: bool,
    list: bool,
    include_remote: bool,
    force: bool,
) -> Result<()> {
    let state_manager = StateManager::new()?;

    if list {
        return list_cleanable_changes(&state_manager);
    }

    if all {
        return cleanup_all_merged(&state_manager, include_remote, force);
    }

    let change_id = change_id
        .ok_or_else(|| eyre::eyre!("Change ID required unless --all or --list is specified"))?;

    cleanup_single_change(&state_manager, change_id, include_remote, force)
}

/// List changes that can be cleaned up
fn list_cleanable_changes(state_manager: &StateManager) -> Result<()> {
    let states = state_manager.list()?;

    let cleanable: Vec<_> = states
        .iter()
        .filter(|s| {
            s.status == ChangeStatus::FullyMerged
                || s.status == ChangeStatus::PartiallyMerged
        })
        .collect();

    if cleanable.is_empty() {
        println!("No changes need cleanup.");
        return Ok(());
    }

    println!("Changes available for cleanup:\n");
    for state in cleanable {
        let repos_needing_cleanup = state.get_repos_needing_cleanup().len();
        let total_repos = state.repositories.len();
        let merged = state.repositories.values()
            .filter(|r| r.status == RepoChangeStatus::PrMerged)
            .count();

        println!("  📦 {} ({} merged, {} need cleanup)",
            state.change_id, merged, repos_needing_cleanup);

        if let Some(desc) = &state.description {
            println!("     {}", desc);
        }
    }

    println!("\nRun `gx cleanup <change-id>` to clean up a specific change.");
    println!("Run `gx cleanup --all` to clean up all merged changes.");

    Ok(())
}

/// Clean up all merged changes
fn cleanup_all_merged(
    state_manager: &StateManager,
    include_remote: bool,
    force: bool,
) -> Result<()> {
    let states = state_manager.list()?;

    let cleanable: Vec<_> = states
        .into_iter()
        .filter(|s| {
            s.status == ChangeStatus::FullyMerged
                || (force && s.status == ChangeStatus::PartiallyMerged)
        })
        .collect();

    if cleanable.is_empty() {
        println!("No changes to clean up.");
        return Ok(());
    }

    println!("Cleaning up {} change(s)...\n", cleanable.len());

    let mut total_cleaned = 0;
    let mut total_skipped = 0;
    let mut total_failed = 0;

    for mut state in cleanable {
        match cleanup_change(&mut state, include_remote, force) {
            Ok(result) => {
                total_cleaned += result.repos_cleaned;
                total_skipped += result.repos_skipped;
                total_failed += result.repos_failed;

                // Update state
                state_manager.save(&state)?;

                // Delete state if fully cleaned
                if state.get_repos_needing_cleanup().is_empty() {
                    state_manager.delete(&state.change_id)?;
                }
            }
            Err(e) => {
                warn!("Failed to cleanup {}: {}", state.change_id, e);
                total_failed += 1;
            }
        }
    }

    println!("\n📊 Cleanup summary:");
    println!("   🧹 {} branches cleaned", total_cleaned);
    println!("   ⏭️  {} skipped", total_skipped);
    if total_failed > 0 {
        println!("   ❌ {} failed", total_failed);
    }

    Ok(())
}

/// Clean up a single change
fn cleanup_single_change(
    state_manager: &StateManager,
    change_id: &str,
    include_remote: bool,
    force: bool,
) -> Result<()> {
    let mut state = state_manager
        .load(change_id)?
        .ok_or_else(|| eyre::eyre!("Change not found: {}", change_id))?;

    let result = cleanup_change(&mut state, include_remote, force)?;

    // Update state
    state_manager.save(&state)?;

    // Print summary
    println!("\n📊 Cleanup for {}:", change_id);
    println!("   🧹 {} branches cleaned", result.repos_cleaned);
    println!("   ⏭️  {} skipped", result.repos_skipped);
    if result.repos_failed > 0 {
        println!("   ❌ {} failed", result.repos_failed);
        for error in &result.errors {
            println!("      - {}", error);
        }
    }

    // Delete state if fully cleaned
    if state.get_repos_needing_cleanup().is_empty() && result.repos_failed == 0 {
        state_manager.delete(change_id)?;
        println!("   ✅ Change state removed");
    }

    Ok(())
}

/// Clean up branches for a change
fn cleanup_change(
    state: &mut ChangeState,
    include_remote: bool,
    force: bool,
) -> Result<CleanupResult> {
    let repos_to_clean = state.get_repos_needing_cleanup();

    let mut cleaned = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for repo_state in repos_to_clean {
        // Check if we should clean this repo
        if !force && repo_state.status != RepoChangeStatus::PrMerged {
            info!("Skipping {} - PR not merged", repo_state.repo_slug);
            skipped += 1;
            continue;
        }

        // Try to find local path
        let local_path = match &repo_state.local_path {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                // Try to find repo in current directory tree
                match find_repo_locally(&repo_state.repo_slug) {
                    Some(p) => p,
                    None => {
                        info!("Skipping {} - local repo not found", repo_state.repo_slug);
                        skipped += 1;
                        continue;
                    }
                }
            }
        };

        // Delete local branch
        match git::delete_local_branch(&local_path, &repo_state.branch_name) {
            Ok(()) => {
                info!("🧹 Deleted local branch {} in {}",
                    repo_state.branch_name, repo_state.repo_slug);
                cleaned += 1;

                // Mark as cleaned up in state
                state.mark_cleaned_up(&repo_state.repo_slug);
            }
            Err(e) => {
                // Check if branch doesn't exist (already deleted)
                let err_str = e.to_string();
                if err_str.contains("not found") || err_str.contains("does not exist") {
                    info!("Branch {} already deleted in {}",
                        repo_state.branch_name, repo_state.repo_slug);
                    state.mark_cleaned_up(&repo_state.repo_slug);
                    skipped += 1;
                } else {
                    warn!("Failed to delete branch {} in {}: {}",
                        repo_state.branch_name, repo_state.repo_slug, e);
                    errors.push(format!("{}: {}", repo_state.repo_slug, e));
                    failed += 1;
                }
            }
        }

        // Optionally delete remote branch
        if include_remote {
            if let Err(e) = git::delete_remote_branch(&local_path, &repo_state.branch_name) {
                // Remote branch might already be deleted by GitHub
                let err_str = e.to_string();
                if !err_str.contains("not found") && !err_str.contains("does not exist") {
                    warn!("Failed to delete remote branch {} in {}: {}",
                        repo_state.branch_name, repo_state.repo_slug, e);
                }
            }
        }
    }

    Ok(CleanupResult {
        change_id: state.change_id.clone(),
        repos_cleaned: cleaned,
        repos_skipped: skipped,
        repos_failed: failed,
        errors,
    })
}

/// Try to find a repository locally by slug
fn find_repo_locally(repo_slug: &str) -> Option<std::path::PathBuf> {
    // Extract repo name from slug
    let repo_name = repo_slug.split('/').last()?;

    // Check current directory and parent directories
    let current = std::env::current_dir().ok()?;

    // Try: ./repo_name
    let direct = current.join(repo_name);
    if direct.join(".git").exists() {
        return Some(direct);
    }

    // Try: ./org/repo_name
    let with_org = current.join(repo_slug);
    if with_org.join(".git").exists() {
        return Some(with_org);
    }

    None
}
```

### Files to Modify/Create

- **New**: `src/cleanup.rs` - Cleanup module
- **Modify**: `src/cli.rs` - Add Cleanup command
- **Modify**: `src/lib.rs` - Add `pub mod cleanup;`
- **Modify**: `src/main.rs` - Handle Cleanup command

---

## Issue 4: Emoji Width Calculation (P2)

### Problem

```
test_emoji_display_width_calculation ... FAILED
Emoji '⚠️ git': calculated=6, expected=5
```

Unicode width calculation is inconsistent for emoji with variation selectors.

### Solution

The issue is with `unicode-display-width` crate handling variation selectors. Options:

1. **Simplify emoji** - Use single-codepoint emoji without variation selectors
2. **Custom width override** - Create a lookup table for known emoji
3. **Accept terminal variation** - Different terminals render differently

### Recommended Approach: Custom Width Lookup

```rust
// src/output.rs

use std::collections::HashMap;
use std::sync::LazyLock;

/// Known emoji widths that differ from unicode-display-width calculation
static EMOJI_WIDTH_OVERRIDES: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    // Emoji with variation selectors that cause issues
    map.insert("⚠️", 2);
    map.insert("⬆️", 2);
    map.insert("⬇️", 2);
    map.insert("👁️", 2);
    map
});

/// Calculate display width with emoji corrections
pub fn calculate_display_width(s: &str) -> usize {
    // Check for known emoji patterns first
    for (emoji, width) in EMOJI_WIDTH_OVERRIDES.iter() {
        if s.starts_with(emoji) {
            let rest = &s[emoji.len()..];
            return *width + unicode_width(rest) as usize;
        }
    }

    unicode_width(s) as usize
}
```

### Alternative: Use Simple Emoji

Replace problematic emoji with single-codepoint alternatives:
- `⚠️` (U+26A0 + U+FE0F) → `⚠` (U+26A0 only)
- `⬆️` (U+2B06 + U+FE0F) → `↑` (U+2191)
- `⬇️` (U+2B07 + U+FE0F) → `↓` (U+2193)

### Files to Modify

- `src/output.rs` - Fix width calculation

---

## Issue 5: Retry Logic for Network Operations (P2)

### Problem

No retry logic for network failures during PR creation, branch push, etc.

### Solution

Add a retry utility with exponential backoff.

### Implementation

```rust
// src/utils.rs (add to existing file)

use std::time::Duration;
use std::thread;
use log::{debug, warn};

/// Configuration for retry behavior
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            backoff_factor: 2.0,
        }
    }
}

/// Retry an operation with exponential backoff
pub fn retry_with_backoff<T, E, F>(
    operation: F,
    config: &RetryConfig,
    operation_name: &str,
) -> Result<T, E>
where
    F: Fn() -> Result<T, E>,
    E: std::fmt::Display,
{
    let mut delay_ms = config.initial_delay_ms;

    for attempt in 1..=config.max_attempts {
        match operation() {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt == config.max_attempts {
                    warn!(
                        "{} failed after {} attempts: {}",
                        operation_name, config.max_attempts, e
                    );
                    return Err(e);
                }

                warn!(
                    "{} failed (attempt {}/{}): {}. Retrying in {}ms...",
                    operation_name, attempt, config.max_attempts, e, delay_ms
                );

                thread::sleep(Duration::from_millis(delay_ms));

                // Exponential backoff with cap
                delay_ms = ((delay_ms as f64) * config.backoff_factor) as u64;
                delay_ms = delay_ms.min(config.max_delay_ms);
            }
        }
    }

    unreachable!()
}

/// Check if an error is retryable (network-related)
pub fn is_retryable_error(error_msg: &str) -> bool {
    let retryable_patterns = [
        "timeout",
        "timed out",
        "connection refused",
        "connection reset",
        "network unreachable",
        "temporary failure",
        "503",
        "502",
        "504",
        "rate limit",
    ];

    let error_lower = error_msg.to_lowercase();
    retryable_patterns.iter().any(|p| error_lower.contains(p))
}
```

### Usage in GitHub Operations

```rust
// src/github.rs

pub fn create_pr(/* ... */) -> Result<()> {
    let config = RetryConfig::default();

    retry_with_backoff(
        || create_pr_inner(repo_slug, branch_name, commit_message, pr),
        &config,
        &format!("Create PR for {}", repo_slug),
    )
}
```

### Files to Modify

- `src/utils.rs` - Add retry utilities
- `src/github.rs` - Wrap operations with retry

---

## Issue 6: Add --dry-run Flag (P2)

### Problem

Only `create` has implicit dry-run. Other commands don't have preview mode.

### Solution

Add `--dry-run` flag to mutating commands.

### Implementation

Add to `cli.rs`:

```rust
// Global option in Cli struct
#[arg(long, global = true, help = "Preview changes without executing")]
pub dry_run: bool,
```

Modify commands to check `cli.dry_run`:

```rust
// In review.rs
pub fn process_review_approve_command(/* ... */) -> Result<()> {
    if cli.dry_run {
        println!("🔍 DRY RUN - Would approve the following PRs:");
        for pr in &open_prs {
            println!("  • PR #{}: {} ({})", pr.number, pr.title, pr.repo_slug);
        }
        return Ok(());
    }

    // ... actual execution
}
```

### Files to Modify

- `src/cli.rs` - Add global dry_run flag
- `src/review.rs` - Check dry_run before operations
- `src/checkout.rs` - Check dry_run before operations
- `src/clone.rs` - Check dry_run before operations

---

## Implementation Order

### Phase 1: Critical Fixes (P0)
1. Fix PR JSON parsing in `github.rs`
2. Implement state tracking module (`state.rs`)

### Phase 2: High Priority (P1)
3. Implement cleanup command
4. Integrate state tracking with create/review commands

### Phase 3: Medium Priority (P2)
5. Fix emoji width calculation
6. Add retry logic
7. Add --dry-run flag

### Phase 4: Testing & Documentation
8. Integration testing
9. Documentation updates
10. User testing and feedback

---

## Testing Strategy

### Unit Tests

Each new module should have comprehensive unit tests:
- `src/github.rs` - JSON parsing tests
- `src/state.rs` - State management tests
- `src/cleanup.rs` - Cleanup logic tests
- `src/utils.rs` - Retry logic tests

### Integration Tests

Create integration tests that:
1. Create changes across multiple repos
2. Verify state tracking
3. Test cleanup after merge
4. Test network retry behavior

### Manual Testing Checklist

- [ ] `gx review ls GX-xxx` returns actual PRs
- [ ] `gx create` saves state to `~/.gx/changes/`
- [ ] `gx cleanup GX-xxx` removes local branches
- [ ] `gx cleanup --list` shows mergeable changes
- [ ] Emoji alignment is consistent
- [ ] Network failures retry gracefully
- [ ] `--dry-run` previews without executing

---

## Success Criteria

| Metric | Target |
|--------|--------|
| `gx review ls` accuracy | 100% of PRs returned |
| State tracking | All create operations tracked |
| Cleanup success rate | >95% branches cleaned |
| Test coverage | >80% for new modules |
| No regressions | All 83 existing tests pass |

---

## Appendix: File Changes Summary

| File | Action | Description |
|------|--------|-------------|
| `src/github.rs` | Modify | Fix JSON parsing |
| `src/state.rs` | New | State management |
| `src/cleanup.rs` | New | Cleanup command |
| `src/cli.rs` | Modify | Add Cleanup command, --dry-run |
| `src/lib.rs` | Modify | Add new modules |
| `src/main.rs` | Modify | Handle Cleanup command |
| `src/output.rs` | Modify | Fix emoji width |
| `src/utils.rs` | Modify | Add retry logic |
| `src/create.rs` | Modify | Integrate state tracking |
| `src/review.rs` | Modify | Use state, add dry-run |

