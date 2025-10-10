# Remote Status Redesign Plan: Git Status --porcelain --branch Approach

## Executive Summary

This document outlines the redesign of `gx status` remote status detection using Git's native `git status --porcelain --branch` command. This approach leverages Git's built-in tracking logic to provide accurate ahead/behind counts while maintaining reliability and simplicity.

## Current Problem

The existing implementation in `src/git.rs` has a fundamental flaw:
- Uses `git ls-remote` to get remote SHA (✅ good)
- Tries to count commits using `git rev-list --count local..remote` (❌ fails)
- When remote SHA doesn't exist locally, defaults to `1` (❌ misleading)
- Result: All repos show exactly `⬇️ 1` or `⬆️ 1` regardless of actual commit difference

## Solution: Git Status --porcelain --branch

### Why This Approach is Superior

1. **Uses Git's Native Logic**: Leverages the same tracking logic that `git status` uses
2. **Handles Edge Cases**: Git already handles all the complex scenarios (detached HEAD, no upstream, etc.)
3. **Simpler Implementation**: Parse one command output instead of multiple git operations
4. **More Reliable**: Less custom logic = fewer bugs
5. **Consistent with Git**: Matches what users see in `git status`

### How Git Status --porcelain --branch Works

```bash
$ git status --porcelain --branch
## main...origin/main [ahead 2, behind 5]
M  modified-file.txt
?? untracked-file.txt
```

The first line contains branch tracking information:
- `## main...origin/main` - local branch tracking remote branch
- `[ahead 2, behind 5]` - exact commit counts
- `[ahead 2]` - only ahead
- `[behind 5]` - only behind
- No bracket - up to date

## Design Architecture

### Core Components

```rust
// New enum for more precise status representation
#[derive(Debug, Clone)]
pub enum RemoteStatus {
    UpToDate,                    // No tracking info in git status
    Ahead(u32),                  // [ahead N]
    Behind(u32),                 // [behind N]
    Diverged(u32, u32),          // [ahead N, behind M]
    NoUpstream,                  // ## main (no upstream)
    DetachedHead,                // ## HEAD (no branch)
    Error(String),               // Git command failed
}

// New tracking info parser
#[derive(Debug)]
struct BranchTrackingInfo {
    local_branch: String,
    remote_branch: Option<String>,
    ahead: u32,
    behind: u32,
}
```

### Implementation Flow

```
1. Execute: git status --porcelain --branch
2. Parse first line for branch tracking info
3. Extract ahead/behind counts from [ahead X, behind Y] pattern
4. Map to RemoteStatus enum
5. Handle edge cases (no upstream, detached HEAD, etc.)
```

## Implementation Plan

### Phase 1: Core Implementation (Week 1)

#### 1.1 Create New Parser Function

```rust
/// Parse git status --porcelain --branch output for remote tracking info
fn parse_branch_tracking_info(status_output: &str) -> Result<BranchTrackingInfo> {
    // Parse first line: ## local...remote [ahead X, behind Y]
    // Handle variations:
    // - ## main...origin/main [ahead 2, behind 5]
    // - ## main...origin/main [ahead 2]
    // - ## main...origin/main [behind 5]
    // - ## main...origin/main (up to date)
    // - ## main (no upstream)
    // - ## HEAD (detached)
}
```

#### 1.2 Replace get_remote_status Function

```rust
/// Get remote tracking status using git status --porcelain --branch
fn get_remote_status_native(repo: &Repo) -> RemoteStatus {
    // Execute git status --porcelain --branch
    let output = Command::new("git")
        .args(["-C", &repo.path, "status", "--porcelain", "--branch"])
        .output()?;

    // Parse tracking info
    let tracking_info = parse_branch_tracking_info(&output_str)?;

    // Convert to RemoteStatus
    match (tracking_info.ahead, tracking_info.behind) {
        (0, 0) => RemoteStatus::UpToDate,
        (a, 0) => RemoteStatus::Ahead(a),
        (0, b) => RemoteStatus::Behind(b),
        (a, b) => RemoteStatus::Diverged(a, b),
    }
}
```

#### 1.3 Update Integration Points

- Modify `get_repo_status()` to use new function
- Update output formatting to handle new status types
- Ensure emoji display logic works with new enum variants

### Phase 2: Enhanced Accuracy (Week 2)

#### 2.1 Add Smart Fetch Option

