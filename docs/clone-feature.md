# Clone Feature Implementation Plan

## Overview

The `gx clone` command will clone all repositories from a GitHub user or organization, with filtering capabilities and intelligent update behavior for existing repositories.

## Command Structure

### Global CLI Updates

Add global `--cwd` option to `Cli` struct in `src/cli.rs`:

```rust
#[derive(Parser)]
#[command(
    name = "gx",
    about = "git operations across multiple repositories",
    version = env!("GIT_DESCRIBE"),
    after_help = HELP_TEXT.as_str()
)]
pub struct Cli {
    /// Working directory (only changes from current directory if specified)
    #[arg(long, help = "Working directory for operations")]
    pub cwd: Option<PathBuf>,

    // ... existing fields

    #[command(subcommand)]
    pub command: Commands,
}
```

### Clone Command

Add `Clone` command to `Commands` enum:

```rust
/// Clone repositories from GitHub user/org
#[command(after_help = "CLONE LEGEND:
  📥  Cloned new repository               🔄  Updated existing repository
  📍  Checked out default branch          ⚠️  Clone/update failed
  🏠  Directory exists but not git repo   🔗  Different remote URL detected
  📦  Stashed uncommitted changes         📊  Summary stats

WORKING DIRECTORY:
  By default, repositories are cloned to the current working directory under {user_or_org}/{repo_name}/
  Use --cwd to specify a different base directory for cloning operations.

EXAMPLES:
  gx clone scottidler                     # Clone to ./scottidler/{repo_name}/
  gx clone tatari-tv frontend api         # Clone filtered repos to ./tatari-tv/{repo_name}/
  gx --cwd /workspace clone tatari-tv     # Clone to /workspace/tatari-tv/{repo_name}/")]
Clone {
    /// GitHub user or organization name
    #[arg(value_name = "USER_OR_ORG")]
    user_or_org: String,

    /// Include archived repositories
    #[arg(long, help = "Include archived repositories")]
    include_archived: bool,

    /// Repository name patterns to filter
    patterns: Vec<String>,
},
```

## Working Directory Logic

In `main.rs`, handle `--cwd` ONLY if user specifies it:

```rust
fn main() -> Result<()> {
    // Setup logging first
    setup_logging().context("Failed to setup logging")?;

    // Parse CLI arguments
    let cli = Cli::parse();

    // ONLY change directory if user explicitly provided --cwd
    if let Some(cwd) = &cli.cwd {
        env::set_current_dir(cwd)
            .context(format!("Failed to change to directory: {}", cwd.display()))?;
        info!("Changed working directory to: {}", cwd.display());
    }
    // Otherwise, stay in current working directory (default behavior)

    // Load configuration
    let config = Config::load(cli.config.as_ref())
        .context("Failed to load configuration")?;

    // Run the main application logic
    run_application(&cli, &config).context("Application failed")?;

    Ok(())
}
```

## GitHub API Integration

Create new module `src/github.rs`:

```rust
/// Get all non-archived repositories for a user/org
pub fn get_user_repos(user_or_org: &str, include_archived: bool) -> Result<Vec<String>> {
    // Read token from ~/.config/github/tokens/{user_or_org} (plain text)
    let token_path = dirs::config_dir()
        .unwrap_or_default()
        .join("github")
        .join("tokens")
        .join(user_or_org);

    let token = fs::read_to_string(&token_path)
        .context(format!("Failed to read token from {}", token_path.display()))?
        .trim()
        .to_string();

    // Query GitHub API
    let archived_filter = if include_archived { "" } else { " | select(.archived == false)" };
    let query = format!("orgs/{}/repos", user_or_org);

    let output = Command::new("gh")
        .env("GH_TOKEN", token)
        .args(["api", &query, "--paginate", "--jq", &format!(".[]{}  | .full_name", archived_filter)])
        .output()
        .context("Failed to execute gh command")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("GitHub API query failed: {}", error));
    }

    let repos = String::from_utf8(output.stdout)?
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    Ok(repos)
}

/// Get default branch for a repository
pub fn get_default_branch(repo_slug: &str, token: &str) -> Result<String> {
    let output = Command::new("gh")
        .env("GH_TOKEN", token)
        .args(["api", &format!("repos/{}", repo_slug), "--jq", ".default_branch"])
        .output()
        .context("Failed to get default branch")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to get default branch: {}", error));
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
```

## Clone Implementation

Add to `src/git.rs`:

```rust
#[derive(Debug, Clone)]
pub struct CloneResult {
    pub repo_slug: String,  // "user/repo"
    pub action: CloneAction,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CloneAction {
    Cloned,                    // 📥 Successfully cloned new repo
    Updated,                   // 🔄 Updated existing repo (checkout + pull)
    Stashed,                   // 📦 Stashed changes during update
    DirectoryNotGitRepo,       // 🏠 Directory exists but not git
    DifferentRemote,           // 🔗 Different remote URL
}

/// Clone or update a repository
pub fn clone_or_update_repo(repo_slug: &str, user_or_org: &str, token: &str) -> CloneResult {
    let parts: Vec<&str> = repo_slug.split('/').collect();
    let repo_name = parts[1];
    let target_dir = PathBuf::from(user_or_org).join(repo_name);

    if !target_dir.exists() {
        // Clone new repository
        return clone_repo(repo_slug, &target_dir, token);
    }

    if !target_dir.join(".git").exists() {
        // Directory exists but not a git repo
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::DirectoryNotGitRepo,
            error: None,
        };
    }

    // Check if existing repo has correct remote
    match get_remote_origin(&target_dir) {
        Ok(origin) if is_same_repo(&origin, repo_slug) => {
            // Update existing repo: get default branch, checkout, pull
            update_existing_repo(&target_dir, repo_slug, token)
        }
        Ok(_) => {
            // Different remote URL
            CloneResult {
                repo_slug: repo_slug.to_string(),
                action: CloneAction::DifferentRemote,
                error: None,
            }
        }
        Err(e) => CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to check remote: {}", e)),
        }
    }
}

fn update_existing_repo(repo_path: &Path, repo_slug: &str, token: &str) -> CloneResult {
    // Get default branch from GitHub
    let default_branch = match crate::github::get_default_branch(repo_slug, token) {
        Ok(branch) => branch,
        Err(e) => return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to get default branch: {}", e)),
        }
    };

    let mut stashed = false;

    // Check for uncommitted changes (same logic as checkout_branch)
    if let Ok(status) = get_status_changes_for_path(repo_path) {
        if !status.is_empty() {
            // Stash changes
            let stash_result = Command::new("git")
                .args(["-C", &repo_path.to_string_lossy(), "stash", "push", "-m", "gx auto-stash for clone update"])
                .output();

            if let Ok(output) = stash_result {
                if output.status.success() {
                    stashed = true;
                }
            }
        }
    }

    // Checkout default branch
    let checkout_result = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "checkout", &default_branch])
        .output();

    if let Err(e) = checkout_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to checkout default branch: {}", e)),
        };
    }

    // Pull latest (same as checkout: --ff-only)
    let pull_result = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "pull", "--ff-only"])
        .output();

    if let Err(e) = pull_result {
        return CloneResult {
            repo_slug: repo_slug.to_string(),
            action: CloneAction::Updated,
            error: Some(format!("Failed to pull latest changes: {}", e)),
        };
    }

    let action = if stashed {
        CloneAction::Stashed
    } else {
        CloneAction::Updated
    };

    CloneResult {
        repo_slug: repo_slug.to_string(),
        action,
        error: None,
    }
}
```

## Main Application Integration

Add to `run_application()` in `src/main.rs`:

```rust
Commands::Clone {
    user_or_org,
    include_archived,
    patterns,
} => {
    process_clone_command(cli, config, user_or_org, *include_archived, patterns)
}
```

## Output Display

Add to `src/output.rs`:

```rust
pub fn display_clone_results(results: Vec<CloneResult>, detailed: bool) {
    // Display results with emojis following existing patterns
    // Log errors to ~/.local/share/gx/logs/gx.log
    // Show detailed errors only when --detailed flag used
}
```

## Files to Create/Modify

1. **`src/github.rs`** - New module for GitHub API integration
2. **`src/cli.rs`** - Add Clone command + global --cwd option
3. **`src/main.rs`** - Add clone command handling + cwd logic
4. **`src/git.rs`** - Add CloneResult types and clone/update functions
5. **`src/output.rs`** - Add clone results display function

## Key Behaviors

- **Working Directory**: Operates in current directory unless `--cwd` specified
- **Clone Location**: `{working_dir}/{user_or_org}/{repo_name}/`
- **Authentication**: Plain text tokens from `~/.config/github/tokens/{user_or_org}`
- **Default Branch**: Always query GitHub API, never assume
- **Uncommitted Changes**: Use same stashing logic as existing checkout command
- **Parallelism**: Use existing rayon patterns with nproc default
- **Error Handling**: Emoji summary + detailed logs, continue on individual failures

## Testing Plan

### Test Organization: `gx-testing`

Use the `gx-testing` GitHub organization for comprehensive testing of the clone feature.

### Setup Requirements

1. **Authentication**: Create token file at `~/.config/github/tokens/gx-testing`
2. **Test Workspace**: Use temporary directory for testing to avoid conflicts
3. **Test Repositories**: The `gx-testing` org should contain:
   - Multiple active repositories
   - At least one archived repository
   - Repositories with different default branches (main, master, develop)
   - Mix of public and private repositories

### Test Cases

#### Basic Functionality Tests

1. **Full Clone Test**
   ```bash
   # Test cloning all non-archived repos
   gx --cwd /tmp/test-workspace clone gx-testing
   ```
   - Verify all non-archived repos are cloned
   - Verify directory structure: `/tmp/test-workspace/gx-testing/{repo_name}/`
   - Verify each repo is on its default branch

2. **Filtered Clone Test**
   ```bash
   # Test pattern filtering
   gx --cwd /tmp/test-workspace clone gx-testing frontend api
   ```
   - Verify only matching repositories are cloned
   - Test all 4 levels of filtering (exact name, starts-with name, exact slug, starts-with slug)

