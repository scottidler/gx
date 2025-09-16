# Stephen's Issues 2-6: Implementation Plan

## Overview

This document outlines the detailed implementation plan to address Stephen's remaining issues with `gx`:

- **Issue #2**: Need to be able to create PRs in Draft mode
- **Issue #3**: Better feedback when regex patterns don't match (currently just shows "X dry runs")
- **Issue #4**: Can't run successive gx commands on the same branch + multi-substitution support
- **Issue #5**: Add option to disable committing .backup files created during substitution
- **Issue #6**: Fix branch detection when running gx from different directories (workspace vs single repo)

## Issue Analysis

### Issue #2: Draft PR Creation

**Current State:**
- `gx` creates standard PRs using `gh pr create` in `src/github.rs:create_pr()`
- No option to create draft PRs
- Current command: `gx create --pr -c "message" ...`

**User Need:**
- Ability to create PRs in draft mode for work-in-progress changes
- Maintains current workflow but adds draft capability

**Current Implementation:**
```rust
// src/github.rs:130-164
pub fn create_pr(repo_slug: &str, branch_name: &str, commit_message: &str) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "pr", "create",
            "--repo", repo_slug,
            "--head", branch_name,
            "--title", &title,
            "--body", &body,
            "--base", "main",
        ])
        .output()
```

### Issue #3: Regex Feedback Improvement

**Current State:**
- When regex patterns don't match, users see "X dry runs" in output
- No indication that zero matches occurred vs. intentional dry run
- Current logic in `src/diff.rs:apply_regex_substitution()` returns `None` for no matches

**User Need:**
- Clear indication when regex patterns find zero matches
- Distinguish between "no matches found" and "dry run mode"
- Expected to see "X committed" but got "X dry runs" with no explanation

**Current Implementation:**
```rust
// src/diff.rs:74-90
pub fn apply_regex_substitution(
    content: &str,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> Result<Option<(String, String)>> {
    let regex = Regex::new(pattern)?;
    if !regex.is_match(content) {
        return Ok(None);  // Silent failure - no feedback
    }
    // ...
}
```

### Issue #4: Successive Commands & Multi-Substitution

**Current State:**
- `gx create -x same-branch` fails when branch already exists
- `git::create_branch()` uses `git checkout -b` which fails if branch exists
- No support for multiple substitutions in single command

**User Need:**
1. **Successive Commands**: Ability to run multiple `gx` commands on same branch
2. **Multi-Substitution**: Run multiple substitutions on separate file groups in one command

**Current Implementation:**
```rust
// src/git.rs:947-987 (UPDATED ANALYSIS)
pub fn create_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    // Check if branch already exists locally
    if branch_exists_locally(repo_path, branch_name)? {
        debug!("Branch '{branch_name}' already exists locally, switching to it");
        return switch_branch(repo_path, branch_name);
    }

    // Check if branch exists on remote
    if branch_exists_on_remote(repo_path, branch_name)? {
        debug!("Branch '{branch_name}' exists on remote, checking out");
        return checkout_remote_branch(repo_path, branch_name);
    }

    // Create new branch from current HEAD
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "checkout", "-b", branch_name])
        .output()
```

**Analysis Update**: The current implementation already handles existing branches! The issue Stephen reported may have been fixed or may be context-specific.

### Issue #5: Backup File Cleanup

**Current State:**
- `gx` creates `.backup` files during substitutions for rollback purposes
- Backup files are cleaned up only during rollback operations via `restore_from_backup()`
- When transactions commit successfully, backup files are left behind
- No option to disable backup file creation or ensure cleanup

**User Need:**
- Option to disable committing `.backup` files created during substitution
- Automatic cleanup of backup files after successful operations
- User control over backup file behavior

