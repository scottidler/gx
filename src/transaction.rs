use eyre::Result;
use log::{debug, error, warn};

/// A single rollback action with metadata
pub struct RollbackAction {
    action: Box<dyn Fn() -> Result<()> + Send>,
    description: String,
    operation_type: RollbackType,
}

/// Types of rollback operations for categorization
#[allow(clippy::enum_variant_names)]
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
            debug!("Rollback point: {point}");
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

    /// Rollback only specific types of operations
    #[allow(dead_code)]
    pub fn rollback_type(&mut self, rollback_type: RollbackType) {
        if self.committed {
            debug!("Transaction already committed, skipping selective rollback");
            return;
        }

        debug!("Executing selective rollback for type: {rollback_type:?}");

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
                        "Selective rollback failed: {} - Error: {e:?}",
                        rollback_action.description
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
    #[allow(dead_code)]
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
        debug!("Transaction committed successfully, cleared {cleared_count} rollback actions");
    }
}

/// Transaction statistics for monitoring and debugging
#[allow(dead_code)]
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

        // Only rollback stash operations
        transaction.rollback_type(RollbackType::StashOperation);

        // Only the stash operation should have executed
        let final_count = *counter.lock().unwrap();
        assert_eq!(final_count, 1);

        // Branch operation should still be in the rollbacks
        assert_eq!(transaction.rollbacks.len(), 1);
        assert_eq!(
            transaction.rollbacks[0].operation_type,
            RollbackType::BranchOperation
        );
    }

    #[test]
    fn test_transaction_stats() {
        let mut transaction = Transaction::new();

        transaction.add_rollback_point("Test phase".to_string());
        transaction.add_rollback(|| Ok(()));
        transaction.add_rollback(|| Ok(()));

        let stats = transaction.get_stats();
        assert_eq!(stats.total_operations, 2);
        assert_eq!(stats.pending_rollbacks, 2);
        assert_eq!(stats.rollback_points, 1);
        assert!(!stats.committed);

        transaction.commit();
        let stats_after_commit = transaction.get_stats();
        assert!(stats_after_commit.committed);
        assert_eq!(stats_after_commit.pending_rollbacks, 0);
    }

    #[test]
    fn test_continue_on_failure_setting() {
        let mut transaction = Transaction::new();
        assert!(transaction.continue_on_rollback_failure);

        transaction.set_continue_on_failure(false);
        assert!(!transaction.continue_on_rollback_failure);
    }
}