```rust
/// Enhanced remote status with optional fetch
fn get_remote_status_with_fetch(repo: &Repo, fetch_first: bool) -> RemoteStatus {
    if fetch_first {
        // Perform lightweight fetch to update tracking refs
        let _ = Command::new("git")
            .args(["-C", &repo.path, "fetch", "--quiet"])
            .output();
    }

    get_remote_status_native(repo)
}
```

#### 2.2 Add CLI Options

```rust
// In cli.rs Status command
#[arg(long, help = "Fetch latest remote refs before status check")]
fetch_first: bool,

#[arg(long, help = "Skip remote status checks entirely")]
no_remote: bool,
```

#### 2.3 Configuration Support

```yaml
# gx.yml
remote_status:
  enabled: true
  fetch_first: false
  timeout_seconds: 10
```

### Phase 3: Testing & Validation (Week 3)

#### 3.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_ahead_behind() {
        let output = "## main...origin/main [ahead 2, behind 5]\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.ahead, 2);
        assert_eq!(info.behind, 5);
    }

    #[test]
    fn test_parse_ahead_only() {
        let output = "## main...origin/main [ahead 3]\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert_eq!(info.ahead, 3);
        assert_eq!(info.behind, 0);
    }

    #[test]
    fn test_parse_no_upstream() {
        let output = "## main\n";
        let info = parse_branch_tracking_info(output).unwrap();
        assert!(info.remote_branch.is_none());
    }
}
```

#### 3.2 Integration Tests

```rust
#[test]
fn test_remote_status_accuracy() {
    let test_repo = create_test_repo_with_commits();

    // Create known ahead/behind scenario
    setup_repo_ahead_behind(&test_repo, 3, 7);

    let status = get_remote_status_native(&test_repo);
    assert_eq!(status, RemoteStatus::Diverged(3, 7));
}
```

#### 3.3 Performance Testing

- Benchmark against current implementation
- Test with large numbers of repositories
- Measure impact of fetch operations

### Phase 4: Documentation & Rollout (Week 4)

#### 4.1 Update Documentation

- Update CLI help text
- Update README with accurate remote status info
- Document new configuration options
- Add troubleshooting guide

#### 4.2 Migration Strategy

- Keep old implementation as fallback
- Add feature flag for gradual rollout
- Monitor for regressions

## Technical Specifications

### Git Status Output Parsing

#### Standard Format Patterns

```bash
# Up to date
## main...origin/main

# Ahead only
## main...origin/main [ahead 3]

# Behind only
## main...origin/main [behind 7]

# Diverged
## main...origin/main [ahead 2, behind 5]

# No upstream
## main

# Detached HEAD
## HEAD (no branch)

# Different remote name
## main...upstream/main [behind 1]
```

#### Regex Pattern

```rust
const BRANCH_TRACKING_REGEX: &str = r"^## (?P<local>\S+)(?:\.\.\.(?P<remote>\S+))?(?: \[(?P<tracking>.*)\])?";
const TRACKING_REGEX: &str = r"(?:ahead (?P<ahead>\d+))?(?:, )?(?:behind (?P<behind>\d+))?";
```

### Error Handling Strategy

```rust
fn get_remote_status_native(repo: &Repo) -> RemoteStatus {
    match execute_git_status(repo) {
        Ok(output) => {
            match parse_branch_tracking_info(&output) {
                Ok(info) => convert_to_remote_status(info),
                Err(parse_error) => {
                    log::warn!("Failed to parse git status for {}: {}", repo.name, parse_error);
                    RemoteStatus::Error("Parse failed".to_string())
                }
            }
        }
        Err(git_error) => {
            log::warn!("Git status failed for {}: {}", repo.name, git_error);
            RemoteStatus::Error("Git command failed".to_string())
        }
    }
}
```

### Performance Considerations

#### Current vs New Performance

| Aspect | Current | New (no fetch) | New (with fetch) |
|--------|---------|----------------|------------------|
| Network calls | 1 per repo (ls-remote) | 0 | 1 per repo (fetch) |
| Git commands | 3-4 per repo | 1 per repo | 2 per repo |
| Accuracy | Poor (always "1") | Good (if refs current) | Excellent |
| Speed | Medium | Fast | Medium |

#### Optimization Strategies

1. **Parallel Execution**: Already implemented with rayon
2. **Batch Fetch**: `git fetch --multiple origin1 origin2 ...`
3. **Selective Fetch**: Only fetch when status shows difference
4. **Caching**: Cache fetch results for short periods

## Edge Cases & Handling

### Repository States

| State | Git Status Output | RemoteStatus Result |
|-------|------------------|-------------------|
| Clean, up to date | `## main...origin/main` | `UpToDate` |
| Dirty, up to date | `## main...origin/main\nM file.txt` | `UpToDate` |
| Behind | `## main...origin/main [behind 5]` | `Behind(5)` |
| Ahead | `## main...origin/main [ahead 3]` | `Ahead(3)` |
| Diverged | `## main...origin/main [ahead 2, behind 7]` | `Diverged(2, 7)` |
| No upstream | `## main` | `NoUpstream` |
| Detached HEAD | `## HEAD (no branch)` | `DetachedHead` |
| No git repo | Command fails | `Error("Not a git repo")` |