**Current Implementation:**
```rust
// src/file.rs:113-134 - Backup creation
pub fn backup_file(file_path: &Path) -> Result<PathBuf> {
    let backup_path = file_path.with_extension(format!(
        "{}.backup",
        file_path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    fs::copy(file_path, &backup_path)?;
    Ok(backup_path)
}

// src/file.rs:137-156 - Restore and cleanup (only called during rollback)
pub fn restore_from_backup(backup_path: &Path, original_path: &Path) -> Result<()> {
    fs::copy(backup_path, original_path)?;
    // Remove backup file only during rollback
    fs::remove_file(backup_path)?;
    Ok(())
}

// src/transaction.rs:264-274 - Successful commit clears rollback actions
pub fn commit(&mut self) {
    self.committed = true;
    let cleared_count = self.rollbacks.len();
    self.rollbacks.clear(); // Backup cleanup actions are discarded
    debug!("Transaction committed successfully, cleared {cleared_count} rollback actions");
}
```

**Problem Analysis**:
- Backup files are created in `apply_regex_change()` and `apply_substitution_change()`
- Cleanup only happens via rollback actions that restore original files
- When transactions commit successfully, rollback actions are cleared without execution
- Result: `.backup` files accumulate in the filesystem

### Issue #6: Branch Detection Context Sensitivity

**Current State:**
- `gx` uses current working directory as the starting point for repository discovery
- Branch detection logic may behave differently when run from workspace vs single repo
- Repository discovery starts from `cli.cwd` or `std::env::current_dir()`

**User Need:**
- Consistent branch detection behavior regardless of execution directory
- Proper handling when running from workspace directory vs individual repo directory

**Current Implementation:**
```rust
// src/create.rs:161-169 - Working directory determination
let current_dir = std::env::current_dir()?;
let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
let max_depth = cli.max_depth.or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth)).unwrap_or(3);

// src/repo.rs:28-60 - Repository discovery
pub fn discover_repos(start_dir: &Path, max_depth: usize) -> Result<Vec<Repo>> {
    for entry in WalkDir::new(start_dir).max_depth(max_depth) {
        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            // Found git repo
        }
    }
}

// src/git.rs:1156-1185 - Branch existence checking
pub fn branch_exists_locally(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "rev-parse", "--verify", &format!("refs/heads/{branch_name}")])
        .output()?;
    Ok(output.status.success())
}
```

**Analysis**:
- The issue likely stems from different repository discovery results based on execution context
- When running from workspace: discovers multiple repos, branch detection per repo
- When running from single repo: discovers single repo, different behavior
- Branch detection itself uses `-C repo_path` so should be consistent
- The issue may be in how repository paths are resolved or how the CLI determines which repos to operate on

## Implementation Plan

### Phase 1: Issue #2 - Draft PR Support

**Priority**: High
**Estimated Effort**: 2-3 hours
**Files to Modify**: `src/cli.rs`, `src/github.rs`

#### 1.1 CLI Changes
**File**: `src/cli.rs`
**Location**: Lines 214-217 (Create command struct)

**Add new flag:**
```rust
#[derive(Args, Debug)]
pub struct Create {
    // ... existing fields ...

    /// Create pull request after committing
    #[arg(long, help = "Create pull request after committing")]
    pr: bool,

    /// Create pull request in draft mode
    #[arg(long, help = "Create pull request in draft mode", requires = "pr")]
    draft: bool,
}
```

**Alternative approach** (simpler):
```rust
/// Create pull request after committing (use --pr=draft for draft mode)
#[arg(long, help = "Create pull request after committing. Use 'draft' for draft mode")]
pr: Option<String>,
```

#### 1.2 GitHub Integration Changes
**File**: `src/github.rs`
**Location**: Lines 130-164 (`create_pr` function)

**Modify function signature:**
```rust
pub fn create_pr(repo_slug: &str, branch_name: &str, commit_message: &str, draft: bool) -> Result<()>
```

**Update implementation:**
```rust
pub fn create_pr(repo_slug: &str, branch_name: &str, commit_message: &str, draft: bool) -> Result<()> {
    let mut args = vec![
        "pr", "create",
        "--repo", repo_slug,
        "--head", branch_name,
        "--title", &title,
        "--body", &body,
        "--base", "main",
    ];

    if draft {
        args.push("--draft");
    }

    let output = Command::new("gh").args(&args).output()
    // ... rest unchanged
}
```

