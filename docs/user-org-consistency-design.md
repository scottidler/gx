# User/Org Parameter Consistency Design

## Overview

This document outlines the architectural changes needed to create consistency in how `gx` handles user/organization parameters across all subcommands. The core insight is that the directory structure created by `gx clone` already encodes the user/org information, and this should be leveraged by other subcommands rather than requiring redundant explicit parameters.

## Current Problems

### 1. Inconsistent Parameter Handling
- **`clone`**: Takes `user_or_org` as positional argument
- **`review`**: Requires explicit `--org` flag
- **Other subcommands**: Ignore user/org context entirely

### 2. Redundant Information
Users must specify `--org tatari-tv` when running from `./tatari-tv/` directory structure that `gx` itself created.

### 3. Hard-coded Token Path
Token path `~/.config/github/tokens/` is hard-coded, reducing configurability.

## Proposed Solution

### Core Principle: Directory Structure as Source of Truth

```
Working Directory Structure (created by gx clone):
├── tatari-tv/           # ← Auto-detected org
│   ├── philo/
│   ├── frontend/
│   └── api/
└── scottidler/          # ← Auto-detected user
    ├── gx/
    └── dotfiles/
```

### Auto-Detection Strategy
1. **Primary**: Extract user/org from repository paths during discovery
2. **Multi-org**: Allow operations across multiple orgs simultaneously
3. **Fallback**: Use explicit `--org`/`--user-org` flags when auto-detection fails

## Design Changes

### 1. Configuration Schema Updates

#### New Configuration Fields
Add to `src/config.rs`:

```rust
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    // ... existing fields ...

    /// Path template for GitHub tokens
    #[serde(rename = "token-path")]
    pub token_path: Option<String>,

    /// Default user/org for operations
    #[serde(rename = "default-user-org")]
    pub default_user_org: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            token_path: Some("~/.config/github/tokens/{user_or_org}".to_string()),
            default_user_org: None,
        }
    }
}
```

#### Configuration File Example
```yaml
# ~/.config/gx/gx.yml
default-user-org: "tatari-tv"
token-path: "~/.config/github/tokens/{user_or_org}"  # {user_or_org} is replaced at runtime
jobs: "nproc"

# Alternative token path examples:
# token-path: "~/.secrets/github/{user_or_org}.token"
# token-path: "/etc/gx/tokens/{user_or_org}"
```

### 2. CLI Parameter Changes

#### Make `--org` Optional in Review Subcommand
Update `src/cli.rs`:

```rust
Review {
    /// GitHub organization (auto-detected if not specified)
    #[arg(short = 'o', long = "org", help = "GitHub organization (auto-detected from directory structure if not specified)")]
    org: Option<String>,  // Changed from String to Option<String>

    // ... rest unchanged ...
}
```

#### Add Global `--user-org` Override (Optional)
```rust
pub struct Cli {
    // ... existing fields ...

    /// Override user/org for operations
    #[arg(long = "user-org", help = "Override user/org (auto-detected from directory structure if not specified)")]
    pub user_org: Option<String>,

    // ... rest unchanged ...
}
```

### 3. Core Logic Implementation

#### New Utility Module: `src/user_org.rs`
Create new module for user/org detection logic:

```rust
use eyre::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use crate::repo::Repo;
use crate::config::Config;

/// User/Org detection result
#[derive(Debug, Clone)]
pub struct UserOrgContext {
    pub user_or_org: String,
    pub detection_method: DetectionMethod,
}

#[derive(Debug, Clone)]
pub enum DetectionMethod {
    Explicit,           // From CLI parameter
    AutoDetected,       // From directory structure
    Configuration,      // From config file default
}

/// Determine user/org(s) from various sources with precedence
pub fn determine_user_orgs(
    cli_override: Option<&str>,
    global_override: Option<&str>,
    discovered_repos: &[Repo],
    config: &Config,
) -> Result<Vec<UserOrgContext>> {
    // 1. Explicit CLI parameter (highest precedence) - single org
    if let Some(user_org) = cli_override.or(global_override) {
        return Ok(vec![UserOrgContext {
            user_or_org: user_org.to_string(),
            detection_method: DetectionMethod::Explicit,
        }]);
    }

    // 2. Auto-detect from repository paths - potentially multiple orgs
    if let Ok(detected_orgs) = auto_detect_from_repos(discovered_repos) {
        return Ok(detected_orgs.into_iter().map(|org| UserOrgContext {
            user_or_org: org,
            detection_method: DetectionMethod::AutoDetected,
        }).collect());
    }

    // 3. Configuration file default - single org
    if let Some(default) = &config.default_user_org {
        return Ok(vec![UserOrgContext {
            user_or_org: default.clone(),
            detection_method: DetectionMethod::Configuration,
        }]);
    }

    Err(eyre::eyre!("Unable to determine user/org: not specified explicitly, cannot auto-detect from directory structure, and no default configured"))
}

/// Auto-detect user/org(s) from repository directory structure
fn auto_detect_from_repos(repos: &[Repo]) -> Result<Vec<String>> {
    let user_orgs: HashSet<String> = repos
        .iter()
        .filter_map(|repo| extract_user_org_from_path(&repo.path))
        .collect();

    match user_orgs.len() {
        0 => Err(eyre::eyre!("No user/org detected from repository paths")),
        _ => Ok(user_orgs.into_iter().collect()),
    }
}

/// Extract user/org from repository path
/// Examples (working from parent directory):
///   ./tatari-tv/philo/.git -> Some("tatari-tv")
///   ./scottidler/gx/.git -> Some("scottidler")
///   ./standalone-repo/.git -> None
fn extract_user_org_from_path(repo_path: &Path) -> Option<String> {
    let path_components: Vec<_> = repo_path.components().collect();

    // Look for pattern: ./user_or_org/repo_name/.git
    // When running from parent directory, repo paths look like:
    // - ./tatari-tv/philo/.git
    // - ./scottidler/gx/.git
    if path_components.len() >= 3 {
        // Get the first directory component after "./" (user/org)
        if let Some(user_org_component) = path_components.get(1) {
            if let Some(user_org) = user_org_component.as_os_str().to_str() {
                // Skip common non-user-org directory names
                if !["src", "projects", "workspace", "repos", "git"].contains(&user_org) {
                    return Some(user_org.to_string());
                }
            }
        }
    }

    None
}

/// Build token path from template and user/org
pub fn build_token_path(template: &str, user_or_org: &str) -> PathBuf {
    let expanded = template.replace("{user_or_org}", user_or_org);

    // Handle tilde expansion
    if expanded.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&expanded[2..]);
        }
    }

    PathBuf::from(expanded)
}
```

### 4. GitHub Module Updates

#### Update Token Reading Logic
Modify `src/github.rs`:

```rust
/// Read GitHub token for a user/org using configurable path
pub fn read_token(user_or_org: &str, config: &Config) -> Result<String> {
    let token_template = config.token_path
        .as_deref()
        .unwrap_or("~/.config/github/tokens/{user_or_org}");

    let token_path = crate::user_org::build_token_path(token_template, user_or_org);

    let token = fs::read_to_string(&token_path)
        .context(format!("Failed to read token from {}", token_path.display()))?
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(eyre::eyre!("Token file is empty: {}", token_path.display()));
    }

    Ok(token)
}
```

### 5. Subcommand Implementation Updates

#### Review Subcommand Changes
Update `src/review.rs`:

```rust
pub fn process_review_ls_command(
    cli: &Cli,
    config: &Config,
    org: Option<&str>,  // Changed from &str to Option<&str>
    patterns: &[String],
    change_ids: &[String],
) -> Result<()> {
    // Discover repositories for auto-detection
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli.max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = repo::discover_repos(start_dir, max_depth)
        .context("Failed to discover repositories")?;

    // Determine user/org(s) with precedence
    let user_org_contexts = crate::user_org::determine_user_orgs(
        org,
        cli.user_org.as_deref(),
        &repos,
        config,
    )?;

    info!("Using {} org(s): {}",
          user_org_contexts.len(),
          user_org_contexts.iter()
              .map(|ctx| format!("{} ({})", ctx.user_or_org, format!("{:?}", ctx.detection_method).to_lowercase()))
              .collect::<Vec<_>>()
              .join(", "));

    // Process each org and aggregate results
    let mut all_prs = Vec::new();
    for context in &user_org_contexts {
        match github::list_prs_by_change_id(&context.user_or_org, change_id) {
            Ok(mut prs) => all_prs.append(&mut prs),
            Err(e) => warn!("Failed to get PRs from {}: {}", context.user_or_org, e),
        }
    }

    // ... rest unchanged ...
}
```

#### Clone Subcommand Changes
Update `src/clone.rs` to use configurable token path:

```rust
pub fn process_clone_command(
    cli: &Cli,
    config: &Config,
    user_or_org: &str,
    include_archived: bool,
    patterns: &[String],
) -> Result<()> {
    // ... existing logic ...

    // Read GitHub token using configurable path
    let token = github::read_token(user_or_org, config)
        .context("Failed to read GitHub token")?;

    // ... rest unchanged ...
}
```

## Implementation Plan

### Phase 1: Configuration Infrastructure
1. **Update `src/config.rs`**
   - Add `token_path` field with default
   - Update `Default` implementation
   - Add validation for token path template

2. **Create `src/user_org.rs`**
   - Implement user/org detection logic
   - Add path extraction utilities
   - Add token path building logic

3. **Update `src/lib.rs`**
   - Add `pub mod user_org;`

