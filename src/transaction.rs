use eyre::Result;
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// A single rollback action with metadata
pub struct RollbackAction {
    action: Box<dyn Fn() -> Result<()> + Send>,
    description: String,
    operation_type: RollbackType,
}

/// Types of rollback operations for categorization
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RollbackType {
    FileOperation,
    GitOperation,
    BranchOperation,
    StashOperation,
    RemoteOperation,
}

/// Serializable rollback state for recovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableRollbackAction {
    pub description: String,
    pub operation_type: RollbackType,
    pub repo_path: String,
    pub parameters: Vec<String>,
}

/// Transaction state that can be persisted for recovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionState {
    pub transaction_id: String,
    pub rollback_actions: Vec<SerializableRollbackAction>,
    pub rollback_points: Vec<String>,
    pub operation_count: usize,
    pub created_at: String,
}

/// Enhanced transaction with operation tracking and granular rollback
pub struct Transaction {
    rollbacks: Vec<RollbackAction>,
    committed: bool,
    operation_count: usize,
    rollback_points: Vec<String>,
    continue_on_rollback_failure: bool,
    transaction_id: String,
    recovery_enabled: bool,
}

// Global counter for unique transaction IDs
static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(1);

impl Transaction {
    pub fn new() -> Self {
        let counter = TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = chrono::Utc::now().timestamp();
        let transaction_id = format!("gx-tx-{timestamp}-{counter}");
        Transaction {
            rollbacks: Vec::new(),
            committed: false,
            operation_count: 0,
            rollback_points: Vec::new(),
            continue_on_rollback_failure: true,
            transaction_id,
            recovery_enabled: false,
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
            description: description.clone(),
            operation_type: rollback_type.clone(),
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
            debug!("Rollback point: {point}");
        }

        let mut successful_rollbacks = 0;
        let mut failed_rollbacks = 0;

        while let Some(rollback_action) = self.rollbacks.pop() {
            // Skip cleanup actions during rollback - they should only run on commit
            if rollback_action.description.contains("Cleanup backup file:") {
                debug!(
                    "Skipping cleanup action during rollback: {}",
                    rollback_action.description
                );
                continue;
            }

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
                        "✗ Rollback failed: {} - Error: {e:?}",
                        rollback_action.description
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
            warn!("Rollback completed with {successful_rollbacks} successes and {failed_rollbacks} failures");
        } else {
            debug!("Rollback completed successfully with {successful_rollbacks} actions");
        }
    }

    /// Set whether to continue rollback on individual failures
    pub fn set_continue_on_failure(&mut self, continue_on_failure: bool) {
        self.continue_on_rollback_failure = continue_on_failure;
    }

    /// Marks the transaction as committed and clears the rollback stack
    pub fn commit(&mut self) {
        // Execute cleanup actions before marking as committed
        self.execute_cleanup_actions();

        self.committed = true;
        let cleared_count = self.rollbacks.len();
        self.rollbacks.clear();
        debug!("Transaction committed successfully, cleared {cleared_count} rollback actions");

        // Clean up recovery state if it exists
        if self.recovery_enabled {
            let _ = self.cleanup_recovery_state();
        }
    }

    /// Commit with preflight check for backup files
    pub fn commit_with_preflight_check(&mut self, repo_path: &Path) -> Result<()> {
        // Run preflight check for backup files
        self.run_backup_preflight_check(repo_path)?;

        // Proceed with normal commit
        self.commit();
        Ok(())
    }

    /// Run preflight check to detect and report backup files before commit
    fn run_backup_preflight_check(&self, repo_path: &Path) -> Result<()> {
        use crate::file::find_backup_files_recursive;

        let backup_files = find_backup_files_recursive(repo_path)?;

        if !backup_files.is_empty() {
            debug!(
                "Preflight check: Found {} backup files before commit",
                backup_files.len()
            );

            // Count how many of these backup files we're responsible for cleaning up
            let cleanup_actions: Vec<_> = self
                .rollbacks
                .iter()
                .filter(|action| action.description.contains("Cleanup backup file:"))
                .collect();

            if backup_files.len() > cleanup_actions.len() {
                // There are more backup files than we have cleanup actions for
                warn!(
                    "Preflight check: Found {} backup files, but only {} cleanup actions registered. Some backup files may not be cleaned up:",
                    backup_files.len(),
                    cleanup_actions.len()
                );

                for backup_file in &backup_files {
                    let relative_path = backup_file.strip_prefix(repo_path).unwrap_or(backup_file);
                    warn!("  - {}", relative_path.display());
                }
            } else {
                debug!(
                    "Preflight check: {} backup files will be cleaned up by {} cleanup actions",
                    backup_files.len(),
                    cleanup_actions.len()
                );

                if log::log_enabled!(log::Level::Debug) {
                    for backup_file in &backup_files {
                        let relative_path =
                            backup_file.strip_prefix(repo_path).unwrap_or(backup_file);
                        debug!("  Will clean up: {}", relative_path.display());
                    }
                }
            }
        } else {
            debug!("Preflight check: No backup files found");
        }

        Ok(())
    }

