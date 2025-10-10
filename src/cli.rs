use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

/// Pull request type
#[derive(Debug, Clone, ValueEnum)]
pub enum PR {
    /// Create a normal pull request
    #[value(name = "normal")]
    Normal,
    /// Create a draft pull request
    #[value(name = "draft")]
    Draft,
}

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);
static JOBS_HELP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "Number of parallel operations [default: {}]",
        num_cpus::get()
    )
});
static DEPTH_HELP: LazyLock<String> = LazyLock::new(|| {
    let effective_default = get_effective_max_depth_default();
    format!("Maximum directory depth to scan [default: {effective_default}]")
});

/// Get the effective default max depth by loading config if available
fn get_effective_max_depth_default() -> usize {
    // Try to load config to get the actual default that would be used
    match crate::config::Config::load(None) {
        Ok(config) => {
            config
                .repo_discovery
                .as_ref()
                .and_then(|rd| rd.max_depth)
                .unwrap_or(3)
            // Program default if not in config
        }
        Err(_) => 3, // Program default if config fails to load
    }
}

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

    /// Path to config file
    #[arg(short, long, help = "Path to config file")]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, help = "Enable verbose output")]
    pub verbose: bool,

    /// Override jobs
    #[arg(short = 'j', long = "jobs", value_name = "INT", help = JOBS_HELP.as_str())]
    pub parallel: Option<usize>,

    /// Override max repository discovery depth
    #[arg(short = 'm', long = "depth", value_name = "INT", help = DEPTH_HELP.as_str())]
    pub max_depth: Option<usize>,

    /// Override user/org for operations
    #[arg(
        long = "user-org",
        help = "Override user/org (auto-detected from directory structure if not specified)"
    )]
    pub user_org: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Show git status across multiple repositories
    #[command(after_help = "EMOJI LEGEND:
  📝  Modified files       ➕  Added files         ❌  Deleted files
  ❓  Untracked files      🎯  Staged files        🔄  Renamed files
  ✅  Clean repository     📁  Repository header   📊  Summary stats

REMOTE STATUS:
  🟢  Up to date with remote    ⬆️N  Ahead by N commits
  ⬇️N  Behind by N commits      🔀  Diverged (ahead+behind)
  📍  No remote branch          ⚠️git Remote check error (git command failed)

EXAMPLES:
  gx status                     # Show all repositories
  gx status --detailed          # Show file-by-file details
  gx status -p frontend -p api  # Filter by repo patterns
  gx status --no-emoji          # Plain text for scripts")]
    Status {
        /// Show detailed file-by-file status
        #[arg(
            short,
            long,
            help = "Show detailed status instead of compact"
        )]
        detailed: bool,

        /// Disable emoji output
        #[arg(long, help = "Disable emoji in output")]
        no_emoji: bool,

        /// Disable colored output
        #[arg(long, help = "Disable colored output")]
        no_color: bool,

        /// Repository name patterns to filter
        #[arg(
            short = 'p',
            long = "patterns",
            help = "Repository name patterns to filter"
        )]
        patterns: Vec<String>,

        /// Fetch latest remote refs before status check
        #[arg(long, help = "Fetch latest remote refs before status check")]
        fetch_first: bool,

        /// Skip remote status checks entirely
        #[arg(long, help = "Skip remote status checks entirely")]
        no_remote: bool,
    },

    /// Checkout branches across multiple repositories
    #[command(after_help = "CHECKOUT LEGEND:
  🔄  Checked out and synced with remote    ✨  Created new branch from remote
  📦  Stashed uncommitted changes           ❌  Checkout failed (error)
  ⚠️   Has untracked files                  📊  Summary stats

EXAMPLES:
  gx checkout                       # Checkout default branch in all repos
  gx checkout default               # Same as above (explicit)
  gx checkout feature-branch        # Checkout existing branch in all repos
  gx checkout -p frontend           # Checkout default branch in repos matching 'frontend'
  gx checkout main -p frontend      # Checkout main branch in repos matching 'frontend'
  gx checkout -b new-feature        # Create and checkout new branch in all repos
  gx checkout -b fix -f main        # Create branch from specific base branch
  gx checkout main -s               # Checkout main and stash uncommitted changes
  gx checkout main -p frontend -p api  # Checkout main in repos matching 'frontend' or 'api'")]
    Checkout {
        /// Create a new branch
        #[arg(
            short = 'b',
            long = "branch",
            help = "Create and checkout a new branch"
        )]
        create_branch: bool,

        /// Base branch to create from (defaults to 'default')
        #[arg(
            short = 'f',
            long = "from",
            value_name = "BRANCH",
            help = "Base branch for new branch creation [Default: default]"
        )]
        from_branch: Option<String>,

        /// Stash uncommitted changes before checkout
        #[arg(
            short = 's',
            long = "stash",
            help = "Stash uncommitted changes before checkout"
        )]
        stash: bool,

        /// Repository name patterns to filter
        #[arg(
            short = 'p',
            long = "patterns",
            value_name = "PATTERN",
            help = "Repository name patterns to filter"
        )]
        patterns: Vec<String>,

        /// Branch name to checkout ('default' for repo's default branch)
        #[arg(value_name = "BRANCH", default_value = "default")]
        branch_name: String,
    },

    /// Clone repositories from GitHub user/org
    #[command(after_help = "CLONE LEGEND:
  📥  Cloned new repository               🔄  Updated existing repository
  📍  Checked out default branch          ⚠️  Clone/update failed
  🏠  Directory exists but not git repo   🔗  Different remote URL detected
  📦  Stashed uncommitted changes         📊  Summary stats

