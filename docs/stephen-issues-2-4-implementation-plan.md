# Stephen's Issues 2-4: Implementation Plan

## Overview

This document outlines the detailed implementation plan to address Stephen's three remaining issues with `gx`:

- **Issue #2**: Need to be able to create PRs in Draft mode
- **Issue #3**: Better feedback when regex patterns don't match (currently just shows "X dry runs")
- **Issue #4**: Can't run successive gx commands on the same branch + multi-substitution support

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
// src/git.rs:948-975
pub fn create_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "checkout", "-b", branch_name])
        .output()  // Fails if branch exists
```

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

**Priority**: High
**Estimated Effort**: 4-6 hours
**Files to Modify**: `src/git.rs`, `src/cli.rs`, `src/create.rs`

#### 3.1 Smart Branch Handling
**File**: `src/git.rs`
**Location**: Lines 948-975 (`create_branch`)

**Add branch existence check:**
```rust
pub fn create_branch(repo_path: &std::path::Path, branch_name: &str) -> Result<()> {
    // Check if branch already exists locally
    if branch_exists_locally(repo_path, branch_name)? {
        debug!("Branch '{}' already exists, switching to it", branch_name);
        return switch_branch(repo_path, branch_name);
    }

    // Check if branch exists on remote
    if branch_exists_on_remote(repo_path, branch_name)? {
        debug!("Branch '{}' exists on remote, checking out", branch_name);
        return checkout_remote_branch(repo_path, branch_name);
    }

    // Create new branch
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "checkout", "-b", branch_name])
        .output()
        .context("Failed to execute git checkout -b")?;

    // ... rest unchanged
}

fn branch_exists_locally(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "branch", "--list", branch_name])
        .output()?;

    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn branch_exists_on_remote(repo_path: &std::path::Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "branch", "-r", "--list", &format!("origin/{}", branch_name)])
        .output()?;

    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}
```

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
- **Dependencies**: None, but benefits from Phase 2 feedback improvements
- **Risk**: High (changes git branch logic)
- **Testing**: Integration tests with multiple command runs

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
- **Phase 3 (Successive Commands)**: 3-4 days
- **Testing & Polish**: 1-2 days

**Total Estimate**: 1-2 weeks for complete implementation

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