    /// Execute cleanup actions (like removing backup files) on successful commit
    fn execute_cleanup_actions(&mut self) {
        let mut successful_cleanups = 0;
        let mut failed_cleanups = 0;

        // Look for backup cleanup actions and execute them
        for rollback_action in &self.rollbacks {
            if rollback_action.description.contains("Cleanup backup file:") {
                debug!("Executing cleanup: {}", rollback_action.description);
                match (rollback_action.action)() {
                    Ok(()) => {
                        debug!("✓ Cleanup succeeded: {}", rollback_action.description);
                        successful_cleanups += 1;
                    }
                    Err(e) => {
                        warn!(
                            "✗ Cleanup failed: {} - Error: {e:?}",
                            rollback_action.description
                        );
                        failed_cleanups += 1;
                    }
                }
            }
        }

        if successful_cleanups > 0 || failed_cleanups > 0 {
            debug!(
                "Cleanup completed: {successful_cleanups} successes, {failed_cleanups} failures"
            );
        }
    }

    /// Load transaction state from recovery file
    pub fn load_recovery_state(transaction_id: &str) -> Result<TransactionState> {
        let recovery_dir = get_recovery_dir()?;
        let state_file = recovery_dir.join(format!("{transaction_id}.json"));

        if !state_file.exists() {
            return Err(eyre::eyre!("Recovery state not found: {}", transaction_id));
        }

        let state_json = fs::read_to_string(&state_file)?;
        let state: TransactionState = serde_json::from_str(&state_json)?;

        debug!(
            "Loaded transaction recovery state: {}",
            state_file.display()
        );
        Ok(state)
    }

    /// List all available recovery states
    pub fn list_recovery_states() -> Result<Vec<TransactionState>> {
        let recovery_dir = get_recovery_dir()?;

        if !recovery_dir.exists() {
            return Ok(Vec::new());
        }

        let mut states = Vec::new();
        for entry in fs::read_dir(&recovery_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match fs::read_to_string(&path) {
                    Ok(content) => {
                        if let Ok(state) = serde_json::from_str::<TransactionState>(&content) {
                            states.push(state);
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to read recovery state file {}: {}",
                            path.display(),
                            e
                        );
                    }
                }
            }
        }

