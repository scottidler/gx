# GX Rollback Enhancement Implementation Plan

## Executive Summary

This document outlines the implementation plan to enhance GX's transaction rollback system to achieve feature parity with SLAM's comprehensive rollback capabilities. The analysis reveals that while GX has a solid transaction foundation, it's missing critical git-specific rollback operations and state preservation features that SLAM provides.

**Current State**: GX has ~60% rollback functionality compared to SLAM
**Target State**: 95%+ rollback functionality parity with enhanced reliability
**Estimated Effort**: 3-5 days of development work across 3 phases

## Table of Contents

1. [Current Gap Analysis](#current-gap-analysis)
2. [Phase 1: Critical Missing Features](#phase-1-critical-missing-features-high-priority)
3. [Phase 2: Enhanced Transaction System](#phase-2-enhanced-transaction-system-medium-priority)
4. [Phase 3: Advanced Features](#phase-3-advanced-features-lower-priority)
5. [Implementation Timeline](#implementation-timeline)
6. [Testing Strategy](#testing-strategy)
7. [Risk Assessment](#risk-assessment)

## Current Gap Analysis

### What GX Has ✅
- Basic transaction system with rollback stack
- File operation rollbacks (create/delete files)
- Branch switching rollback in `commit_changes()`
- Rollback safety check (prevents rollback after commit)
- Comprehensive test coverage for transaction basics

### What GX Is Missing ❌
- **Git stash management integration**
- **Comprehensive git operation rollbacks** (reset hard, reset commit, remote branch deletion)
- **Multi-state preservation** (original branch, head branch, stash state)
- **Enhanced git helper functions** for rollback operations
- **Granular rollback points** throughout git workflows

### SLAM's Superior Rollback Features
1. **Automatic stash save/restore** for uncommitted changes
2. **Head branch switching** with rollback capability
3. **File modification rollback** via `git reset --hard`
4. **Commit rollback** via `git reset --soft HEAD~1`
5. **Remote branch deletion rollback** for push operations
6. **Pre-commit hook state preservation** (intentionally excluded in GX)

## Phase 1: Critical Missing Features (High Priority)

**Estimated Effort**: 2-3 days
**Impact**: High - Brings core rollback functionality to SLAM parity

### 1.1 Add Missing Git Helper Functions

**File**: `src/git.rs`

Add the following functions to match SLAM's git operation capabilities:

```rust
/// Save current changes to stash with GX-specific message
/// Returns the stash reference (e.g., "stash@{0}")
pub fn stash_save(repo_path: &Path, message: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["stash", "push", "-m", message])
        .output()
        .map_err(|e| eyre!("Failed to run git stash push: {}", e))?;

    if output.status.success() {
        debug!("Stashed changes in '{}'", repo_path.display());
        // Return the stash reference - new stash is always stash@{0}
        Ok("stash@{0}".to_string())
    } else {
        Err(eyre!(
            "Failed to stash changes: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Pop specific stash by reference
pub fn stash_pop(repo_path: &Path, stash_ref: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["stash", "pop", stash_ref])
        .output()
        .map_err(|e| eyre!("Failed to run git stash pop: {}", e))?;

    if output.status.success() {
        debug!("Popped stash {} in '{}'", stash_ref, repo_path.display());
        Ok(())
    } else {
        Err(eyre!(
            "Failed to pop stash {}: {}",
            stash_ref,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Hard reset to HEAD (for rollback of file modifications)
pub fn reset_hard(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["reset", "--hard", "HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to run git reset --hard: {}", e))?;

    if output.status.success() {
        debug!("Hard reset completed in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!(
            "Failed to reset hard: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Reset last commit (for rollback of commits)
pub fn reset_commit(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["reset", "--soft", "HEAD~1"])
        .output()
        .map_err(|e| eyre!("Failed to run git reset --soft: {}", e))?;

    if output.status.success() {
        debug!("Commit reset completed in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!(
            "Failed to reset commit: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Get the default/head branch name for the repository
pub fn get_head_branch(repo_path: &Path) -> Result<String> {
    // First try to get the default branch from remote
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to get HEAD branch: {}", e))?;

    if output.status.success() {
        let head_ref = String::from_utf8_lossy(&output.stdout).trim();
        // Extract branch name from "refs/remotes/origin/main"
        if let Some(branch_name) = head_ref.strip_prefix("refs/remotes/origin/") {
            return Ok(branch_name.to_string());
        }
    }

    // Fallback: assume main or master
    for default_branch in &["main", "master"] {
        if branch_exists_remotely(repo_path, default_branch)? {
            return Ok(default_branch.to_string());
        }
    }

    Err(eyre!("Could not determine head branch"))
}

/// Check if a branch exists on remote
pub fn branch_exists_remotely(repo_path: &Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["ls-remote", "--heads", "origin", branch_name])
        .output()
        .map_err(|e| eyre!("Failed to check remote branch: {}", e))?;

    Ok(output.status.success() && !output.stdout.is_empty())
}

/// Delete a remote branch (for rollback of push operations)
pub fn delete_remote_branch(repo_path: &Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["push", "origin", "--delete", branch_name])
        .output()
        .map_err(|e| eyre!("Failed to delete remote branch: {}", e))?;

    if output.status.success() {
        debug!("Deleted remote branch '{}' in '{}'", branch_name, repo_path.display());
        Ok(())
    } else {
        // Don't fail if branch doesn't exist remotely
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("remote ref does not exist") {
            debug!("Remote branch '{}' already deleted", branch_name);
            Ok(())
        } else {
            Err(eyre!("Failed to delete remote branch: {}", stderr))
        }
    }
}

/// Check if repository has uncommitted changes (modified or staged files)
pub fn has_uncommitted_changes(repo_path: &Path) -> Result<bool> {
    let status = get_status_changes_for_path(repo_path)?;
    Ok(!status.is_empty())
}
```

### 1.2 Enhance Create Operation with Comprehensive Rollbacks

**File**: `src/create.rs`

Replace the `process_single_repo()` function with enhanced rollback capabilities:

```rust
/// Process create command for a single repository with comprehensive rollback
fn process_single_repo(
    repo: &Repo,
    change_id: &str,
    file_patterns: &[String],
    change: &Change,
    commit_message: Option<&str>,
    pr: Option<&crate::cli::PR>,
) -> CreateResult {
    debug!("Processing repository: {}", repo.name);

    let mut transaction = Transaction::new();
    let repo_path = &repo.path;
    let mut files_affected = Vec::new();
    let mut diff_parts = Vec::new();
    let mut stash_ref: Option<String> = None;
    let mut original_branch: Option<String> = None;
    let mut head_branch: Option<String> = None;

    // 1. Handle uncommitted changes with stash
    match git::has_uncommitted_changes(repo_path) {
        Ok(true) => {
            debug!("Found uncommitted changes, stashing...");
            match git::stash_save(repo_path, &format!("GX auto-stash for {}", change_id)) {
                Ok(stash) => {
                    stash_ref = Some(stash.clone());
                    transaction.add_rollback({
                        let repo_path = repo_path.clone();
                        let stash_ref = stash.clone();
                        move || {
                            debug!("Rolling back: restoring stashed changes");
                            git::stash_pop(&repo_path, &stash_ref)
                        }
                    });
                }
                Err(e) => {
                    return CreateResult {
                        repo: repo.clone(),
                        change_id: change_id.to_string(),
                        action: CreateAction::DryRun,
                        files_affected: Vec::new(),
                        substitution_stats: None,
                        error: Some(format!("Failed to stash changes: {}", e)),
                    };
                }
            }
        }
        Ok(false) => {} // No uncommitted changes
        Err(e) => {
            return CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected: Vec::new(),
                substitution_stats: None,
                error: Some(format!("Failed to check repository status: {}", e)),
            };
        }
    }

    // 2. Get current branch and switch to head branch if needed
    original_branch = match git::get_current_branch_name(repo_path) {
        Ok(branch) => Some(branch),
        Err(e) => {
            transaction.rollback();
            return CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected: Vec::new(),
                substitution_stats: None,
                error: Some(format!("Failed to get current branch: {}", e)),
            };
        }
    };

    // 3. Switch to head branch if not already on it
    match git::get_head_branch(repo_path) {
        Ok(head) => {
            head_branch = Some(head.clone());
            if let Some(ref current) = original_branch {
                if current != &head {
                    debug!("Switching from '{}' to head branch '{}'", current, head);
                    if let Err(e) = git::switch_branch(repo_path, &head) {
                        transaction.rollback();
                        return CreateResult {
                            repo: repo.clone(),
                            change_id: change_id.to_string(),
                            action: CreateAction::DryRun,
                            files_affected: Vec::new(),
                            substitution_stats: None,
                            error: Some(format!("Failed to switch to head branch: {}", e)),
                        };
                    }

                    // Add rollback to switch back to original branch
                    transaction.add_rollback({
                        let repo_path = repo_path.clone();
                        let original_branch = current.clone();
                        move || {
                            debug!("Rolling back: switching back to original branch '{}'", original_branch);
                            git::switch_branch(&repo_path, &original_branch)
                        }
                    });
                }
            }
        }
        Err(e) => {
            debug!("Could not determine head branch, continuing with current branch: {}", e);
        }
    }

    // 4. Pull latest changes
    if let Err(e) = git::pull_latest_changes(repo_path) {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),
            substitution_stats: None,
            error: Some(format!("Failed to pull latest changes: {}", e)),
        };
    }

    // 5. Apply changes with file modification rollback
    let mut substitution_stats = None;
    let change_result = match change {
        Change::Add(file_path, content) => {
            apply_add_change(
                repo_path,
                file_path,
                content,
                &mut transaction,
                &mut files_affected,
                &mut diff_parts,
            )
        }
        Change::Delete => {
            apply_delete_change(
                repo_path,
                file_patterns,
                &mut transaction,
                &mut files_affected,
                &mut diff_parts,
            )
        }
        Change::Sub(pattern, replacement) => {
            match apply_sub_change(
                repo_path,
                file_patterns,
                pattern,
                replacement,
                &mut transaction,
                &mut files_affected,
                &mut diff_parts,
            ) {
                Ok(stats) => {
                    substitution_stats = Some(stats);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        Change::Regex(pattern, replacement) => {
            match apply_regex_change(
                repo_path,
                file_patterns,
                pattern,
                replacement,
                &mut transaction,
                &mut files_affected,
                &mut diff_parts,
            ) {
                Ok(stats) => {
                    substitution_stats = Some(stats);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
    };

    // Add file modification rollback after changes are applied
    if !files_affected.is_empty() {
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            move || {
                debug!("Rolling back: resetting file modifications");
                git::reset_hard(&repo_path)
            }
        });
    }

    if let Err(e) = change_result {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),
            substitution_stats,
            error: Some(format!("Failed to apply changes: {}", e)),
        };
    }

    // If no files were affected, return early
    if files_affected.is_empty() {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),
            substitution_stats,
            error: None,
        };
    }

    // If no commit message, this is a dry run - rollback and return
    if commit_message.is_none() {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected,
            substitution_stats,
            error: None,
        };
    }

    // 6. Create branch and commit changes with comprehensive rollback
    let commit_result = commit_changes_with_rollback(
        repo_path,
        change_id,
        original_branch.as_deref().unwrap_or("main"),
        commit_message.unwrap(),
        &mut transaction,
    );

    match commit_result {
        Ok(()) => {
            let final_action = if let Some(pr) = pr {
                match create_pull_request(repo, change_id, commit_message.unwrap(), pr) {
                    Ok(()) => CreateAction::PrCreated,
                    Err(e) => {
                        warn!("Failed to create PR for {}: {}", repo.name, e);
                        CreateAction::Committed
                    }
                }
            } else {
                CreateAction::Committed
            };

            transaction.commit();
            CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: final_action,
                files_affected,
                substitution_stats,
                error: None,
            }
        }
        Err(e) => {
            transaction.rollback();
            CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected,
                substitution_stats,
                error: Some(format!("Failed to commit changes: {}", e)),
            }
        }
    }
}

/// Enhanced commit function with comprehensive rollback
fn commit_changes_with_rollback(
    repo_path: &Path,
    change_id: &str,
    original_branch: &str,
    commit_message: &str,
    transaction: &mut Transaction,
) -> Result<()> {
    // Check if branch existed before we try to create it
    let branch_existed = git::branch_exists_locally(repo_path, change_id)
        .unwrap_or(false);

    // Create and switch to branch (or switch to existing)
    git::create_branch(repo_path, change_id)
        .with_context(|| format!("Failed to create or switch to branch: {}", change_id))?;

    // Add branch rollback
    transaction.add_rollback({
        let repo_path = repo_path.to_path_buf();
        let original_branch = original_branch.to_string();
        let change_id = change_id.to_string();
        move || {
            debug!("Rolling back: switching back to original branch and cleaning up");
            // Switch back to original branch
            if let Err(e) = git::switch_branch(&repo_path, &original_branch) {
                warn!("Failed to switch back to original branch {}: {}", original_branch, e);
            }

            // Only delete the branch if we created it (not if it existed before)
            if !branch_existed {
                if let Err(e) = git::delete_local_branch(&repo_path, &change_id) {
                    warn!("Failed to delete branch {}: {}", change_id, e);
                }
            }

            Ok(())
        }
    });

    // Stage all changes
    git::add_all_changes(repo_path).context("Failed to stage changes")?;

    // Commit changes
    git::commit_changes(repo_path, commit_message).context("Failed to commit changes")?;

    // Add commit rollback
    transaction.add_rollback({
        let repo_path = repo_path.to_path_buf();
        move || {
            debug!("Rolling back: resetting commit");
            git::reset_commit(&repo_path)
        }
    });

    // Push branch to remote
    git::push_branch(repo_path, change_id).context("Failed to push branch")?;

    // Add push rollback (delete remote branch)
    transaction.add_rollback({
        let repo_path = repo_path.to_path_buf();
        let change_id = change_id.to_string();
        move || {
            debug!("Rolling back: deleting remote branch '{}'", change_id);
            git::delete_remote_branch(&repo_path, &change_id)
        }
    });

    Ok(())
}
```

### 1.3 Add Comprehensive Tests

**File**: `src/git.rs` (add to existing tests)

```rust
#[cfg(test)]
mod rollback_tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    fn setup_test_repo() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path().to_path_buf();

        // Initialize git repo
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["init"])
            .output()
            .unwrap();

        // Set up git config
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["config", "user.email", "test@example.com"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["config", "user.name", "Test User"])
            .output()
            .unwrap();

        (temp_dir, repo_path)
    }

    #[test]
    fn test_stash_save_and_pop() {
        let (_temp_dir, repo_path) = setup_test_repo();

        // Create a file and modify it
        fs::write(repo_path.join("test.txt"), "original content").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["add", "test.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["commit", "-m", "initial commit"])
            .output()
            .unwrap();

        // Modify the file (uncommitted change)
        fs::write(repo_path.join("test.txt"), "modified content").unwrap();

        // Test stash save
        let stash_ref = stash_save(&repo_path, "test stash").unwrap();
        assert_eq!(stash_ref, "stash@{0}");

        // File should be back to original content
        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "original content");

        // Test stash pop
        stash_pop(&repo_path, &stash_ref).unwrap();

        // File should have modified content again
        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "modified content");
    }

    #[test]
    fn test_reset_hard() {
        let (_temp_dir, repo_path) = setup_test_repo();

        // Create initial commit
        fs::write(repo_path.join("test.txt"), "original content").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["add", "test.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["commit", "-m", "initial commit"])
            .output()
            .unwrap();

        // Modify file
        fs::write(repo_path.join("test.txt"), "modified content").unwrap();

        // Reset hard should restore original content
        reset_hard(&repo_path).unwrap();

        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "original content");
    }

    #[test]
    fn test_reset_commit() {
        let (_temp_dir, repo_path) = setup_test_repo();

        // Create initial commit
        fs::write(repo_path.join("test.txt"), "original content").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["add", "test.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["commit", "-m", "initial commit"])
            .output()
            .unwrap();

        // Create second commit
        fs::write(repo_path.join("test2.txt"), "second file").unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["add", "test2.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo_path)
            .args(&["commit", "-m", "second commit"])
            .output()
            .unwrap();

        // Reset commit should undo the last commit but keep files staged
        reset_commit(&repo_path).unwrap();

        // test2.txt should still exist but be staged
        assert!(repo_path.join("test2.txt").exists());

        // Check git status to verify file is staged
        let output = Command::new("git")
            .current_dir(&repo_path)
            .args(&["status", "--porcelain"])
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&output.stdout);
        assert!(status.contains("A  test2.txt"));
    }
}
```

## Phase 2: Enhanced Transaction System (Medium Priority)

**Estimated Effort**: 1-2 days
**Impact**: Medium - Improves reliability and debugging capabilities

### 2.1 Enhanced Transaction Structure

**File**: `src/transaction.rs`

```rust
use eyre::Result;
use log::{debug, error, warn};

/// A single rollback action with metadata
pub struct RollbackAction {
    action: Box<dyn Fn() -> Result<()> + Send>,
    description: String,
    operation_type: RollbackType,
}

/// Types of rollback operations for categorization
#[derive(Debug, Clone, PartialEq)]
pub enum RollbackType {
    FileOperation,
    GitOperation,
    BranchOperation,
    StashOperation,
    RemoteOperation,
}

/// Enhanced transaction with operation tracking and granular rollback
pub struct Transaction {
    rollbacks: Vec<RollbackAction>,
    committed: bool,
    operation_count: usize,
    rollback_points: Vec<String>,
    continue_on_rollback_failure: bool,
}

impl Transaction {
    pub fn new() -> Self {
        Transaction {
            rollbacks: Vec::new(),
            committed: false,
            operation_count: 0,
            rollback_points: Vec::new(),
            continue_on_rollback_failure: true,
        }
    }

    /// Register a rollback action with description and type
    pub fn add_rollback_with_type<F>(
        &mut self,
        action: F,
        description: String,
        rollback_type: RollbackType,
    ) where
        F: Fn() -> Result<()> + Send + 'static,
    {
        self.rollbacks.push(RollbackAction {
            action: Box::new(action),
            description,
            operation_type: rollback_type,
        });
        self.operation_count += 1;
    }

    /// Register a rollback action (backward compatibility)
    pub fn add_rollback<F>(&mut self, action: F)
    where
        F: Fn() -> Result<()> + Send + 'static,
    {
        self.add_rollback_with_type(
            action,
            format!("Operation {}", self.operation_count + 1),
            RollbackType::GitOperation,
        );
    }

    /// Add a rollback point marker
    pub fn add_rollback_point(&mut self, description: String) {
        self.rollback_points.push(format!(
            "Point {}: {} (after {} operations)",
            self.rollback_points.len() + 1,
            description,
            self.operation_count
        ));
    }

    /// Execute rollback actions in reverse order with detailed logging
    pub fn rollback(&mut self) {
        if self.committed {
            debug!("Transaction already committed, skipping rollback");
            return;
        }

        error!(
            "Initiating rollback of {} actions across {} rollback points",
            self.rollbacks.len(),
            self.rollback_points.len()
        );

        // Log rollback points
        for point in &self.rollback_points {
            debug!("Rollback point: {}", point);
        }

        let mut successful_rollbacks = 0;
        let mut failed_rollbacks = 0;

        while let Some(rollback_action) = self.rollbacks.pop() {
            debug!(
                "Executing rollback: {} (type: {:?})",
                rollback_action.description, rollback_action.operation_type
            );

            match (rollback_action.action)() {
                Ok(()) => {
                    debug!("✓ Rollback succeeded: {}", rollback_action.description);
                    successful_rollbacks += 1;
                }
                Err(e) => {
                    error!(
                        "✗ Rollback failed: {} - Error: {:?}",
                        rollback_action.description, e
                    );
                    failed_rollbacks += 1;

                    if !self.continue_on_rollback_failure {
                        error!("Stopping rollback due to failure");
                        break;
                    }
                }
            }
        }

        if failed_rollbacks > 0 {
            warn!(
                "Rollback completed with {} successes and {} failures",
                successful_rollbacks, failed_rollbacks
            );
        } else {
            debug!("Rollback completed successfully with {} actions", successful_rollbacks);
        }
    }

    /// Rollback only specific types of operations
    pub fn rollback_type(&mut self, rollback_type: RollbackType) {
        if self.committed {
            debug!("Transaction already committed, skipping selective rollback");
            return;
        }

        debug!("Executing selective rollback for type: {:?}", rollback_type);

        let mut i = 0;
        while i < self.rollbacks.len() {
            if self.rollbacks[i].operation_type == rollback_type {
                let rollback_action = self.rollbacks.remove(i);
                debug!(
                    "Executing selective rollback: {}",
                    rollback_action.description
                );

                if let Err(e) = (rollback_action.action)() {
                    error!(
                        "Selective rollback failed: {} - Error: {:?}",
                        rollback_action.description, e
                    );
                }
            } else {
                i += 1;
            }
        }
    }

    /// Set whether to continue rollback on individual failures
    pub fn set_continue_on_failure(&mut self, continue_on_failure: bool) {
        self.continue_on_rollback_failure = continue_on_failure;
    }

    /// Get rollback statistics
    pub fn get_stats(&self) -> TransactionStats {
        TransactionStats {
            total_operations: self.operation_count,
            pending_rollbacks: self.rollbacks.len(),
            rollback_points: self.rollback_points.len(),
            committed: self.committed,
        }
    }

    /// Marks the transaction as committed and clears the rollback stack
    pub fn commit(&mut self) {
        self.committed = true;
        let cleared_count = self.rollbacks.len();
        self.rollbacks.clear();
        debug!(
            "Transaction committed successfully, cleared {} rollback actions",
            cleared_count
        );
    }
}

/// Transaction statistics for monitoring and debugging
#[derive(Debug)]
pub struct TransactionStats {
    pub total_operations: usize,
    pub pending_rollbacks: usize,
    pub rollback_points: usize,
    pub committed: bool,
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}
```

### 2.2 Update Create Operations to Use Enhanced Transaction

**File**: `src/create.rs` (update existing functions)

```rust
// Update the enhanced create function to use new transaction features
fn process_single_repo_enhanced(
    repo: &Repo,
    change_id: &str,
    file_patterns: &[String],
    change: &Change,
    commit_message: Option<&str>,
    pr: Option<&crate::cli::PR>,
) -> CreateResult {
    let mut transaction = Transaction::new();
    transaction.set_continue_on_failure(true); // Continue rollback even if some actions fail

    // Add rollback points for major phases
    transaction.add_rollback_point("Repository preparation phase".to_string());

    // ... existing stash logic with enhanced rollback ...
    if let Ok(stash) = git::stash_save(repo_path, &format!("GX auto-stash for {}", change_id)) {
        transaction.add_rollback_with_type(
            {
                let repo_path = repo_path.clone();
                let stash_ref = stash.clone();
                move || {
                    debug!("Rolling back: restoring stashed changes");
                    git::stash_pop(&repo_path, &stash_ref)
                }
            },
            format!("Restore stash: {}", stash),
            RollbackType::StashOperation,
        );
    }

    transaction.add_rollback_point("Branch operations phase".to_string());

    // ... branch switching with enhanced rollback ...
    transaction.add_rollback_with_type(
        {
            let repo_path = repo_path.clone();
            let original_branch = current_branch.clone();
            move || git::switch_branch(&repo_path, &original_branch)
        },
        format!("Switch back to branch: {}", current_branch),
        RollbackType::BranchOperation,
    );

    transaction.add_rollback_point("File modifications phase".to_string());

    // ... file operations with enhanced rollback ...
    transaction.add_rollback_with_type(
        {
            let repo_path = repo_path.clone();
            move || git::reset_hard(&repo_path)
        },
        "Reset file modifications".to_string(),
        RollbackType::FileOperation,
    );

    transaction.add_rollback_point("Git operations phase".to_string());

    // ... rest of implementation ...
}
```

## Phase 3: Advanced Features (Lower Priority)

**Estimated Effort**: 1-2 days
**Impact**: Low-Medium - Nice-to-have features for advanced use cases

### 3.1 Partial Rollback Support

**File**: `src/transaction.rs` (add to existing implementation)

```rust
impl Transaction {
    /// Rollback to a specific rollback point
    pub fn rollback_to_point(&mut self, point_index: usize) {
        if self.committed {
            debug!("Transaction already committed, skipping partial rollback");
            return;
        }

        if point_index >= self.rollback_points.len() {
            warn!("Invalid rollback point index: {}", point_index);
            return;
        }

        debug!(
            "Rolling back to point {}: {}",
            point_index, self.rollback_points[point_index]
        );

        // Calculate how many operations to rollback
        // This is a simplified implementation - in practice, you'd need to track
        // operation counts at each rollback point
        let operations_to_rollback = self.rollbacks.len().saturating_sub(point_index);

        for _ in 0..operations_to_rollback {
            if let Some(rollback_action) = self.rollbacks.pop() {
                debug!(
                    "Partial rollback: {}",
                    rollback_action.description
                );

                if let Err(e) = (rollback_action.action)() {
                    error!(
                        "Partial rollback failed: {} - Error: {:?}",
                        rollback_action.description, e
                    );
                }
            }
        }
    }

    /// Dry run rollback - show what would be rolled back without executing
    pub fn dry_run_rollback(&self) -> Vec<String> {
        let mut rollback_plan = Vec::new();

        rollback_plan.push(format!(
            "Rollback plan for {} operations:",
            self.rollbacks.len()
        ));

        for (index, rollback_action) in self.rollbacks.iter().rev().enumerate() {
            rollback_plan.push(format!(
                "  {}. {} (type: {:?})",
                index + 1,
                rollback_action.description,
                rollback_action.operation_type
            ));
        }

        rollback_plan
    }
}
```

### 3.2 Rollback Configuration and Monitoring

**File**: `src/config.rs` (add to existing config)

```rust
/// Rollback configuration options
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RollbackConfig {
    /// Continue rollback even if individual actions fail
    pub continue_on_failure: bool,

    /// Maximum time to wait for rollback operations (seconds)
    pub timeout_seconds: u64,

    /// Enable detailed rollback logging
    pub verbose_logging: bool,

    /// Types of operations to exclude from rollback
    pub exclude_types: Vec<String>,
}

impl Default for RollbackConfig {
    fn default() -> Self {
        RollbackConfig {
            continue_on_failure: true,
            timeout_seconds: 300, // 5 minutes
            verbose_logging: false,
            exclude_types: Vec::new(),
        }
    }
}
```

### 3.3 Rollback Monitoring and Metrics

**File**: `src/transaction.rs` (add to existing implementation)

```rust
use std::time::{Duration, Instant};

/// Rollback execution metrics
#[derive(Debug)]
pub struct RollbackMetrics {
    pub start_time: Instant,
    pub end_time: Option<Instant>,
    pub successful_operations: usize,
    pub failed_operations: usize,
    pub total_operations: usize,
    pub duration: Option<Duration>,
}

impl Transaction {
    /// Execute rollback with metrics collection
    pub fn rollback_with_metrics(&mut self) -> RollbackMetrics {
        let start_time = Instant::now();
        let total_operations = self.rollbacks.len();
        let mut successful_operations = 0;
        let mut failed_operations = 0;

        if self.committed {
            debug!("Transaction already committed, skipping rollback");
            return RollbackMetrics {
                start_time,
                end_time: Some(start_time),
                successful_operations: 0,
                failed_operations: 0,
                total_operations: 0,
                duration: Some(Duration::from_nanos(0)),
            };
        }

        error!(
            "Initiating rollback of {} actions",
            total_operations
        );

        while let Some(rollback_action) = self.rollbacks.pop() {
            match (rollback_action.action)() {
                Ok(()) => {
                    debug!("✓ Rollback succeeded: {}", rollback_action.description);
                    successful_operations += 1;
                }
                Err(e) => {
                    error!(
                        "✗ Rollback failed: {} - Error: {:?}",
                        rollback_action.description, e
                    );
                    failed_operations += 1;

                    if !self.continue_on_rollback_failure {
                        break;
                    }
                }
            }
        }

        let end_time = Instant::now();
        let duration = end_time.duration_since(start_time);

        debug!(
            "Rollback completed in {:?} with {} successes and {} failures",
            duration, successful_operations, failed_operations
        );

        RollbackMetrics {
            start_time,
            end_time: Some(end_time),
            successful_operations,
            failed_operations,
            total_operations,
            duration: Some(duration),
        }
    }
}
```

## Implementation Timeline

### Week 1: Phase 1 Implementation
- **Days 1-2**: Implement missing git helper functions
- **Day 3**: Enhance create operations with comprehensive rollbacks
- **Days 4-5**: Add comprehensive tests and debugging

### Week 2: Phase 2 Implementation (Optional)
- **Days 1-2**: Implement enhanced transaction system
- **Day 3**: Update create operations to use enhanced features

### Week 3: Phase 3 Implementation (Optional)
- **Days 1-2**: Implement advanced features
- **Day 3**: Integration testing and documentation

## Testing Strategy

### Unit Tests
- Test all new git helper functions individually
- Test transaction rollback scenarios
- Test rollback failure handling
- Test stash save/pop operations

### Integration Tests
- End-to-end create operation with rollback
- Multi-repository rollback scenarios
- Error injection testing for rollback reliability
- Performance testing for large-scale operations

### Manual Testing Scenarios
1. **Stash Rollback Test**: Create uncommitted changes, run create operation, verify stash restore on failure
2. **Branch Rollback Test**: Verify branch switching and cleanup on failure
3. **File Modification Rollback Test**: Verify file changes are reset on failure
4. **Commit Rollback Test**: Verify commits are undone on failure
5. **Push Rollback Test**: Verify remote branches are cleaned up on failure

### Test Data Requirements
- Repositories with various states (clean, dirty, different branches)
- Repositories with existing branches matching change IDs
- Repositories with various remote configurations
- Edge cases: empty repositories, repositories with no remotes

## Risk Assessment

### High Risk Items
1. **Git State Corruption**: Improper rollback could leave repositories in inconsistent states
   - **Mitigation**: Comprehensive testing, atomic operations where possible
2. **Data Loss**: Rollback operations could accidentally delete important data
   - **Mitigation**: Careful stash management, thorough testing of reset operations
3. **Remote Repository Impact**: Push rollbacks affect shared repositories
   - **Mitigation**: Clear documentation, optional rollback steps for remote operations

### Medium Risk Items
1. **Performance Impact**: Additional rollback operations may slow down create operations
   - **Mitigation**: Benchmark performance, optimize rollback operations
2. **Complexity Increase**: More complex transaction system may introduce bugs
   - **Mitigation**: Thorough testing, gradual rollout, feature flags

### Low Risk Items
1. **Backward Compatibility**: Changes should not break existing functionality
   - **Mitigation**: Maintain existing API, add new features as optional
2. **Configuration Complexity**: New rollback options may confuse users
   - **Mitigation**: Sensible defaults, clear documentation

## Success Metrics

### Functionality Metrics
- [ ] All critical SLAM rollback operations supported (stash, reset, branch cleanup)
- [ ] Rollback success rate > 95% in integration tests
- [ ] No data loss in rollback scenarios
- [ ] Rollback operations complete within 2x normal operation time

### Reliability Metrics
- [ ] Zero repository corruption incidents in testing
- [ ] Graceful handling of rollback failures
- [ ] Proper cleanup of temporary branches and stashes
- [ ] Consistent behavior across different repository states

### User Experience Metrics
- [ ] Clear error messages during rollback operations
- [ ] Detailed logging for debugging rollback issues
- [ ] Minimal impact on successful operation performance
- [ ] Intuitive configuration options for rollback behavior

## Conclusion

This implementation plan will bring GX's rollback functionality to full parity with SLAM while adding modern enhancements like detailed logging, metrics collection, and configurable behavior. The phased approach allows for incremental implementation and testing, reducing risk while ensuring comprehensive coverage of rollback scenarios.

The critical Phase 1 implementation addresses the most important gaps and should be prioritized for immediate development. Phases 2 and 3 provide additional value but can be implemented based on user feedback and requirements.

With this enhanced rollback system, GX will provide users with confidence that their repositories will be left in a clean state even when operations fail, matching and exceeding SLAM's reliability guarantees.