#### 1.3 Integration Changes
**File**: `src/create.rs`
**Location**: Lines 364-370 (PR creation logic)

**Update call sites:**
```rust
match create_pull_request(repo, change_id, commit_message.unwrap(), draft_mode) {
    Ok(()) => CreateAction::PrCreated,
    // ...
}
```

**File**: `src/main.rs`
**Location**: Lines 93-136 (Create command handling)

**Pass draft flag through:**
```rust
let draft_mode = matches!(pr_option.as_deref(), Some("draft")) || draft_flag;
```

### Phase 2: Issue #3 - Regex Feedback Enhancement

**Priority**: Medium
**Estimated Effort**: 3-4 hours
**Files to Modify**: `src/diff.rs`, `src/create.rs`, `src/file.rs`

#### 2.1 Enhanced Return Types
**File**: `src/diff.rs`
**Location**: Lines 74-90 (`apply_regex_substitution`)

**Create new result enum:**
```rust
#[derive(Debug, Clone)]
pub enum SubstitutionResult {
    Changed(String, String),  // (updated_content, diff)
    NoMatches,               // Pattern valid but no matches found
    NoChange,               // Matches found but no actual changes
}

pub fn apply_regex_substitution(
    content: &str,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> Result<SubstitutionResult> {
    let regex = Regex::new(pattern)?;

    if !regex.is_match(content) {
        return Ok(SubstitutionResult::NoMatches);
    }

    let updated = regex.replace_all(content, replacement).to_string();
    if updated == content {
        return Ok(SubstitutionResult::NoChange);
    }

    let diff = generate_diff(content, &updated, buffer);
    Ok(SubstitutionResult::Changed(updated, diff))
}
```

#### 2.2 Enhanced Feedback in Create
**File**: `src/create.rs`
**Location**: Lines 556-612 (`apply_regex_change`)

**Track match statistics:**
```rust
fn apply_regex_change(
    // ... existing params ...
) -> Result<MatchStats> {
    let mut stats = MatchStats::new();

    for file_path in all_files {
        match file::apply_regex_to_file(&full_path, pattern, replacement, 3)? {
            SubstitutionResult::Changed(updated_content, diff) => {
                // Apply changes
                stats.files_changed += 1;
                stats.matches_found += regex.find_iter(&original_content).count();
            }
            SubstitutionResult::NoMatches => {
                stats.files_no_matches += 1;
            }
            SubstitutionResult::NoChange => {
                stats.files_no_change += 1;
                stats.matches_found += regex.find_iter(&original_content).count();
            }
        }
    }

    Ok(stats)
}
```

#### 2.3 Enhanced Output Display
**File**: `src/create.rs`
**Location**: Lines 671-700 (`display_create_summary`)

**Add detailed feedback:**
```rust
fn display_create_summary(results: &[CreateResult], opts: &StatusOptions) {
    // ... existing summary ...

    // Add regex feedback
    let total_files_scanned = results.iter().map(|r| r.files_scanned).sum::<usize>();
    let files_with_matches = results.iter().map(|r| r.files_with_matches).sum::<usize>();
    let total_matches = results.iter().map(|r| r.total_matches).sum::<usize>();

    if total_files_scanned > 0 {
        println!("\n📊 Pattern Analysis:");
        println!("  Files scanned: {}", total_files_scanned);
        println!("  Files with matches: {}", files_with_matches);
        println!("  Total matches found: {}", total_matches);

        if files_with_matches == 0 {
            println!("  ⚠️  No files matched the regex pattern");
        }
    }
}
```

### Phase 3: Issue #4 - Successive Commands & Multi-Substitution

**Priority**: Medium (already partially implemented)
**Estimated Effort**: 2-3 hours (verification and testing)
**Files to Modify**: `src/cli.rs`, `src/create.rs`

#### 3.1 Verification of Existing Implementation
**File**: `src/git.rs`
**Location**: Lines 947-987 (`create_branch`)

