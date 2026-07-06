use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
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

/// Log verbosity, mirroring `log::LevelFilter`. Case-insensitive on the CLI.
#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn to_filter(self) -> log::LevelFilter {
        match self {
            LogLevel::Off => log::LevelFilter::Off,
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Trace => log::LevelFilter::Trace,
        }
    }
}

/// Validate a `--change-id`: it must start with `GX-` so the review tooling can
/// find its PRs by that prefix ([A11]). Rejected at parse time.
fn validate_change_id(value: &str) -> Result<String, String> {
    if value.starts_with("GX-") {
        Ok(value.to_string())
    } else {
        Err(format!(
            "change-id must start with 'GX-' (got '{value}'); gx review finds PRs by the GX- prefix"
        ))
    }
}

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
    version = env!("GIT_DESCRIBE")
)]
pub struct Cli {
    /// Working directory (only changes from current directory if specified)
    #[arg(long, help = "Working directory for operations")]
    pub cwd: Option<PathBuf>,

    /// Log verbosity (replaces RUST_LOG)
    #[arg(
        short = 'l',
        long = "log-level",
        value_enum,
        ignore_case = true,
        default_value = "info",
        help = "Log verbosity: off|error|warn|info|debug|trace"
    )]
    pub log_level: LogLevel,

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
  🟢  Up to date with remote    ↑N  Ahead by N commits
  ↓N  Behind by N commits       🔀  Diverged (ahead+behind)
  📍  No remote branch          🚨git Remote check error (git command failed)

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
  🚨  Has untracked files                  📊  Summary stats

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
  📍  Checked out default branch          🚨  Clone/update failed
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
  👀  Dry run (would change)  ➖  Dry run (no change)
  💾  Changes committed        ❌  Error occurred

EXAMPLES:
  gx create --files '*.json'                                    # Show matching files (dry-run)
  gx create --files '*.json' -p frontend                        # Show matches in frontend repos only
  gx create --files '*.json' add config.json '{\"debug\": true}' # Create files (dry-run)
  gx create --files '*.md' --commit 'Update docs' sub 'old-text' 'new-text'
  gx create --files 'package.json' --commit 'Bump version' regex '\"version\": \"[^\"]+\"' '\"version\": \"1.2.3\"'
  gx create --files '*.txt' --commit 'Remove old files' --pr delete
  gx create --files '*.md' --commit 'Draft update' --pr=draft sub 'old' 'new'")]
    Create {
        /// Files to target (glob patterns)
        #[arg(short = 'f', long = "files", help = "File patterns to match")]
        files: Vec<String>,

        /// Change ID for branch and PR naming
        #[arg(
            short = 'x',
            long = "change-id",
            help = "Change ID for branch/PR (auto-generated if not provided)",
            value_parser = validate_change_id
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

        /// Skip the confirmation prompt before committing (for automation)
        #[arg(
            short = 'y',
            long = "yes",
            help = "Skip the confirmation prompt before committing"
        )]
        yes: bool,

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

    /// Clean up branches after PR merge
    #[command(after_help = "CLEANUP LEGEND:
  🧹  Local branch deleted     🌐  Remote branch deleted
  ⏭️   Already cleaned          🚨  Still has open PR
  ❌  Cleanup failed            📊  Summary stats

EXAMPLES:
  gx cleanup GX-2024-01-15           # Clean up specific change
  gx cleanup --all                   # Clean up all merged changes
  gx cleanup --list                  # List changes needing cleanup")]
    Cleanup {
        /// Change ID to clean up (optional if --all or --list)
        #[arg(value_name = "CHANGE_ID")]
        change_id: Option<String>,

        /// Clean up all merged changes
        #[arg(long, conflicts_with = "change_id")]
        all: bool,

        /// List changes that can be cleaned up
        #[arg(long, conflicts_with = "change_id", conflicts_with = "all")]
        list: bool,

        /// Also delete remote branches (if not auto-deleted)
        #[arg(long)]
        include_remote: bool,

        /// Force cleanup even if PR status is unknown
        #[arg(long)]
        force: bool,
    },

    /// Check required tools and report orphaned gx artifacts
    #[command(after_help = "EXAMPLES:
  gx doctor            # Check git/gh versions and list orphaned artifacts
  gx doctor --purge    # Also remove orphaned recovery/backup artifacts (via rkvr)")]
    Doctor {
        /// Remove orphaned recovery/backup artifacts (via rkvr, not rm)
        #[arg(
            long,
            help = "Remove orphaned recovery/backup artifacts via rkvr"
        )]
        purge: bool,
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
        #[arg(
            long,
            help = "Enable auto-merge (merge when all checks pass)"
        )]
        auto: bool,
    },
    /// Delete PRs and branches
    Delete {
        #[arg(help = "Change ID to delete")]
        change_id: String,
    },
    /// Purge gx-created branches with no open PR
    Purge {
        /// Skip the confirmation prompt before deleting branches
        #[arg(
            short = 'y',
            long = "yes",
            help = "Skip the confirmation prompt before purging"
        )]
        yes: bool,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_change_id_accepts_gx_prefix() {
        assert_eq!(
            validate_change_id("GX-2026-06-11").unwrap(),
            "GX-2026-06-11"
        );
    }

    #[test]
    fn test_validate_change_id_rejects_non_gx() {
        assert!(validate_change_id("my-change").is_err());
        assert!(validate_change_id("gx-lowercase").is_err());
        assert!(validate_change_id("").is_err());
    }
}
