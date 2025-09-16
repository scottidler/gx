# Issue #6: Branch Detection Consistency - Specification & Design

## Problem Statement

Stephen reported that `gx` behaves differently when running from a workspace directory versus a single repository directory. The core issue is inconsistent branch detection and repository discovery behavior based on execution context.

## Root Cause Analysis

The current implementation has several issues:

1. **Repository Discovery Inconsistency**: The discovery logic doesn't handle all execution contexts uniformly
2. **User/Org Detection Flaws**: Current user/org detection assumes a single top-level value rather than per-repo detection
3. **Branch Detection Context Sensitivity**: Branch detection behavior varies based on where the command is executed

## Design Principles

### 1. **Uniform Repository Discovery**
- Repository discovery should work identically regardless of execution directory
- Whether run from workspace root, subdirectory, or single repo - same logic applies
- Recursive discovery should find all repositories within the specified depth

### 2. **Per-Repository User/Org Detection**
- Each repository has its own user/org derived from its `.git/config`
- No global/workspace-level user/org assumptions
- Support mixed user/org scenarios within a single workspace

### 3. **Consistent Branch Resolution**
- Branch detection logic should be identical across all execution contexts
- Default branch resolution should work the same way everywhere

## Technical Specification

### Repository Discovery Enhancement

#### Current State
```rust
// Current discovery in src/repo.rs
pub fn discover_repos(start_dir: &Path, max_depth: usize) -> Result<Vec<Repo>> {
    // Walks directories looking for .git folders
    // Returns Vec<Repo> with basic path/name/slug info
}
```

#### Enhanced Design
```rust
// Enhanced repository structure
#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub slug: String,           // Always determinable from git config or defaults
    pub user_org: UserOrg,      // NEW: Per-repo user/org (always present)
    pub remote_info: RemoteInfo, // NEW: Enhanced remote information
}

#[derive(Debug, Clone)]
pub struct UserOrg {
    pub user: String,           // GitHub account (user or org)
    pub org: Option<String>,    // Reserved for future enterprise GitHub distinction
}

#[derive(Debug, Clone)]
pub struct RemoteInfo {
    pub origin_url: String,     // Always present (fallback to "unknown" if needed)
    pub upstream_url: Option<String>, // Optional upstream remote
    pub ssh_url: Option<String>,      // Derived SSH URL if applicable
    pub https_url: Option<String>,    // Derived HTTPS URL if applicable
}
```

#### Discovery Algorithm
1. **Walk Directory Tree**: Use existing walkdir logic with max_depth
2. **Identify Git Repositories**: Find all `.git` directories
3. **Extract Repository Information**: For each repository:
   - Read `.git/config` to extract remote information
   - Parse remote URLs to determine user/org
   - Determine repository slug from remote
   - Extract default branch information

### Per-Repository User/Org Detection

#### Implementation Strategy
```rust
// New function in src/repo.rs
impl Repo {
    pub fn detect_user_org(&self) -> Result<UserOrg> {
        let config_path = self.path.join(".git").join("config");
        let config_content = fs::read_to_string(config_path)?;

        // Parse git config to extract remote.origin.url
        let remote_url = extract_remote_url(&config_content, "origin")?;

        if let Some(url) = remote_url {
            parse_user_org_from_url(&url)
        } else {
            // PANIC: If we can't extract user/org from git config, something is fundamentally wrong
            panic!("Repository at {} has no remote origin configured - cannot determine user/org", self.path.display())
        }
    }
}

// Enhanced URL parsing - always returns a UserOrg (never fails)
fn parse_user_org_from_url(url: &str) -> Result<UserOrg> {
    // Handle both SSH and HTTPS URLs
    // git@github.com:user/repo.git -> UserOrg { user: "user", org: None }
    // git@github.com:org/repo.git -> UserOrg { user: "org", org: None }
    // https://github.com/org/repo.git -> UserOrg { user: "org", org: None }

    // For enterprise GitHub:
    // git@github.company.com:org/repo.git -> UserOrg { user: "org", org: None }

    // Note: GitHub doesn't distinguish user vs org in URL structure
    // The "user" field represents the account (which could be user or org)
    // If parsing fails, PANIC - we should never have malformed git remotes
}
```

#### User vs Organization Distinction
Since GitHub URLs don't distinguish between user and organization accounts, we'll use a pragmatic approach:
- The `user` field represents the GitHub account (could be user or org) - **always present**
- The `org` field remains `None` for standard GitHub repositories
- Future enhancement could use GitHub API to distinguish user vs org accounts
- **No Optional types**: Every repository gets a UserOrg struct, program panics if parsing fails

### Branch Detection Consistency

#### Current Issues
- Branch resolution might behave differently based on execution directory
- Default branch detection inconsistent across contexts

#### Solution
```rust
// Enhanced branch resolution in src/git.rs
pub fn resolve_branch_name(repo: &Repo, branch_name: &str) -> Result<String> {
    if branch_name == "default" {
        get_default_branch_for_repo(repo)
    } else {
        Ok(branch_name.to_string())
    }
}

fn get_default_branch_for_repo(repo: &Repo) -> Result<String> {
    // Always use the repository's own git directory
    // This ensures consistent behavior regardless of execution context

    // 1. Try to get default branch from remote HEAD
    if let Ok(branch) = get_remote_default_branch(&repo.path) {
        return Ok(branch);
    }

    // 2. Fall back to common default branches
    for candidate in &["main", "master", "develop"] {
        if branch_exists_locally(&repo.path, candidate)? {
            return Ok(candidate.to_string());
        }
    }

    // 3. Use current branch as last resort
    get_current_branch_name(&repo.path)
}
```