**Current implementation already handles this:**
```rust
pub fn create_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    // Check if branch already exists locally
    if branch_exists_locally(repo_path, branch_name)? {
        debug!("Branch '{branch_name}' already exists locally, switching to it");
        return switch_branch(repo_path, branch_name);
    }

    // Check if branch exists on remote
    if branch_exists_on_remote(repo_path, branch_name)? {
        debug!("Branch '{branch_name}' exists on remote, checking out");
        return checkout_remote_branch(repo_path, branch_name);
    }

    // Create new branch from current HEAD
    // ... existing implementation
}
```

**Action Required**: Test to confirm this resolves Stephen's issue. The logic is already implemented.

#### 3.2 Multi-Substitution CLI Support
**File**: `src/cli.rs`
**Location**: Lines 222-247 (`CreateAction` enum)

**Add multi-substitution action:**
```rust
#[derive(Subcommand, Debug)]
pub enum CreateAction {
    // ... existing actions ...

    /// Multiple substitutions on different file groups
    Multi {
        #[arg(help = "Substitution specs in format 'files:pattern:replacement'")]
        specs: Vec<String>,
    },
}
```

**Alternative approach** (extend existing):
```rust
/// Regex substitution (supports multiple with --files per pattern)
Regex {
    #[arg(help = "Regex pattern to find")]
    pattern: String,
    #[arg(help = "Replacement text")]
    replacement: String,
    #[arg(long, help = "File patterns for this substitution (overrides global --files)")]
    files: Option<Vec<String>>,
},
```

#### 3.3 Multi-Substitution Processing
**File**: `src/create.rs`
**Location**: Lines 278-313 (change application logic)

**Add multi-substitution handling:**
```rust
Change::Multi(substitutions) => {
    for sub in substitutions {
        let sub_result = apply_regex_change(
            repo_path,
            &sub.files,
            &sub.pattern,
            &sub.replacement,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        if let Err(e) = sub_result {
            transaction.rollback();
            return CreateResult {
                error: Some(format!("Multi-substitution failed: {e}")),
                // ...
            };
        }
    }
}
```

#### 3.4 Enhanced Change ID Logic
**File**: `src/create.rs`
**Location**: Lines 622-654 (`commit_changes`)

**Modify branch creation to handle existing branches:**
```rust
fn commit_changes(
    repo_path: &Path,
    change_id: &str,
    original_branch: &str,
    commit_message: &str,
    transaction: &mut Transaction,
) -> Result<()> {
    let branch_existed = git::branch_exists_locally(repo_path, change_id)?;

    if branch_existed {
        // Switch to existing branch
        git::switch_branch(repo_path, change_id)
            .with_context(|| format!("Failed to switch to existing branch: {change_id}"))?;

        // Only add rollback to switch back (don't delete existing branch)
        let original_branch = original_branch.to_string();
        let repo_path_clone = repo_path.to_path_buf();
        transaction.add_rollback(move || {
            git::switch_branch(&repo_path_clone, &original_branch)
        });
    } else {
        // Create new branch (existing logic)
        git::create_branch(repo_path, change_id)
            .with_context(|| format!("Failed to create branch: {change_id}"))?;

        // Add rollback to delete created branch
        // ... existing rollback logic
    }

    // ... rest unchanged (stage, commit, push)
}
```

### Phase 4: Issue #5 - Backup File Cleanup

**Priority**: High
**Estimated Effort**: 3-4 hours
**Files to Modify**: `src/transaction.rs`, `src/file.rs`, `src/cli.rs`, `src/config.rs`

#### 4.1 Add Cleanup Actions to Transaction
**File**: `src/transaction.rs`
**Location**: Lines 264-274 (`commit` method)