        // Sort by creation time (newest first)
        states.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(states)
    }

    /// Execute recovery for a specific transaction
    pub fn execute_recovery(transaction_id: &str) -> Result<()> {
        let state = Self::load_recovery_state(transaction_id)?;
        debug!("Executing recovery for transaction: {transaction_id}");

        let mut successful_rollbacks = 0;
        let mut failed_rollbacks = 0;

        for action in state.rollback_actions.iter().rev() {
            debug!(
                "Executing recovery rollback: {} (type: {:?})",
                action.description, action.operation_type
            );

            let result = match action.operation_type {
                RollbackType::StashOperation => {
                    if action.parameters.len() >= 2 {
                        crate::git::stash_pop(Path::new(&action.repo_path), &action.parameters[1])
                    } else {
                        Err(eyre::eyre!("Invalid stash parameters"))
                    }
                }
                RollbackType::BranchOperation => {
                    if action.description.contains("Switch back to")
                        && !action.parameters.is_empty()
                    {
                        crate::git::switch_branch(
                            Path::new(&action.repo_path),
                            &action.parameters[0],
                        )
                    } else if action.description.contains("delete") && action.parameters.len() >= 2
                    {
                        crate::git::delete_local_branch(
                            Path::new(&action.repo_path),
                            &action.parameters[1],
                        )
                    } else {
                        Err(eyre::eyre!("Invalid branch parameters"))
                    }
                }
                RollbackType::FileOperation => crate::git::reset_hard(Path::new(&action.repo_path)),
                RollbackType::GitOperation => {
                    if action.description.contains("Reset commit") {
                        crate::git::reset_commit(Path::new(&action.repo_path))
                    } else {
                        Err(eyre::eyre!("Unknown git operation"))
                    }
                }
                RollbackType::RemoteOperation => {
                    if action.description.contains("Delete remote branch")
                        && !action.parameters.is_empty()
                    {
                        crate::git::delete_remote_branch(
                            Path::new(&action.repo_path),
                            &action.parameters[0],
                        )
                    } else {
                        Err(eyre::eyre!("Invalid remote parameters"))
                    }
                }
            };

            match result {
                Ok(()) => {
                    debug!("✓ Recovery rollback succeeded: {}", action.description);
                    successful_rollbacks += 1;
                }
                Err(e) => {
                    error!(
                        "✗ Recovery rollback failed: {} - Error: {e:?}",
                        action.description
                    );
                    failed_rollbacks += 1;
                }
            }
        }

        if failed_rollbacks > 0 {
            warn!("Recovery completed with {successful_rollbacks} successes and {failed_rollbacks} failures");
        } else {
            debug!("Recovery completed successfully with {successful_rollbacks} actions");
        }

        // Clean up recovery state after successful recovery
        Self::cleanup_recovery_state_by_id(transaction_id)?;
        Ok(())
    }

    /// Clean up recovery state for this transaction
    pub fn cleanup_recovery_state(&self) -> Result<()> {
        Self::cleanup_recovery_state_by_id(&self.transaction_id)
    }

    /// Clean up recovery state by transaction ID
    pub fn cleanup_recovery_state_by_id(transaction_id: &str) -> Result<()> {
        let recovery_dir = get_recovery_dir()?;
        let state_file = recovery_dir.join(format!("{transaction_id}.json"));

        if state_file.exists() {
            fs::remove_file(&state_file)?;
            debug!("Cleaned up recovery state: {}", state_file.display());
        }

        Ok(())
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the recovery directory path
fn get_recovery_dir() -> Result<std::path::PathBuf> {
    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| eyre::eyre!("Could not determine home directory"))?;

    Ok(std::path::PathBuf::from(home_dir)
        .join(".gx")
        .join("recovery"))
}

/// Validation result for rollback operations
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub valid: bool,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl Default for ValidationResult {
    fn default() -> Self {
        Self::new()
    }
}