## Implementation Plan

### Phase 1: Enhanced Repository Structure
**Files to modify**: `src/repo.rs`
**Estimated effort**: 2-3 hours

1. **Extend Repo struct** with `user_org` and `remote_info` fields
2. **Implement user/org detection** from git config parsing
3. **Add comprehensive URL parsing** for various git remote formats
4. **Update repository discovery** to populate new fields

### Phase 2: Git Config Parsing
**Files to modify**: `src/repo.rs`, `src/git.rs`
**Estimated effort**: 2-3 hours

1. **Implement git config parser** to extract remote URLs
2. **Add support for multiple remotes** (origin, upstream)
3. **Handle various URL formats** (SSH, HTTPS, enterprise GitHub)
4. **Add error handling** for malformed configs

### Phase 3: Branch Detection Consistency
**Files to modify**: `src/git.rs`
**Estimated effort**: 1-2 hours

1. **Standardize branch resolution** to always use repo-specific git operations
2. **Enhance default branch detection** with multiple fallback strategies
3. **Ensure consistent behavior** regardless of execution directory

### Phase 4: Command Integration
**Files to modify**: `src/create.rs`, `src/status.rs`, `src/checkout.rs`
**Estimated effort**: 1-2 hours

1. **Update commands** to use enhanced repository information
2. **Remove any context-dependent logic** that might cause inconsistencies
3. **Ensure uniform behavior** across all execution contexts

### Phase 5: Testing & Validation
**Files to modify**: Test files, integration tests
**Estimated effort**: 2-3 hours

1. **Add unit tests** for git config parsing
2. **Add integration tests** for various execution contexts
3. **Test mixed user/org scenarios** in workspace
4. **Validate consistent behavior** across different directory structures

## Test Scenarios

### Repository Discovery Tests
1. **Single repository**: Execute from repo root
2. **Workspace with multiple repos**: Execute from workspace root
3. **Subdirectory execution**: Execute from subdirectory within workspace
4. **Nested repositories**: Handle repos within repos
5. **Mixed remotes**: Repos with different user/org combinations

### User/Org Detection Tests
1. **GitHub SSH URLs**: `git@github.com:user/repo.git`
2. **GitHub HTTPS URLs**: `https://github.com/user/repo.git`
3. **Enterprise GitHub**: `git@github.company.com:org/repo.git`
4. **GitLab URLs**: `git@gitlab.com:user/repo.git`
5. **Multiple remotes**: Origin and upstream with different users
6. **Malformed URLs**: Graceful handling of invalid configs

### Branch Detection Tests
1. **Default branch resolution**: Consistent across execution contexts
2. **Current branch detection**: Same behavior from any directory
3. **Branch existence checks**: Work from any execution location
4. **Remote branch detection**: Consistent remote branch discovery

## Success Criteria

### Functional Requirements
- [ ] Repository discovery works identically from any execution directory
- [ ] Each repository has its own user/org detected from git config (never Optional)
- [ ] Repository slug is always determinable from git config or sensible defaults
- [ ] Branch detection behavior is consistent across all contexts
- [ ] Mixed user/org workspaces are fully supported
- [ ] No special context detection or enums required
- [ ] Program panics when git config parsing fails (no graceful fallbacks to "unknown")

### Performance Requirements
- [ ] Git config parsing adds minimal overhead to discovery
- [ ] Repository discovery performance remains acceptable
- [ ] Branch detection performance is not degraded

### Compatibility Requirements
- [ ] All existing functionality continues to work
- [ ] No breaking changes to command interfaces
- [ ] Backward compatibility with existing configurations

## Risk Assessment

### Low Risk
- **Git config parsing**: Standard format, well-documented
- **URL parsing**: Limited set of patterns to handle
- **Branch detection**: Using existing git commands

### Medium Risk
- **Performance impact**: Additional git operations per repository
- **Error handling**: Malformed git configs or missing remotes
- **Edge cases**: Unusual repository configurations

### Mitigation Strategies
- **Caching**: Cache git config parsing results where appropriate
- **Graceful degradation**: Fall back to existing behavior if enhanced detection fails
- **Comprehensive testing**: Cover edge cases and error conditions

## Future Enhancements

### GitHub API Integration
- Distinguish between user and organization accounts
- Fetch additional repository metadata
- Support for GitHub Enterprise with custom API endpoints

### Multi-Remote Support
- Handle complex remote configurations
- Support for fork workflows with multiple remotes
- Intelligent remote selection for operations

### Configuration Caching
- Cache git config parsing results
- Invalidate cache when git config changes
- Performance optimization for large workspaces

## Conclusion

This approach eliminates the need for context detection by making repository discovery and branch detection work consistently regardless of execution location. By detecting user/org per-repository from git config, we support mixed scenarios and eliminate assumptions about workspace structure.

The implementation maintains backward compatibility while providing more accurate and consistent behavior across all use cases Stephen identified.