**Add cleanup phase before clearing rollbacks:**
```rust
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

/// Execute cleanup actions (like removing backup files) on successful commit
fn execute_cleanup_actions(&mut self) {
    let mut successful_cleanups = 0;
    let mut failed_cleanups = 0;

    // Look for backup cleanup actions and execute them
    for rollback_action in &self.rollbacks {
        if rollback_action.description.contains("backup cleanup") {
            debug!("Executing cleanup: {}", rollback_action.description);
            match (rollback_action.action)() {
                Ok(()) => {
                    debug!("✓ Cleanup succeeded: {}", rollback_action.description);
                    successful_cleanups += 1;
                }
                Err(e) => {
                    warn!("✗ Cleanup failed: {} - Error: {e:?}", rollback_action.description);
                    failed_cleanups += 1;
                }
            }
        }
    }

    if successful_cleanups > 0 || failed_cleanups > 0 {
        debug!("Cleanup completed: {successful_cleanups} successes, {failed_cleanups} failures");
    }
}
```

#### 4.2 Add Backup Cleanup Functions
**File**: `src/file.rs`
**Location**: After line 156

**Add cleanup-only function:**
```rust
/// Clean up a backup file without restoring
pub fn cleanup_backup_file(backup_path: &Path) -> Result<()> {
    if backup_path.exists() {
        fs::remove_file(backup_path)
            .with_context(|| format!("Failed to remove backup file: {}", backup_path.display()))?;
        debug!("Cleaned up backup file: {}", backup_path.display());
    }
    Ok(())
}
```

#### 4.3 Add Configuration Option
**File**: `src/config.rs`
**Location**: Add to config struct

**Add backup behavior control:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    /// Whether to clean up backup files after successful operations
    pub cleanup_on_success: bool,
    /// Whether to create backup files at all
    pub create_backups: bool,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            cleanup_on_success: true,
            create_backups: true,
        }
    }
}

// Add to main Config struct:
pub backup: Option<BackupConfig>,
```

#### 4.4 Add CLI Flag
**File**: `src/cli.rs`
**Location**: Add global flag

**Add backup control flag:**
```rust
/// Disable backup file creation
#[arg(long, global = true, help = "Disable creation of .backup files")]
no_backup: bool,

/// Keep backup files after successful operations
#[arg(long, global = true, help = "Keep .backup files after successful operations")]
keep_backups: bool,
```

#### 4.5 Update File Operations
**File**: `src/create.rs`
**Location**: Lines 748, 661 (backup creation sites)

**Add cleanup actions alongside rollback actions:**
```rust
// In apply_regex_change and apply_substitution_change
match file::apply_regex_to_file(&full_path, pattern, replacement, 3)? {
    diff::SubstitutionResult::Changed(updated_content, diff) => {
        // Create backup for rollback (if enabled)
        let backup_path = if config.backup.as_ref().map_or(true, |b| b.create_backups) && !cli.no_backup {
            Some(file::backup_file(&full_path)?)
        } else {
            None
        };

        // Write updated content
        file::write_file_content(&full_path, &updated_content)?;

        // Add rollback action (only if backup was created)
        if let Some(backup_path) = &backup_path {
            let backup_path_clone = backup_path.clone();
            let full_path_clone = full_path.clone();
            transaction.add_rollback(move || {
                file::restore_from_backup(&backup_path_clone, &full_path_clone)
            });

            // Add cleanup action (unless user wants to keep backups)
            if !cli.keep_backups && config.backup.as_ref().map_or(true, |b| b.cleanup_on_success) {
                let cleanup_backup_path = backup_path.clone();
                transaction.add_rollback_with_type(
                    move || file::cleanup_backup_file(&cleanup_backup_path),
                    format!("Cleanup backup file: {}", backup_path.display()),
                    RollbackType::FileOperation,
                );
            }
        }
        // ... rest of the logic
    }
}
```

### Phase 5: Issue #6 - Branch Detection Context Consistency

**Priority**: Medium
**Estimated Effort**: 2-3 hours
**Files to Modify**: `src/repo.rs`, `src/git.rs`, `src/main.rs`

#### 5.1 Enhanced Repository Context Detection
**File**: `src/repo.rs`
**Location**: Lines 28-60 (`discover_repos`)

**Add single-repo detection mode:**
```rust
/// Discover git repositories with context awareness
pub fn discover_repos_with_context(start_dir: &Path, max_depth: usize) -> Result<(Vec<Repo>, RepoContext)> {
    debug!("Discovering repos from {} with max depth {}", start_dir.display(), max_depth);

    // Check if start_dir itself is a git repository
    let start_dir_is_repo = start_dir.join(".git").exists();

    if start_dir_is_repo && max_depth <= 1 {
        // Single repository mode
        let repo = Repo::new(start_dir.to_path_buf());
        debug!("Single repo mode: {} at {}", repo.name, repo.path.display());
        return Ok((vec![repo], RepoContext::SingleRepo));
    }

    // Multi-repository workspace mode
    let mut repos = Vec::new();
    for entry in WalkDir::new(start_dir).max_depth(max_depth).into_iter().filter_entry(|e| !is_ignored_directory(e.path())) {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            if let Some(repo_root) = path.parent() {
                let repo = Repo::new(repo_root.to_path_buf());
                debug!("Found repo: {} at {}", repo.name, repo.path.display());
                repos.push(repo);
            }
        }
    }

    repos.sort_by(|a, b| a.path.cmp(&b.path));
    debug!("Discovered {} repositories", repos.len());

    let context = if repos.len() == 1 { RepoContext::SingleRepo } else { RepoContext::Workspace };
    Ok((repos, context))
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepoContext {
    SingleRepo,  // Operating on a single repository
    Workspace,   // Operating on multiple repositories in a workspace
}
```

#### 5.2 Context-Aware Command Processing
**File**: `src/create.rs`
**Location**: Lines 161-169 (working directory determination)

**Update to use context-aware discovery:**
```rust
let current_dir = std::env::current_dir()?;
let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
let max_depth = cli.max_depth.or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth)).unwrap_or(3);