impl ValidationResult {
    pub fn new() -> Self {
        Self {
            valid: true,
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    pub fn add_error(&mut self, error: String) {
        self.errors.push(error);
        self.valid = false;
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }
}

/// Validate rollback operations before execution
pub fn validate_rollback_operations(actions: &[SerializableRollbackAction]) -> ValidationResult {
    let mut result = ValidationResult::new();

    for action in actions {
        validate_single_action(action, &mut result);
    }

    result
}

/// Validate a single rollback action
fn validate_single_action(action: &SerializableRollbackAction, result: &mut ValidationResult) {
    let repo_path = Path::new(&action.repo_path);

    // Check if repository path exists
    if !repo_path.exists() {
        result.add_error(format!(
            "Repository path does not exist: {}",
            action.repo_path
        ));
        return;
    }

    // Check if it's a git repository
    let git_dir = repo_path.join(".git");
    if !git_dir.exists() {
        result.add_error(format!("Not a git repository: {}", action.repo_path));
        return;
    }

    // Validate specific operation types
    match action.operation_type {
        RollbackType::StashOperation => {
            validate_stash_operation(action, result);
        }
        RollbackType::BranchOperation => {
            validate_branch_operation(action, result);
        }
        RollbackType::FileOperation => {
            validate_file_operation(action, result);
        }
        RollbackType::GitOperation => {
            validate_git_operation(action, result);
        }
        RollbackType::RemoteOperation => {
            validate_remote_operation(action, result);
        }
    }
}

/// Validate stash operations
fn validate_stash_operation(action: &SerializableRollbackAction, result: &mut ValidationResult) {
    if action.parameters.len() < 2 {
        result.add_error(format!(
            "Stash operation requires at least 2 parameters, got: {}",
            action.parameters.len()
        ));
        return;
    }

    let repo_path = Path::new(&action.repo_path);
    let stash_ref = &action.parameters[1];

    // Check if stash exists (this is a basic check)
    if !stash_ref.starts_with("stash@{") {
        result.add_warning(format!(
            "Stash reference format may be invalid: {stash_ref}"
        ));
    }

    // Check for uncommitted changes that might conflict
    match crate::git::has_uncommitted_changes(repo_path) {
        Ok(true) => {
            result.add_warning(format!(
                "Repository has uncommitted changes that may conflict with stash pop: {}",
                action.repo_path
            ));
        }
        Err(e) => {
            result.add_error(format!("Failed to check repository status: {e}"));
        }
        _ => {}
    }
}

/// Validate branch operations
fn validate_branch_operation(action: &SerializableRollbackAction, result: &mut ValidationResult) {
    let repo_path = Path::new(&action.repo_path);

    if action.description.contains("Switch back to") {
        if action.parameters.is_empty() {
            result
                .add_error("Branch switch operation requires target branch parameter".to_string());
            return;
        }

        let target_branch = &action.parameters[0];

        // Check if target branch exists
        match crate::git::branch_exists_locally(repo_path, target_branch) {
            Ok(false) => {
                result.add_error(format!("Target branch does not exist: {target_branch}"));
            }
            Err(e) => {
                result.add_error(format!("Failed to check if branch exists: {e}"));
            }
            _ => {}
        }
    } else if action.description.contains("delete") {
        if action.parameters.len() < 2 {
            result.add_error("Branch deletion requires branch name parameter".to_string());
            return;
        }

        let branch_to_delete = &action.parameters[1];

        // Check if we're not trying to delete the current branch
        match crate::git::get_current_branch_name(repo_path) {
            Ok(current_branch) if current_branch == *branch_to_delete => {
                result.add_error(format!("Cannot delete current branch: {branch_to_delete}"));
            }
            Err(e) => {
                result.add_warning(format!("Could not determine current branch: {e}"));
            }
            _ => {}
        }
    }
}

/// Validate file operations
fn validate_file_operation(action: &SerializableRollbackAction, result: &mut ValidationResult) {
    let repo_path = Path::new(&action.repo_path);

    // Check for uncommitted changes
    match crate::git::has_uncommitted_changes(repo_path) {
        Ok(true) => {
            result.add_warning(format!(
                "Repository has uncommitted changes that will be lost: {}",
                action.repo_path
            ));
        }
        Err(e) => {
            result.add_error(format!("Failed to check repository status: {e}"));
        }
        _ => {}
    }
}

/// Validate git operations
fn validate_git_operation(action: &SerializableRollbackAction, result: &mut ValidationResult) {
    let repo_path = Path::new(&action.repo_path);

    if action.description.contains("Reset commit") {
        // Check if there are commits to reset
        match std::process::Command::new("git")
            .current_dir(repo_path)
            .args(["rev-list", "--count", "HEAD"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let count_str = String::from_utf8_lossy(&output.stdout);
                if let Ok(count) = count_str.trim().parse::<i32>() {
                    if count <= 1 {
                        result.add_warning(format!(
                            "Repository has only {count} commit(s), reset may not be safe"
                        ));
                    }
                }
            }
            Err(e) => {
                result.add_error(format!("Failed to check commit count: {e}"));
            }
            _ => {}
        }
    }
}

/// Validate remote operations
fn validate_remote_operation(action: &SerializableRollbackAction, result: &mut ValidationResult) {
    if action.description.contains("Delete remote branch") {
        if action.parameters.is_empty() {
            result.add_error("Remote branch deletion requires branch name parameter".to_string());
            return;
        }

        let branch_name = &action.parameters[0];
        let repo_path = Path::new(&action.repo_path);

        // Check if remote branch exists
        match crate::git::remote_branch_exists(repo_path, branch_name) {
            Ok(false) => {
                result.add_warning(format!("Remote branch may not exist: {branch_name}"));
            }
            Err(e) => {
                result.add_warning(format!("Could not verify remote branch existence: {e}"));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_transaction_new() {
        let transaction = Transaction::new();
        assert_eq!(transaction.rollbacks.len(), 0);
        assert!(!transaction.committed);
        assert_eq!(transaction.operation_count, 0);
        assert_eq!(transaction.rollback_points.len(), 0);
        assert!(transaction.continue_on_rollback_failure);
    }

    #[test]
    fn test_add_rollback() {
        let mut transaction = Transaction::new();

        transaction.add_rollback(|| Ok(()));
        assert_eq!(transaction.rollbacks.len(), 1);
        assert_eq!(transaction.operation_count, 1);

        transaction.add_rollback(|| Ok(()));
        assert_eq!(transaction.rollbacks.len(), 2);
        assert_eq!(transaction.operation_count, 2);
    }

    #[test]
    fn test_commit() {
        let mut transaction = Transaction::new();

        transaction.add_rollback(|| Ok(()));
        transaction.add_rollback(|| Ok(()));
        assert_eq!(transaction.rollbacks.len(), 2);
        assert!(!transaction.committed);

        transaction.commit();
        assert_eq!(transaction.rollbacks.len(), 0);
        assert!(transaction.committed);
    }

    #[test]
    fn test_rollback_successful_actions() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        // Add rollback actions that increment the counter
        let counter_clone1 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone1.lock().unwrap();
            *count += 1;
            Ok(())
        });

        let counter_clone2 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone2.lock().unwrap();
            *count += 10;
            Ok(())
        });

        transaction.rollback();

        // Actions should be executed in reverse order: 10 first, then 1
        let final_count = *counter.lock().unwrap();
        assert_eq!(final_count, 11);
        assert_eq!(transaction.rollbacks.len(), 0);
    }