WORKING DIRECTORY:
  By default, repositories are cloned to the current working directory under <user|org>/<repo-name>/
  Use --cwd to specify a different base directory for cloning operations.

EXAMPLES:
  gx clone scottidler                     # Clone to ./scottidler/<repo-name>/
  gx clone tatari-tv -p frontend -p api   # Clone filtered repos to ./tatari-tv/<repo-name>/
  gx --cwd /workspace clone tatari-tv     # Clone to /workspace/tatari-tv/<repo-name>/")]
    Clone {
        /// GitHub user or organization name
        #[arg(value_name = "USER|ORG")]
        user_or_org: String,

        /// Include archived repositories
        #[arg(long, help = "Include archived repositories")]
        include_archived: bool,

        /// Repository name patterns to filter
        #[arg(
            short = 'p',
            long = "patterns",
            help = "Repository name patterns to filter"
        )]
        patterns: Vec<String>,
    },

    /// Apply changes across multiple repositories and create PRs
    #[command(after_help = "CREATE LEGEND:
  📝  Files modified        ➕  Files added         ❌  Files deleted
  🔄  Branch created        📥  PR created          📊  Summary stats
  👁️  Dry run (preview)     💾  Changes committed   ❌  Error occurred

EXAMPLES:
  gx create --files '*.json'                                    # Show matching files (dry-run)
  gx create --files '*.json' -p frontend                       # Show matches in frontend repos only
  gx create --files '*.json' add config.json '{\"debug\": true}' # Actually create files
  gx create --files '*.md' sub 'old-text' 'new-text' --commit 'Update docs'
  gx create --files 'package.json' regex '\"version\": \"[^\"]+\"' '\"version\": \"1.2.3\"'
  gx create --files '*.txt' delete --commit 'Remove old files' --pr
  gx create --files '*.md' sub 'old' 'new' --commit 'Draft update' --pr=draft")]
    Create {
        /// Files to target (glob patterns)
        #[arg(short = 'f', long = "files", help = "File patterns to match")]
        files: Vec<String>,

        /// Change ID for branch and PR naming
        #[arg(
            short = 'x',
            long = "change-id",
            help = "Change ID for branch/PR (auto-generated if not provided)"
        )]
        change_id: Option<String>,

        /// Repository patterns to filter
        #[arg(
            short = 'p',
            long = "patterns",
            help = "Repository patterns to filter"
        )]
        patterns: Vec<String>,

        /// Commit changes with message
        #[arg(
            short = 'c',
            long = "commit",
            help = "Commit changes with message"
        )]
        commit: Option<String>,

        /// Create PR after committing (use --pr=draft for draft mode)
        #[arg(
            long,
            help = "Create pull request after committing (use --pr=draft for draft mode)",
            default_missing_value = "normal",
            num_args = 0..=1
        )]
        pr: Option<PR>,

        #[command(subcommand)]
        action: Option<CreateAction>,
    },

    /// Manage PRs across multiple repositories
    #[command(after_help = "REVIEW LEGEND:
  📋  PR listed             📥  Repository cloned   ✅  PR approved
  ❌  PR deleted            🧹  Repository purged   📊  Summary stats

EXAMPLES:
  gx review ls GX-2024-01-15                    # List PRs (auto-detect org)
  gx review ls --org tatari-tv GX-2024-01-15    # List PRs for specific org
  gx review clone GX-2024-01-15                 # Clone repos with PRs (auto-detect)
  gx review approve GX-2024-01-15 --admin       # Approve and merge PRs (auto-detect)
  gx review delete GX-2024-01-15                # Delete PRs and branches (auto-detect)
  gx review purge --org tatari-tv                # Clean up GX branches (explicit org)")]
    Review {
        /// GitHub organization (auto-detected if not specified)
        #[arg(
            short = 'o',
            long = "org",
            help = "GitHub organization (auto-detected from directory structure if not specified)"
        )]
        org: Option<String>,

        /// Repository patterns to filter
        #[arg(
            short = 'p',
            long = "patterns",
            help = "Repository patterns to filter"
        )]
        patterns: Vec<String>,

        #[command(subcommand)]
        action: ReviewAction,
    },

    /// Rollback interrupted operations and recovery management
    #[command(after_help = "ROLLBACK LEGEND:
  🔄  Rollback executed         ✅  Recovery successful   ❌  Rollback failed
  📋  Recovery states listed    🧹  Recovery state cleaned 📊  Summary stats

