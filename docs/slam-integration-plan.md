# SLAM Integration Plan: Create and Review Subcommands for GX

## Executive Summary

This document outlines the integration plan for incorporating SLAM's `create` and `review` subcommands into the GX codebase. After a thorough analysis of the SLAM codebase, this plan provides a comprehensive strategy for adapting SLAM's powerful bulk repository management capabilities to fit GX's architectural patterns and design philosophy.

## Table of Contents

1. [Code Review Summary](#code-review-summary)
2. [Architecture Analysis](#architecture-analysis)
3. [Integration Strategy](#integration-strategy)
4. [Implementation Plan](#implementation-plan)
5. [File Structure Changes](#file-structure-changes)
6. [API Design](#api-design)
7. [Migration Path](#migration-path)
8. [Testing Strategy](#testing-strategy)
9. [Risk Assessment](#risk-assessment)

## Code Review Summary

### SLAM Codebase Analysis

#### Strengths
- **Robust Error Handling**: SLAM uses comprehensive error handling with `eyre::Result` throughout
- **Transaction-Based Operations**: Implements rollback mechanisms for safe multi-step git operations
- **Parallel Processing**: Uses `rayon` for parallel repository operations
- **Rich Diff Generation**: Sophisticated diff generation with colored output using `similar` crate
- **Comprehensive Git Operations**: Full git workflow support including stashing, branching, PR creation
- **GitHub Integration**: Deep integration with GitHub CLI for PR management
- **Flexible File Matching**: Glob-based file pattern matching within repositories
- **Four-Level Filtering**: Sophisticated repository filtering logic (exact name, starts-with name, exact slug, starts-with slug)

#### Key Components Analyzed

##### 1. Create Subcommand (`slam create`)
- **Purpose**: Apply changes across multiple repositories and create PRs
- **Change Types**:
  - `Add(path, content)` - Create new files
  - `Delete` - Remove matching files
  - `Sub(pattern, replacement)` - String substitution
  - `Regex(pattern, replacement)` - Regex-based replacement
- **Workflow**: Discovery → Filtering → Change Application → Git Operations → PR Creation
- **Transaction Support**: Full rollback capability for failed operations

##### 2. Review Subcommand (`slam review`)
- **Purpose**: Manage PRs across multiple repositories
- **Actions**:
  - `ls` - List PRs by change ID
  - `clone` - Clone repos with specific PRs
  - `approve` - Approve and merge PRs
  - `delete` - Delete PRs and branches
  - `purge` - Clean up all SLAM-related PRs and branches
- **GitHub Integration**: Heavy use of `gh` CLI for PR operations

##### 3. Core Infrastructure
- **Transaction System**: Rollback-capable operation chains
- **Diff Engine**: Advanced diff generation with context and coloring
- **Git Wrapper**: Comprehensive git command abstraction
- **Repository Discovery**: Recursive git repository finding
- **File Processing**: Pattern-based file discovery and modification

### Architectural Patterns

#### SLAM's Architecture
```
CLI → Process Functions → Repo Operations → Git/GitHub Commands
  ↓                      ↓
Config                 Transaction System
  ↓                      ↓
Logging               Diff Engine
```

#### GX's Architecture
```
CLI → Command Processors → Git Operations → Output Formatting
  ↓                       ↓
Config                   Parallel Processing
  ↓                       ↓
Logging                 Status/Result Types
```

## Integration Strategy

### Design Philosophy Alignment

#### Similarities
- Both use structured error handling with `eyre`
- Both employ parallel processing patterns
- Both have comprehensive CLI interfaces with `clap`
- Both use logging for debugging and operation tracking
- Both implement repository discovery and filtering

#### Differences
- **Output Patterns**: GX uses unified output formatting; SLAM uses inline printing
- **Result Types**: GX uses structured result types; SLAM uses direct output
- **Transaction Model**: SLAM has explicit transactions; GX uses simpler error propagation
- **GitHub Integration**: SLAM heavily uses GitHub CLI; GX has minimal GitHub integration
- **Pre-commit Integration**: SLAM automatically runs pre-commit hooks; GX will not include this behavior

### Integration Approach: Hybrid Architecture

We will adopt a **hybrid approach** that preserves GX's architectural patterns while incorporating SLAM's powerful capabilities:

1. **Preserve GX's Output System**: Adapt SLAM's functionality to use GX's `UnifiedDisplay` trait and output formatting
2. **Adopt SLAM's Transaction System**: Integrate SLAM's rollback capabilities for safe operations
3. **Enhance GX's GitHub Integration**: Add GitHub CLI integration following GX's patterns
4. **Maintain GX's Parallel Processing**: Use GX's existing parallel patterns with SLAM's operations
5. **Extend GX's Configuration**: Add SLAM-specific configuration options to GX's config system
6. **Exclude Automatic Pre-commit**: GX will not automatically run pre-commit hooks during create operations

## Implementation Plan

### Phase 1: Core Infrastructure

#### 1.1 Transaction System Integration
- **File**: `src/transaction.rs` (new)
- **Purpose**: Port SLAM's transaction system to GX
- **Changes**:
  - Adapt SLAM's `Transaction` struct to GX patterns
  - Integrate with GX's error handling
  - Add GX-specific rollback operations

#### 1.2 Diff Engine Integration
- **File**: `src/diff.rs` (new)
- **Purpose**: Port SLAM's diff generation capabilities
- **Changes**:
  - Adapt SLAM's diff generation to GX's output patterns
  - Integrate with GX's color/emoji configuration
  - Support GX's unified display format

#### 1.3 Enhanced Git Operations
- **File**: `src/git.rs` (extend existing)
- **Purpose**: Add SLAM's git operations to GX
- **Changes**:
  - Add branch management functions
  - Add stashing operations
  - Add commit and push operations
  - Integrate with transaction system
- **Excluded**: Pre-commit hook execution functions (deliberate design decision)

### Phase 2: Create Subcommand

#### 2.1 Change Engine
- **File**: `src/create.rs` (new)
- **Purpose**: Implement SLAM's change application logic
- **Key Components**:
```rust
#[derive(Debug, Clone)]
pub enum Change {
    Add(String, String),    // path, content
    Delete,                 // delete matched files
    Sub(String, String),    // pattern, replacement
    Regex(String, String),  // regex pattern, replacement
}

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub repo: Repo,
    pub change_id: String,
    pub action: CreateAction,
    pub files_affected: Vec<String>,
    pub diff_summary: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CreateAction {
    DryRun,           // No changes made (preview)
    Applied,          // Changes applied successfully
    Committed,        // Changes committed to branch
    PrCreated,        // PR created successfully
}
```

#### 2.2 File Processing
- **File**: `src/file_ops.rs` (new)
- **Purpose**: Handle file discovery and modification
- **Features**:
  - Glob pattern matching
  - File content modification
  - Backup and rollback support
  - Integration with transaction system

#### 2.3 CLI Integration
- **File**: `src/cli.rs` (extend existing)
- **Purpose**: Add create subcommand to GX CLI
- **Command Structure**:
```rust
#[command(after_help = "CREATE LEGEND:
  📝  Files modified        ➕  Files added         ❌  Files deleted
  🔄  Branch created        📥  PR created          📊  Summary stats

EXAMPLES:
  gx create --files '*.json' --add config.json '{\"debug\": true}'
  gx create --files '*.md' --sub 'old-text' 'new-text' --commit
  gx create --files 'package.json' --regex '\"version\": \"[^\"]+\"' '\"version\": \"1.2.3\"'")]
Create {
    /// Files to target (glob patterns)
    #[arg(short, long, help = "File patterns to match")]
    files: Vec<String>,

    /// Change ID for branch and PR naming
    #[arg(short = 'x', long, help = "Change ID for branch/PR")]
    change_id: Option<String>,

    /// Repository patterns to filter
    #[arg(short = 'p', long, help = "Repository patterns to filter")]
    patterns: Vec<String>,

    /// Commit changes with message
    #[arg(short = 'c', long, help = "Commit changes with message")]
    commit: Option<String>,

    /// Create PR after committing
    #[arg(long, help = "Create pull request after committing")]
    pr: bool,

    #[command(subcommand)]
    action: CreateAction,
}

#[derive(Subcommand, Debug)]
pub enum CreateAction {
    /// Add new files
    Add {
        #[arg(help = "File path to create")]
        path: String,
        #[arg(help = "File content")]
        content: String,
    },
    /// Delete matching files
    Delete,
    /// String substitution
    Sub {
        #[arg(help = "Pattern to find")]
        pattern: String,
        #[arg(help = "Replacement text")]
        replacement: String,
    },
    /// Regex substitution
    Regex {
        #[arg(help = "Regex pattern to find")]
        pattern: String,
        #[arg(help = "Replacement text")]
        replacement: String,
    },
}
```

### Phase 3: Review Subcommand

#### 3.1 GitHub Integration
- **File**: `src/github.rs` (extend existing)
- **Purpose**: Add GitHub CLI integration for PR management
- **Key Functions**:
```rust
pub fn list_prs_by_change_id(org: &str, change_id: &str) -> Result<Vec<PrInfo>>;
pub fn approve_pr(repo_slug: &str, pr_number: u64) -> Result<()>;
pub fn merge_pr(repo_slug: &str, pr_number: u64, admin_override: bool) -> Result<()>;
pub fn close_pr(repo_slug: &str, pr_number: u64) -> Result<()>;
pub fn delete_branch(repo_slug: &str, branch: &str) -> Result<()>;
pub fn get_pr_diff(repo_slug: &str, pr_number: u64) -> Result<String>;
```

#### 3.2 Review Operations
- **File**: `src/review.rs` (new)
- **Purpose**: Implement PR management operations
- **Key Components**:
```rust
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub repo: Repo,
    pub change_id: String,
    pub pr_number: Option<u64>,
    pub action: ReviewAction,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ReviewAction {
    Listed,           // PR information displayed
    Cloned,           // Repository cloned/updated
    Approved,         // PR approved and merged
    Deleted,          // PR closed and branch deleted
    Purged,           // All SLAM branches cleaned up
}
```

#### 3.3 CLI Integration
- **File**: `src/cli.rs` (extend existing)
- **Purpose**: Add review subcommand to GX CLI
- **Command Structure**:
```rust
#[command(after_help = "REVIEW LEGEND:
  📋  PR listed             📥  Repository cloned   ✅  PR approved
  ❌  PR deleted            🧹  Repository purged   📊  Summary stats

EXAMPLES:
  gx review ls SLAM-2024-01-15    # List PRs for change ID
  gx review clone SLAM-2024-01-15 # Clone repos with PRs
  gx review approve SLAM-2024-01-15 --admin  # Approve and merge PRs
  gx review delete SLAM-2024-01-15 # Delete PRs and branches
  gx review purge -o tatari-tv     # Clean up all SLAM branches")]
Review {
    /// GitHub organization
    #[arg(short = 'o', long, help = "GitHub organization")]
    org: String,

    /// Repository patterns to filter
    #[arg(short = 'p', long, help = "Repository patterns to filter")]
    patterns: Vec<String>,

    #[command(subcommand)]
    action: ReviewAction,
}

#[derive(Subcommand, Debug)]
pub enum ReviewAction {
    /// List PRs by change ID
    Ls {
        #[arg(help = "Change ID patterns to match")]
        change_ids: Vec<String>,
    },
    /// Clone repositories with PRs
    Clone {
        #[arg(help = "Change ID to clone")]
        change_id: String,
        #[arg(short, long, help = "Include closed PRs")]
        all: bool,
    },
    /// Approve and merge PRs
    Approve {
        #[arg(help = "Change ID to approve")]
        change_id: String,
        #[arg(long, help = "Use admin override for merge")]
        admin: bool,
    },
    /// Delete PRs and branches
    Delete {
        #[arg(help = "Change ID to delete")]
        change_id: String,
    },
    /// Purge all SLAM branches and PRs
    Purge,
}
```

### Phase 4: Output Integration

#### 4.1 Unified Display Implementation
- **File**: `src/output.rs` (extend existing)
- **Purpose**: Add UnifiedDisplay implementations for create/review results
- **Changes**:
```rust
impl UnifiedDisplay for CreateResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.change_id)
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        match (&self.error, &self.action) {
            (Some(_), _) => if opts.use_emoji { "❌" } else { "ERROR" }.to_string(),
            (None, CreateAction::DryRun) => if opts.use_emoji { "👁️" } else { "DRY" }.to_string(),
            (None, CreateAction::Applied) => if opts.use_emoji { "📝" } else { "MOD" }.to_string(),
            (None, CreateAction::Committed) => if opts.use_emoji { "💾" } else { "COMMIT" }.to_string(),
            (None, CreateAction::PrCreated) => if opts.use_emoji { "📥" } else { "PR" }.to_string(),
        }
    }

    // ... other implementations
}

impl UnifiedDisplay for ReviewResult {
    // Similar implementation for review results
}
```

#### 4.2 Summary Display
- **Purpose**: Add summary displays matching GX patterns
- **Features**:
  - Count of successful/failed operations
  - File modification statistics
  - PR creation/management summaries

### Phase 5: Configuration Integration

#### 5.1 Configuration Extensions
- **File**: `src/config.rs` (extend existing)
- **Purpose**: Add SLAM-specific configuration options
- **New Sections**:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct CreateConfig {
    pub default_change_id_format: Option<String>,
    pub auto_commit_message: Option<String>,
    pub auto_create_pr: Option<bool>,
    pub max_file_size: Option<usize>,
    pub excluded_patterns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ReviewConfig {
    pub default_org: Option<String>,
    pub auto_approve_criteria: Option<Vec<String>>,
    pub require_admin_override: Option<bool>,
    pub purge_confirmation: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GitHubConfig {
    pub token_command: Option<String>,
    pub api_base_url: Option<String>,
    pub cli_path: Option<String>,
}
```

## File Structure Changes

### New Files
```
src/
├── create.rs           # Create subcommand implementation
├── review.rs           # Review subcommand implementation
├── transaction.rs      # Transaction/rollback system
├── diff.rs            # Diff generation and display
├── file_ops.rs        # File pattern matching and modification
└── changes.rs         # Change type definitions and processing
```

### Extended Files
```
src/
├── cli.rs             # Add create/review subcommands
├── output.rs          # Add UnifiedDisplay for new result types
├── git.rs             # Add branch/commit/push operations
├── github.rs          # Add PR management operations
├── config.rs          # Add create/review configuration sections
└── main.rs            # Add new subcommand routing
```

### Documentation Files
```
docs/
├── slam-integration-plan.md    # This document
├── create-subcommand.md        # Create subcommand documentation
├── review-subcommand.md        # Review subcommand documentation
└── migration-from-slam.md      # Migration guide for SLAM users
```

### Future Considerations
```
src/
└── pre_commit.rs             # Future: Separate pre-commit subcommand
                              # (NOT part of this integration plan)
```

## API Design

### Core Types

#### Change System
```rust
// Core change types
pub enum Change {
    Add(String, String),
    Delete,
    Sub(String, String),
    Regex(String, String),
}

// Change application context
pub struct ChangeContext {
    pub change_id: String,
    pub files: Vec<String>,
    pub commit_message: Option<String>,
    pub create_pr: bool,
    pub dry_run: bool,
}

// File processing result
pub struct FileProcessResult {
    pub path: PathBuf,
    pub action: FileAction,
    pub diff: Option<String>,
    pub error: Option<String>,
}
```

#### GitHub Integration
```rust
// PR information
pub struct PrInfo {
    pub repo_slug: String,
    pub number: u64,
    pub title: String,
    pub branch: String,
    pub author: String,
    pub state: PrState,
}

// PR management operations
pub trait PrManager {
    fn list_prs(&self, org: &str, change_id_pattern: &str) -> Result<Vec<PrInfo>>;
    fn approve_pr(&self, repo_slug: &str, pr_number: u64) -> Result<()>;
    fn merge_pr(&self, repo_slug: &str, pr_number: u64, admin: bool) -> Result<()>;
    fn close_pr(&self, repo_slug: &str, pr_number: u64) -> Result<()>;
}
```

### Command Processing Flow

#### Create Command Flow
```
1. Parse CLI arguments
2. Discover repositories (using existing GX logic)
3. Filter repositories (using GX's 4-level filtering)
4. For each repository:
   a. Start transaction
   b. Find matching files
   c. Apply changes (with diff generation)
   d. Commit if requested (WITHOUT running pre-commit hooks)
   e. Create PR if requested
   f. Commit transaction or rollback on error
5. Display unified results
6. Show summary statistics

Note: Pre-commit hooks are NOT automatically executed during this flow,
unlike SLAM's behavior. Users can run hooks separately via a future
'gx pre-commit' subcommand if needed.
```

#### Review Command Flow
```
1. Parse CLI arguments
2. Query GitHub for PRs (using change ID patterns)
3. Filter repositories if patterns provided
4. For each matched PR/repository:
   a. Execute requested action (ls/clone/approve/delete/purge)
   b. Handle GitHub API operations
   c. Update local repositories if needed
5. Display unified results
6. Show summary statistics
```

## Migration Path

### For Existing SLAM Users

#### Command Mapping
```bash
# SLAM create commands
slam create -f "*.json" sub "old" "new" -c "Update config"
# becomes
gx create --files "*.json" sub "old" "new" --commit "Update config"

# SLAM review commands
slam review -o tatari-tv ls SLAM-2024-01-15
# becomes
gx review --org tatari-tv ls SLAM-2024-01-15
```

#### Configuration Migration
- Provide migration script to convert SLAM config to GX config
- Document configuration differences and new options
- Provide compatibility layer for common SLAM patterns

#### Workflow Migration
- Document how SLAM workflows translate to GX workflows
- Provide examples for common use cases
- Create migration guide with before/after examples

### Compatibility Considerations

#### Preserved Features
- Four-level repository filtering
- Transaction-based rollback
- Parallel processing
- Comprehensive diff generation
- GitHub PR management
- File pattern matching

#### Enhanced Features
- Unified output formatting (consistent with GX patterns)
- Better error handling and reporting
- Integration with GX's configuration system
- Consistent CLI patterns across all subcommands

#### Excluded Features
- **Automatic Pre-commit Hook Execution**: SLAM automatically runs `pre-commit` hooks during create operations after applying changes but before committing. This behavior will NOT be ported to GX for the following reasons:
  - **Separation of Concerns**: Pre-commit hooks should be a separate, explicit operation
  - **Performance**: Automatic hook execution can significantly slow down bulk operations
  - **User Control**: Users should have explicit control over when hooks are executed
  - **Flexibility**: Some workflows may need to apply changes without running hooks immediately
  - **Alternative**: A separate `gx pre-commit` subcommand can be implemented for explicit hook management

## Testing Strategy

### Unit Tests
- **Transaction System**: Test rollback scenarios
- **Change Application**: Test each change type with various inputs
- **File Operations**: Test pattern matching and file modification
- **GitHub Integration**: Mock GitHub CLI operations
- **Diff Generation**: Test diff output formatting

### Integration Tests
- **End-to-End Workflows**: Test complete create/review workflows
- **Repository Discovery**: Test with various repository layouts
- **Parallel Processing**: Test concurrent operations
- **Error Scenarios**: Test error handling and recovery

### Performance Tests
- **Large Repository Sets**: Test with hundreds of repositories
- **Large File Operations**: Test with large files and many changes
- **Parallel Efficiency**: Measure performance improvements

### Compatibility Tests
- **SLAM Migration**: Test migration from SLAM configurations
- **GitHub Integration**: Test with various GitHub setups
- **Cross-Platform**: Test on different operating systems

## Risk Assessment

### High-Risk Areas

#### 1. GitHub API Rate Limits
- **Risk**: Excessive GitHub API usage during review operations
- **Mitigation**: Implement rate limiting and caching
- **Monitoring**: Add logging for API usage patterns

#### 2. Transaction Rollback Complexity
- **Risk**: Incomplete rollbacks leaving repositories in inconsistent state
- **Mitigation**: Comprehensive testing of rollback scenarios
- **Monitoring**: Add detailed transaction logging

#### 3. Parallel Operation Conflicts
- **Risk**: Concurrent operations on same repository causing conflicts
- **Mitigation**: Repository-level locking during operations
- **Monitoring**: Add conflict detection and reporting

### Medium-Risk Areas

#### 4. File Pattern Matching Performance
- **Risk**: Slow performance with complex glob patterns on large repositories
- **Mitigation**: Optimize pattern matching algorithms
- **Monitoring**: Add performance metrics for file operations

#### 5. Diff Generation Memory Usage
- **Risk**: High memory usage when generating diffs for large files
- **Mitigation**: Implement streaming diff generation
- **Monitoring**: Add memory usage tracking

### Low-Risk Areas

#### 6. CLI Interface Changes
- **Risk**: Breaking changes to existing GX CLI patterns
- **Mitigation**: Careful design review and backward compatibility
- **Monitoring**: User feedback collection

#### 7. Configuration Migration
- **Risk**: Complex migration from SLAM configurations
- **Mitigation**: Comprehensive migration tools and documentation
- **Monitoring**: Migration success tracking

## Success Metrics

### Functionality Metrics
- [ ] All SLAM create operations supported
- [ ] All SLAM review operations supported
- [ ] Transaction rollback success rate > 99%
- [ ] GitHub integration reliability > 95%

### Performance Metrics
- [ ] Create operations complete within 2x SLAM performance
- [ ] Review operations complete within 1.5x SLAM performance
- [ ] Memory usage stays within 150% of current GX usage
- [ ] Parallel efficiency maintains GX standards

### User Experience Metrics
- [ ] CLI interface consistency with existing GX commands
- [ ] Error messages are clear and actionable
- [ ] Output formatting matches GX patterns
- [ ] Documentation completeness score > 90%

### Integration Metrics
- [ ] Configuration migration success rate > 95%
- [ ] Existing GX functionality unaffected
- [ ] Test coverage > 85% for new functionality
- [ ] No regression in existing GX performance

## Conclusion

This integration plan provides a comprehensive strategy for incorporating SLAM's powerful create and review capabilities into GX while maintaining GX's architectural integrity and user experience standards. The phased approach allows for incremental development and testing, reducing risk while ensuring a high-quality integration.

The hybrid architecture approach preserves the best aspects of both systems: SLAM's robust transaction system and comprehensive git operations, combined with GX's unified output formatting and consistent CLI patterns. This integration will significantly enhance GX's capabilities while maintaining its design philosophy and user experience.

Key success factors:
1. **Incremental Implementation**: Phased approach reduces risk and allows for course correction
2. **Architectural Consistency**: Maintains GX patterns while incorporating SLAM capabilities
3. **Comprehensive Testing**: Ensures reliability and performance standards
4. **User Migration Support**: Provides clear path for SLAM users to adopt GX
5. **Performance Monitoring**: Ensures integration doesn't degrade existing functionality

The result will be a unified tool that combines the best of both SLAM and GX, providing users with powerful bulk repository management capabilities within GX's consistent and user-friendly interface.