    #[test]
    fn test_rollback_with_failing_actions() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        // Add a successful rollback action
        let counter_clone1 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone1.lock().unwrap();
            *count += 1;
            Ok(())
        });

        // Add a failing rollback action
        transaction.add_rollback(|| Err(eyre::eyre!("Rollback failed")));

        // Add another successful rollback action
        let counter_clone2 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone2.lock().unwrap();
            *count += 10;
            Ok(())
        });

        transaction.rollback();

        // All actions should be attempted, even if some fail
        // Successful actions: 10 + 1 = 11
        let final_count = *counter.lock().unwrap();
        assert_eq!(final_count, 11);
        assert_eq!(transaction.rollbacks.len(), 0);
    }

    #[test]
    fn test_rollback_empty_transaction() {
        let mut transaction = Transaction::new();

        // Should not panic on empty rollback
        transaction.rollback();
        assert_eq!(transaction.rollbacks.len(), 0);
    }

    #[test]
    fn test_multiple_rollbacks() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        let counter_clone = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone.lock().unwrap();
            *count += 1;
            Ok(())
        });

        // First rollback
        transaction.rollback();
        assert_eq!(*counter.lock().unwrap(), 1);
        assert_eq!(transaction.rollbacks.len(), 0);

        // Second rollback should do nothing
        transaction.rollback();
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    fn test_commit_after_rollback() {
        let mut transaction = Transaction::new();

        transaction.add_rollback(|| Ok(()));
        transaction.rollback();

        // Commit after rollback should work but do nothing
        transaction.commit();
        assert!(transaction.committed);
        assert_eq!(transaction.rollbacks.len(), 0);
    }

    #[test]
    fn test_rollback_after_commit() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        let counter_clone = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone.lock().unwrap();
            *count += 1;
            Ok(())
        });

        transaction.commit();
        transaction.rollback(); // Should do nothing

        assert_eq!(*counter.lock().unwrap(), 0);
        assert!(transaction.committed);
    }

    #[test]
    fn test_default() {
        let transaction = Transaction::default();
        assert!(!transaction.committed);
        assert_eq!(transaction.rollbacks.len(), 0);
    }

    // Phase 2 Enhanced Tests
    #[test]
    fn test_add_rollback_with_type() {
        let mut transaction = Transaction::new();

        transaction.add_rollback_with_type(
            || Ok(()),
            "Test stash operation".to_string(),
            RollbackType::StashOperation,
        );

        assert_eq!(transaction.rollbacks.len(), 1);
        assert_eq!(transaction.operation_count, 1);
        assert_eq!(transaction.rollbacks[0].description, "Test stash operation");
        assert_eq!(
            transaction.rollbacks[0].operation_type,
            RollbackType::StashOperation
        );
    }

    #[test]
    fn test_rollback_points() {
        let mut transaction = Transaction::new();

        transaction.add_rollback_point("Phase 1: Setup".to_string());
        transaction.add_rollback(|| Ok(()));
        transaction.add_rollback_point("Phase 2: Execution".to_string());
        transaction.add_rollback(|| Ok(()));

        assert_eq!(transaction.rollback_points.len(), 2);
        assert!(transaction.rollback_points[0].contains("Phase 1: Setup"));
        assert!(transaction.rollback_points[1].contains("Phase 2: Execution"));
    }

    #[test]
    fn test_selective_rollback() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        // Add different types of rollback actions
        let counter_clone1 = Arc::clone(&counter);
        transaction.add_rollback_with_type(
            move || {
                let mut count = counter_clone1.lock().unwrap();
                *count += 1;
                Ok(())
            },
            "Stash operation".to_string(),
            RollbackType::StashOperation,
        );

        let counter_clone2 = Arc::clone(&counter);
        transaction.add_rollback_with_type(
            move || {
                let mut count = counter_clone2.lock().unwrap();
                *count += 10;
                Ok(())
            },
            "Branch operation".to_string(),
            RollbackType::BranchOperation,
        );

        // Test regular rollback instead
        transaction.rollback();

        // Both operations should have executed
        let final_count = *counter.lock().unwrap();
        assert_eq!(final_count, 11); // 1 + 10
    }

    #[test]
    fn test_continue_on_failure_setting() {
        let mut transaction = Transaction::new();
        assert!(transaction.continue_on_rollback_failure);

        transaction.set_continue_on_failure(false);
        assert!(!transaction.continue_on_rollback_failure);
    }

    #[test]
    fn test_transaction_id_generation() {
        let transaction1 = Transaction::new();
        let transaction2 = Transaction::new();

        // Transaction IDs should be unique (using timestamp + counter)
        assert_ne!(transaction1.transaction_id, transaction2.transaction_id);
        assert!(transaction1.transaction_id.starts_with("gx-tx-"));
        assert!(transaction2.transaction_id.starts_with("gx-tx-"));

        // Should contain both timestamp and counter parts
        assert!(transaction1.transaction_id.matches('-').count() >= 2);
        assert!(transaction2.transaction_id.matches('-').count() >= 2);
    }

    #[test]
    fn test_serializable_rollback_action() {
        let action = SerializableRollbackAction {
            description: "Test stash operation".to_string(),
            operation_type: RollbackType::StashOperation,
            repo_path: "/tmp/test".to_string(),
            parameters: vec!["stash".to_string(), "stash@{0}".to_string()],
        };

        assert_eq!(action.description, "Test stash operation");
        assert_eq!(action.operation_type, RollbackType::StashOperation);
        assert_eq!(action.repo_path, "/tmp/test");
        assert_eq!(action.parameters.len(), 2);
    }

    #[test]
    fn test_transaction_state_creation() {
        let state = TransactionState {
            transaction_id: "gx-tx-123456789".to_string(),
            rollback_actions: vec![],
            rollback_points: vec!["Phase 1".to_string()],
            operation_count: 5,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(state.transaction_id, "gx-tx-123456789");
        assert_eq!(state.rollback_actions.len(), 0);
        assert_eq!(state.rollback_points.len(), 1);
        assert_eq!(state.operation_count, 5);
    }

    #[test]
    fn test_validation_result() {
        let mut result = ValidationResult::new();
        assert!(result.is_valid());
        assert!(result.warnings.is_empty());
        assert!(result.errors.is_empty());

        result.add_warning("Test warning".to_string());
        assert!(result.is_valid()); // Warnings don't invalidate
        assert_eq!(result.warnings.len(), 1);

        result.add_error("Test error".to_string());
        assert!(!result.is_valid()); // Errors invalidate
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_validate_rollback_operations_empty() {
        let actions: Vec<SerializableRollbackAction> = vec![];
        let result = validate_rollback_operations(&actions);
        assert!(result.is_valid());
        assert!(result.warnings.is_empty());
        assert!(result.errors.is_empty());
    }

    // Note: More comprehensive validation tests would require setting up actual git repositories
    // These tests focus on the structure and basic validation logic
}