### Network & Authentication Issues

```rust
fn handle_fetch_errors(error: &std::io::Error) -> RemoteStatus {
    let error_msg = error.to_string().to_lowercase();

    if error_msg.contains("timeout") {
        RemoteStatus::Error("Network timeout".to_string())
    } else if error_msg.contains("authentication") || error_msg.contains("permission") {
        RemoteStatus::Error("Authentication failed".to_string())
    } else if error_msg.contains("network") || error_msg.contains("connection") {
        RemoteStatus::Error("Network error".to_string())
    } else {
        RemoteStatus::Error("Fetch failed".to_string())
    }
}
```

## Benefits of This Approach

### 1. Accuracy
- ✅ Provides exact commit counts (when refs are current)
- ✅ Handles all Git edge cases correctly
- ✅ Consistent with native Git behavior

### 2. Reliability
- ✅ Uses Git's battle-tested tracking logic
- ✅ Less custom code = fewer bugs
- ✅ Handles complex scenarios (rebases, force pushes, etc.)

### 3. Performance
- ✅ Single git command per repo (without fetch)
- ✅ No network calls required (uses local tracking refs)
- ✅ Parallel execution already implemented

### 4. User Experience
- ✅ Matches user expectations from `git status`
- ✅ Clear, actionable information
- ✅ Optional fetch for guaranteed accuracy

### 5. Maintainability
- ✅ Simpler implementation
- ✅ Leverages Git's existing functionality
- ✅ Easier to test and debug

## Migration & Rollback Plan

### Feature Flag Implementation

```rust
// In config.rs
#[derive(Debug, Deserialize)]
pub struct RemoteStatusConfig {
    pub use_native_git_status: bool,  // Feature flag
    pub fetch_first: bool,
    pub timeout_seconds: u32,
}

// In git.rs
fn get_remote_status(repo: &Repo, config: &RemoteStatusConfig) -> RemoteStatus {
    if config.use_native_git_status {
        get_remote_status_native(repo)
    } else {
        get_remote_status_legacy(repo)  // Current implementation
    }
}
```

### Gradual Rollout Strategy

1. **Week 1**: Implement with feature flag disabled by default
2. **Week 2**: Enable for internal testing
3. **Week 3**: Enable by default, keep legacy as fallback
4. **Week 4**: Remove legacy implementation if no issues

### Rollback Triggers

- Performance regression > 50%
- Accuracy issues in common scenarios
- User reports of incorrect status
- Network timeout issues

## Success Metrics

### Accuracy Metrics
- ✅ 0% of repos showing misleading "1" count
- ✅ >95% accuracy in ahead/behind detection
- ✅ Correct handling of edge cases (no upstream, detached HEAD)

### Performance Metrics
- ✅ <2x slowdown compared to current (without fetch)
- ✅ <5s total execution time for 100 repos (with fetch)
- ✅ Graceful handling of network timeouts

### User Experience Metrics
- ✅ Clear, actionable status information
- ✅ Consistent with Git's native behavior
- ✅ Reduced user confusion about repo status

## Conclusion

The `git status --porcelain --branch` approach provides the best balance of accuracy, reliability, and maintainability for remote status detection in `gx`. By leveraging Git's native tracking logic, we eliminate the current issues with misleading commit counts while providing a more robust and user-friendly experience.

This implementation will transform `gx status` from a tool that provides misleading information to one that gives users accurate, actionable insights about their repository states.
