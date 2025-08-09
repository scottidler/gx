# gx status - Implementation Plan

## Overview
Implement the `gx status` subcommand to show git status across multiple repositories discovered from the current working directory, using parallel execution patterns from slam.

## Command Signature
```bash
gx status [OPTIONS] [REPO_PATTERNS...]
```

## Options
- `--all, -a`: Show all repos including clean ones (default: only dirty repos)
- `--detailed, -d`: Show detailed file-by-file status (default: compact emoji format)
- `--parallel <N>`: Override parallelism (default: nproc)
- `--max-depth <N>`: Override repo discovery depth (default: 10)

## Implementation Components

### 1. Repository Discovery (`src/repo.rs`)
```rust
pub struct Repo {
    pub path: PathBuf,           // Absolute path to repo root
    pub name: String,            // Directory name
    pub slug: Option<String>,    // org/repo if detectable from remote
}

pub fn discover_repos(start_dir: &Path, max_depth: usize) -> Result<Vec<Repo>> {
    // Walk directory tree looking for .git folders
    // Extract repo info (name, remote origin if available)
    // Return sorted list of repositories
}
```

### 2. Repository Filtering (`src/filter.rs`)
```rust
pub fn filter_repos(repos: Vec<Repo>, patterns: &[String]) -> Vec<Repo> {
    // Apply slam's 4-level filtering:
    // 1. Exact match on repo name
    // 2. Starts-with match on repo name
    // 3. Exact match on full slug
    // 4. Starts-with match on full slug
    // Return first non-empty level
}
```

### 3. Git Status Operations (`src/git.rs`)
```rust
#[derive(Debug)]
pub struct RepoStatus {
    pub repo: Repo,
    pub branch: Option<String>,
    pub is_clean: bool,
    pub changes: StatusChanges,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct StatusChanges {
    pub modified: u32,
    pub added: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub untracked: u32,
    pub staged: u32,
}

pub fn get_repo_status(repo: &Repo) -> RepoStatus {
    // Run `git status --porcelain=v1` in repo directory
    // Parse output to count file changes
    // Get current branch name
    // Handle errors gracefully
}
```

### 4. Parallel Execution (`src/main.rs`)
```rust
use rayon::prelude::*;

fn process_status_command(patterns: Vec<String>, opts: StatusOptions) -> Result<()> {
    // 1. Discover repositories
    let repos = discover_repos(&current_dir()?, opts.max_depth)?;

    // 2. Filter repositories
    let filtered = filter_repos(repos, &patterns);

    // 3. Process in parallel
    let results: Vec<RepoStatus> = filtered
        .par_iter()
        .map(|repo| get_repo_status(repo))
        .collect();

    // 4. Display results
    display_status_results(results, &opts);

    // 5. Exit with error count
    let error_count = results.iter().filter(|r| r.error.is_some()).count();
    std::process::exit(error_count as i32);
}
```

### 5. Output Formatting (`src/output.rs`)
```rust
pub fn display_status_results(results: Vec<RepoStatus>, opts: &StatusOptions) {
    let mut clean_count = 0;
    let mut dirty_count = 0;
    let mut error_count = 0;

    for result in results {
        match result.error {
            Some(err) => {
                println!("{} ❌ {}", result.repo.name, err);
                error_count += 1;
            }
            None if result.is_clean => {
                clean_count += 1;
                if opts.show_all {
                    println!("{} ✅", result.repo.name);
                }
            }
            None => {
                dirty_count += 1;
                if opts.detailed {
                    display_detailed_status(&result);
                } else {
                    display_compact_status(&result);
                }
            }
        }
    }

    // Summary
    println!("\n📊 {} clean, {} dirty, {} errors", clean_count, dirty_count, error_count);
}

fn display_compact_status(status: &RepoStatus) {
    let changes = &status.changes;
    let mut parts = vec![status.repo.name.clone()];

    if changes.modified > 0 { parts.push(format!("📝{}", changes.modified)); }
    if changes.added > 0 { parts.push(format!("➕{}", changes.added)); }
    if changes.deleted > 0 { parts.push(format!("❌{}", changes.deleted)); }
    if changes.untracked > 0 { parts.push(format!("❓{}", changes.untracked)); }
    if changes.staged > 0 { parts.push(format!("🎯{}", changes.staged)); }

    if let Some(branch) = &status.branch {
        parts.push(format!("({})", branch));
    }

    println!("{}", parts.join(" "));
}
```

## Configuration Integration

### Default Configuration (`gx.yml`)
```yaml
default_user_org: "tatari-tv"
parallelism: null  # Use nproc
repo_discovery:
  max_depth: 10
output:
  emoji: true
  compact: true
  show_clean: false
```

### Environment Variables
- `GX_PARALLELISM`: Override parallelism (default: nproc)
- `GX_REPO_DEPTH`: Override max discovery depth
- `GX_OUTPUT_EMOJI`: Enable/disable emoji output

## CLI Structure Updates

### Update `src/cli.rs`
```rust
#[derive(Subcommand)]
pub enum Commands {
    Status {
        /// Show all repos including clean ones
        #[arg(short, long)]
        all: bool,

        /// Show detailed file-by-file status
        #[arg(short, long)]
        detailed: bool,

        /// Repository name patterns to filter
        patterns: Vec<String>,
    },
}
```

## Dependencies to Add
```toml
[dependencies]
rayon = "1.8"           # Parallel processing
walkdir = "2.4"         # Directory traversal
git2 = "0.18"           # Git operations (alternative to shelling out)
# OR use std::process::Command for git calls like slam does
```

## Error Handling Strategy
1. **Repository Discovery Errors**: Log and continue with other repos
2. **Git Command Errors**: Capture stderr, show in output with ❌
3. **Permission Errors**: Handle gracefully, show clear error messages
4. **Exit Code**: Return count of repositories that had errors

## Testing Strategy
1. **Unit Tests**: Test filtering logic, status parsing
2. **Integration Tests**: Test with mock git repositories
3. **Property Tests**: Test with various directory structures
4. **Performance Tests**: Verify parallel execution scales properly

## Implementation Order
1. Repository discovery and filtering
2. Git status parsing (single repo)
3. Parallel execution framework
4. Output formatting (compact + detailed)
5. Configuration integration
6. CLI argument parsing
7. Error handling and exit codes
8. Tests and documentation

This provides the foundation for the first working subcommand while establishing the patterns for clone and checkout commands.