// Use context-aware discovery
let (repos, context) = discover_repos_with_context(start_dir, max_depth).context("Failed to discover repositories")?;

info!("Discovered {} repositories in {:?} context", repos.len(), context);

// Adjust behavior based on context
let filtered_repos = match context {
    RepoContext::SingleRepo => {
        // In single repo mode, ignore patterns that might exclude the only repo
        if patterns.is_empty() {
            repos
        } else {
            filter_repos(repos, patterns)
        }
    }
    RepoContext::Workspace => {
        // In workspace mode, apply patterns normally
        filter_repos(repos, patterns)
    }
};
```

#### 5.3 Consistent Branch Resolution
**File**: `src/git.rs`
**Location**: Add helper function

**Add context-aware branch resolution:**
```rust
/// Resolve branch name with context awareness
pub fn resolve_branch_with_context(
    repo: &Repo,
    branch_name: &str,
    context: &RepoContext
) -> Result<String> {
    let resolved = resolve_branch_name(repo, branch_name)?;

    // In single repo context, we can be more aggressive about branch detection
    match context {
        RepoContext::SingleRepo => {
            // Check current branch if no specific branch requested
            if branch_name == "default" || branch_name.is_empty() {
                get_current_branch_name(&repo.path)
                    .or_else(|_| get_default_branch_local(repo))
            } else {
                Ok(resolved)
            }
        }
        RepoContext::Workspace => {
            // Use standard resolution in workspace mode
            Ok(resolved)
        }
    }
}
```

## Implementation Order & Dependencies

### Phase 1: Draft PR Support (Issue #2)
- **Dependencies**: None
- **Risk**: Low
- **Testing**: Manual testing with `gh pr create --draft`

### Phase 2: Regex Feedback (Issue #3)
- **Dependencies**: None
- **Risk**: Medium (changes core diff logic)
- **Testing**: Unit tests for new `SubstitutionResult` enum

### Phase 3: Successive Commands (Issue #4)
- **Dependencies**: None
- **Risk**: Low (mostly verification of existing implementation)
- **Testing**: Integration tests with multiple command runs

### Phase 4: Backup File Cleanup (Issue #5)
- **Dependencies**: None
- **Risk**: Medium (changes transaction commit behavior)
- **Testing**: Unit tests for cleanup logic, integration tests for file system state

### Phase 5: Branch Detection Context (Issue #6)
- **Dependencies**: None
- **Risk**: Low (additive changes to repository discovery)
- **Testing**: Integration tests with different execution contexts

## Testing Strategy

### Unit Tests
```rust
// src/diff.rs
#[test]
fn test_substitution_result_no_matches() {
    let result = apply_regex_substitution("hello world", r"\d+", "X", 1);
    assert!(matches!(result.unwrap(), SubstitutionResult::NoMatches));
}