RECOVERY OPERATIONS:
  gx rollback list              # List available recovery states
  gx rollback execute <id>      # Execute recovery for specific transaction
  gx rollback validate <id>     # Validate recovery operations before execution
  gx rollback cleanup           # Clean up old recovery states
  gx rollback cleanup <id>      # Clean up specific recovery state

EXAMPLES:
  gx rollback list                          # Show all interrupted transactions
  gx rollback execute gx-tx-1234567890      # Recover specific transaction
  gx rollback validate gx-tx-1234567890     # Check if recovery is safe
  gx rollback cleanup --older-than 7d       # Clean up states older than 7 days")]
    Rollback {
        #[command(subcommand)]
        action: RollbackAction,
    },
}

#[derive(Debug, Subcommand)]
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
    /// Purge all GX branches and PRs
    Purge,
}

#[derive(Debug, Clone, Subcommand)]
pub enum RollbackAction {
    /// List available recovery states
    List,
    /// Execute recovery for a specific transaction
    Execute {
        #[arg(help = "Transaction ID to recover")]
        transaction_id: String,
        #[arg(short, long, help = "Skip validation before executing")]
        force: bool,
    },
    /// Validate recovery operations without executing
    Validate {
        #[arg(help = "Transaction ID to validate")]
        transaction_id: String,
    },
    /// Clean up recovery states
    Cleanup {
        #[arg(help = "Specific transaction ID to clean up (optional)")]
        transaction_id: Option<String>,
        #[arg(
            long,
            help = "Clean up states older than specified duration (e.g., 7d, 24h)"
        )]
        older_than: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
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

/// Generate tool validation help text
fn get_tool_validation_help() -> String {
    let mut help = String::new();

    // Check git version
    let git_status = check_tool_version("git", "--version", "2.20.0");
    help.push_str("REQUIRED TOOLS:\n");
    help.push_str(&format!(
        "  {} {:<3} {:>12}\n",
        git_status.status_icon, "git", git_status.version
    ));

    // Check gh version
    let gh_status = check_tool_version("gh", "--version", "2.0.0");
    help.push_str(&format!(
        "  {} {:<3} {:>12}\n",
        gh_status.status_icon, "gh", gh_status.version
    ));

    help.push_str("\nLogs are written to: ~/.local/share/gx/logs/gx.log");
    help
}

#[derive(Debug)]
struct ToolStatus {
    version: String,
    status_icon: String,
}

/// Check if a tool is installed and meets minimum version requirements
fn check_tool_version(tool: &str, version_arg: &str, min_version: &str) -> ToolStatus {
    match Command::new(tool).arg(version_arg).output() {
        Ok(output) if output.status.success() => {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = extract_version_from_output(tool, &version_output);

            let meets_requirement = if let Some(stripped) = version.strip_prefix("v") {
                version_compare(stripped, min_version)
            } else {
                version_compare(&version, min_version)
            };

            ToolStatus {
                version: if version.is_empty() {
                    "unknown".to_string()
                } else {
                    version
                },
                status_icon: if meets_requirement { "✅" } else { "⚠️" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "❌".to_string(),
        },
    }
}

/// Extract version number from tool output
fn extract_version_from_output(tool: &str, output: &str) -> String {
    match tool {
        "git" => {
            // git version 2.34.1
            if let Some(line) = output.lines().next() {
                if let Some(version_part) = line.split_whitespace().nth(2) {
                    return version_part.to_string();
                }
            }
        }
        "gh" => {
            // gh version 2.40.1 (2023-12-13)
            if let Some(line) = output.lines().next() {
                if let Some(version_part) = line.split_whitespace().nth(2) {
                    return version_part.to_string();
                }
            }
        }
        _ => {}
    }
    "unknown".to_string()
}

/// Simple version comparison (assumes semantic versioning)
fn version_compare(version: &str, min_version: &str) -> bool {
    let parse_version =
        |v: &str| -> Vec<u32> { v.split('.').map(|part| part.parse().unwrap_or(0)).collect() };

    let v1 = parse_version(version);
    let v2 = parse_version(min_version);

    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }

    v1.len() >= v2.len()
}