3. **Archived Repositories Test**
   ```bash
   # Test including archived repos
   gx --cwd /tmp/test-workspace clone gx-testing --include-archived
   ```
   - Verify archived repositories are included when flag is used
   - Verify archived repositories are excluded by default

#### Update Behavior Tests

4. **Clean Update Test**
   ```bash
   # Clone repos, then run clone again
   gx --cwd /tmp/test-workspace clone gx-testing
   gx --cwd /tmp/test-workspace clone gx-testing
   ```
   - Verify second run updates existing repos (checkout default + pull)
   - Verify no duplicate directories created

5. **Uncommitted Changes Test**
   ```bash
   # Clone repos, make local changes, then run clone again
   gx --cwd /tmp/test-workspace clone gx-testing
   # Make uncommitted changes in one repo
   echo "test" >> /tmp/test-workspace/gx-testing/repo1/test.txt
   gx --cwd /tmp/test-workspace clone gx-testing
   ```
   - Verify uncommitted changes are stashed
   - Verify repo is updated to latest default branch
   - Verify stash contains the changes

#### Error Handling Tests

6. **Directory Conflict Test**
   ```bash
   # Create non-git directory with same name as repo
   mkdir -p /tmp/test-workspace/gx-testing/conflicting-repo
   echo "not a git repo" > /tmp/test-workspace/gx-testing/conflicting-repo/file.txt
   gx --cwd /tmp/test-workspace clone gx-testing
   ```
   - Verify 🏠 emoji shown for directory that exists but isn't git repo
   - Verify error logged but other repos continue processing

7. **Different Remote Test**
   ```bash
   # Clone repo, then change its remote origin
   gx --cwd /tmp/test-workspace clone gx-testing
   cd /tmp/test-workspace/gx-testing/some-repo
   git remote set-url origin https://github.com/different/repo.git
   cd /tmp/test-workspace
   gx clone gx-testing
   ```
   - Verify 🔗 emoji shown for different remote URL
   - Verify error logged but repo left unchanged

8. **Authentication Failure Test**
   ```bash
   # Test with invalid token
   echo "invalid_token" > ~/.config/github/tokens/gx-testing
   gx --cwd /tmp/test-workspace clone gx-testing
   ```
   - Verify authentication error is handled gracefully
   - Verify appropriate error message and emoji

#### Working Directory Tests

9. **Default CWD Test**
   ```bash
   cd /tmp/test-workspace
   gx clone gx-testing
   ```
   - Verify repos cloned to current directory: `/tmp/test-workspace/gx-testing/{repo_name}/`

10. **Explicit CWD Test**
    ```bash
    gx --cwd /tmp/different-workspace clone gx-testing
    ```
    - Verify repos cloned to specified directory: `/tmp/different-workspace/gx-testing/{repo_name}/`

#### Parallelism Tests

11. **Parallel Execution Test**
    ```bash
    # Test with different parallelism settings
    gx --parallel 1 --cwd /tmp/test-workspace-serial clone gx-testing
    gx --parallel 4 --cwd /tmp/test-workspace-parallel clone gx-testing
    ```
    - Verify both complete successfully
    - Verify parallel version is faster (if org has many repos)

#### Integration Tests

12. **Status Integration Test**
    ```bash
    gx --cwd /tmp/test-workspace clone gx-testing
    gx --cwd /tmp/test-workspace status
    ```
    - Verify `gx status` works correctly on cloned repositories
    - Verify all cloned repos appear in status output

13. **Checkout Integration Test**
    ```bash
    gx --cwd /tmp/test-workspace clone gx-testing
    gx --cwd /tmp/test-workspace checkout -b test-branch
    ```
    - Verify `gx checkout` works correctly on cloned repositories
    - Verify new branch created in all repos

### Test Automation

Create test script `tests/clone_tests.rs` with automated test cases:

```rust
#[cfg(test)]
mod clone_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_clone_basic_functionality() {
        // Test basic clone operation
    }

    #[test]
    fn test_clone_with_filters() {
        // Test pattern filtering
    }

    #[test]
    fn test_clone_update_behavior() {
        // Test update existing repos
    }

    #[test]
    fn test_clone_error_handling() {
        // Test various error conditions
    }

    #[test]
    fn test_clone_working_directory() {
        // Test --cwd behavior
    }
}
```

### Manual Testing Checklist

- [ ] Basic clone functionality
- [ ] Pattern filtering (all 4 levels)
- [ ] Archived repository handling
- [ ] Update behavior for existing repos
- [ ] Uncommitted changes stashing
- [ ] Directory conflict handling
- [ ] Different remote URL detection
- [ ] Authentication error handling
- [ ] Default vs explicit working directory
- [ ] Parallel execution
- [ ] Integration with status command
- [ ] Integration with checkout command
- [ ] Emoji output and error reporting
- [ ] Detailed error logging

### Performance Testing

- Test with large organizations (>50 repos)
- Measure clone time vs update time
- Verify parallel execution scales appropriately
- Test memory usage during large operations

### Cleanup

After testing, clean up test workspaces:
```bash
rm -rf /tmp/test-workspace*
```