// src/git.rs
#[test]
fn test_create_branch_existing() {
    // Test branch creation when branch already exists
}
```

### Integration Tests
```rust
// tests/successive_commands.rs
#[test]
fn test_successive_gx_commands_same_branch() {
    // Run gx create -x test-branch twice
    // Verify second command succeeds
}
```

## Success Criteria

### Issue #2: Draft PR Support
- [ ] `gx create --pr --draft` creates draft PRs
- [ ] `gx create --pr=draft` creates draft PRs
- [ ] Regular `--pr` still creates normal PRs
- [ ] Draft PRs appear correctly in GitHub UI

### Issue #3: Regex Feedback
- [ ] Clear message when regex finds no matches
- [ ] Distinguish "no matches" from "dry run"
- [ ] Show match statistics in summary
- [ ] Users understand why they see "X dry runs"

### Issue #4: Successive Commands
- [ ] `gx create -x same-branch` works multiple times
- [ ] Existing branches are reused appropriately
- [ ] Multi-substitution syntax works
- [ ] Multiple file groups can have different patterns

### Issue #5: Backup File Cleanup
- [ ] `--no-backup` flag disables backup file creation
- [ ] `--keep-backups` flag preserves backup files after success
- [ ] Backup files are automatically cleaned up by default
- [ ] Configuration option controls default cleanup behavior
- [ ] No backup files left behind after successful operations

### Issue #6: Branch Detection Context
- [ ] Consistent behavior when running from workspace directory
- [ ] Consistent behavior when running from single repo directory
- [ ] Branch detection works the same regardless of execution context
- [ ] Repository discovery correctly identifies single vs multi-repo contexts

## Risk Mitigation

### High Risk Areas
1. **Git branch logic changes** - Could break existing workflows
   - *Mitigation*: Extensive testing with existing repos
   - *Rollback*: Keep original `create_branch` as fallback

2. **Core diff logic changes** - Could affect all substitutions
   - *Mitigation*: Maintain backward compatibility
   - *Rollback*: Preserve existing `Option<(String, String)>` interface

### Medium Risk Areas
1. **CLI changes** - Could break existing scripts
   - *Mitigation*: Additive changes only, no breaking changes
   - *Testing*: Verify all existing command combinations still work

## Timeline Estimate

- **Phase 1 (Draft PRs)**: 1-2 days
- **Phase 2 (Regex Feedback)**: 2-3 days
- **Phase 3 (Successive Commands)**: 1 day (verification)
- **Phase 4 (Backup Cleanup)**: 2-3 days
- **Phase 5 (Branch Context)**: 1-2 days
- **Testing & Polish**: 2-3 days

**Total Estimate**: 1.5-2.5 weeks for complete implementation

## Future Enhancements

### Beyond Initial Implementation
1. **Advanced Multi-Substitution**: YAML/JSON config for complex scenarios
2. **Branch Strategy Options**: Configure branch reuse vs. creation behavior
3. **Pattern Library**: Common regex patterns for version bumps, etc.
4. **Interactive Mode**: Preview changes before applying
5. **Undo Functionality**: Reverse applied changes

### Integration Opportunities
1. **CI Integration**: Draft PRs for automated dependency updates
2. **Workflow Templates**: Pre-defined multi-substitution workflows
3. **Git Hooks**: Validate patterns before commit
4. **IDE Integration**: VS Code extension for gx operations