### Phase 2: GitHub Integration
1. **Update `src/github.rs`**
   - Modify `read_token()` to accept config parameter
   - Use configurable token path template
   - Update all callers

### Phase 3: CLI Parameter Updates
1. **Update `src/cli.rs`**
   - Make `org` parameter optional in `Review` struct
   - Add optional global `--user-org` parameter
   - Update help text and examples

### Phase 4: Subcommand Updates
1. **Update `src/review.rs`**
   - Modify all `process_review_*_command()` functions
   - Add user/org detection logic
   - Update function signatures to accept `Option<&str>`

2. **Update `src/clone.rs`**
   - Use configurable token path
   - Pass config to `github::read_token()`

3. **Update `src/main.rs`**
   - Pass optional parameters correctly
   - Handle new CLI structure

### Phase 5: Documentation & Testing
1. **Update Documentation**
   - Update help text in CLI definitions
   - Update example commands
   - Document new configuration options

2. **Add Tests**
   - Test user/org detection logic
   - Test token path building
   - Test edge cases (multiple orgs, no detection)

3. **Update Configuration Examples**
   - Update `gx.yml` with new fields
   - Update test fixtures

## Backwards Compatibility

### Maintained Compatibility
- Existing `--org` parameter continues to work
- Existing token file locations work (default unchanged)
- Existing configuration files work (new fields optional)

### Migration Path
- Users can gradually adopt auto-detection
- Explicit parameters override auto-detection
- Configuration migration is optional

## Benefits

1. **Consistency**: All subcommands use same user/org detection logic
2. **User Experience**: Reduced typing, natural workflow
3. **Flexibility**: Configurable token paths for different environments
4. **Maintainability**: Single source of truth for user/org handling
5. **Backwards Compatible**: Existing workflows continue to work

## Edge Cases Handled

1. **Multiple Orgs**: Operations run across all detected orgs automatically
2. **No Detection**: Falls back to configuration default
3. **Custom Layouts**: Explicit parameters override auto-detection
4. **Missing Tokens**: Clear error messages with path information per org
5. **Invalid Paths**: Validation and helpful error messages
6. **Partial Failures**: Continue processing other orgs when one fails

## Configuration Examples

### Basic Configuration
```yaml
# ~/.config/gx/gx.yml
default-user-org: "tatari-tv"
token-path: "~/.config/github/tokens/{user_or_org}"
```

### Advanced Configuration
```yaml
# ~/.config/gx/gx.yml
default-user-org: "tatari-tv"
token-path: "~/.secrets/github-tokens/{user_or_org}.token"
jobs: 8

repo-discovery:
  max-depth: 5
  ignore-patterns:
    - "node_modules"
    - ".git"
```

### Environment-Specific Configuration
```yaml
# ~/.config/gx/gx.yml (CI environment)
token-path: "/etc/github-tokens/{user_or_org}"
default-user-org: "tatari-tv"
jobs: 16
```

## Multi-Org Operation Examples

### Natural Multi-Org Workflow
```bash
~/slam/                          # Working directory
├── tatari-tv/                   # Org directory
│   ├── philo/
│   ├── frontend/
│   └── api/
└── scottidler/                  # User directory
    ├── gx/
    └── dotfiles/

# Multi-org operations (no --org needed)
~/slam$ gx review ls GX-2024-01-15
# Output:
# Using 2 org(s): tatari-tv (autodetected), scottidler (autodetected)
#
# tatari-tv/frontend:
#   PR #123: Add new feature (Open)
#   PR #124: Fix bug (Open)
#
# scottidler/gx:
#   PR #456: Update docs (Open)

~/slam$ gx review clone GX-2024-01-15
# Clones repos from both orgs that have matching PRs
# Uses ~/.config/github/tokens/tatari-tv for tatari-tv repos
# Uses ~/.config/github/tokens/scottidler for scottidler repos

~/slam$ gx review approve GX-2024-01-15
# Approves PRs across both orgs
# Continues even if some fail due to permissions

~/slam$ gx review delete GX-2024-01-15
# Deletes PRs across both orgs
# Uses appropriate token for each org

~/slam$ gx review purge
# Purges GX branches from all repos in both orgs
# User takes full responsibility for multi-org destruction
```

### Single Org Override
```bash
~/slam$ gx review ls --org tatari-tv GX-2024-01-15
# Forces operation to only tatari-tv, ignoring scottidler repos
```

## Success Criteria

1. **Functional**: All existing commands work without changes
2. **Enhanced UX**: `gx review ls CHANGE-ID` works from parent directory without `--org`
3. **Multi-org Support**: Operations automatically work across multiple detected orgs
4. **Configurable**: Token paths can be customized via configuration
5. **Robust**: Clear error messages for edge cases and partial failures
6. **Maintainable**: Single, consistent pattern for user/org handling across all subcommands
