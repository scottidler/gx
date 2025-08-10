use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

static HELP_TEXT: LazyLock<String> = LazyLock::new(|| get_tool_validation_help());
static JOBS_HELP: LazyLock<String> = LazyLock::new(|| format!("Number of parallel operations [default: {}]", num_cpus::get()));
static DEPTH_HELP: LazyLock<String> = LazyLock::new(|| "Maximum directory depth to scan [default: 2]".to_string());

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

    /// Override parallelism
    #[arg(short = 'j', long = "jobs", value_name = "INT", help = JOBS_HELP.as_str())]
    pub parallel: Option<usize>,

    /// Override max repository discovery depth
    #[arg(short = 'm', long = "depth", value_name = "INT", help = DEPTH_HELP.as_str())]
    pub max_depth: Option<usize>,

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
  📍  No remote branch          ⚠️  Remote check error

EXAMPLES:
  gx status                     # Show all repositories
  gx status --detailed          # Show file-by-file details
  gx status frontend api        # Filter by repo patterns
  gx status --no-emoji          # Plain text for scripts")]
    Status {
        /// Show detailed file-by-file status
        #[arg(short, long, help = "Show detailed status instead of compact")]
        detailed: bool,

        /// Disable emoji output
        #[arg(long, help = "Disable emoji in output")]
        no_emoji: bool,

        /// Disable colored output
        #[arg(long, help = "Disable colored output")]
        no_color: bool,

        /// Repository name patterns to filter
        patterns: Vec<String>,
    },

    /// Checkout branches across multiple repositories
    #[command(after_help = "CHECKOUT LEGEND:
  🔄  Checked out and synced with remote    ✨  Created new branch from remote
  📦  Stashed uncommitted changes           ❌  Checkout failed (error)
  ⚠️   Has untracked files                  📊  Summary stats

EXAMPLES:
  gx checkout feature-branch        # Checkout existing branch and sync with remote
  gx checkout -b new-feature        # Create and checkout new branch in all repos
  gx checkout -b fix -f main        # Create branch from specific base branch
  gx checkout main -s               # Checkout main and stash uncommitted changes
  gx checkout main frontend api     # Checkout main in filtered repos")]
    Checkout {
        /// Create a new branch
        #[arg(short = 'b', long = "branch", help = "Create and checkout a new branch")]
        create_branch: bool,

        /// Base branch to create from (defaults to current HEAD)
        #[arg(short = 'f', long = "from", value_name = "BRANCH", help = "Base branch for new branch creation [Default: main|master]")]
        from_branch: Option<String>,

        /// Stash uncommitted changes before checkout
        #[arg(short = 's', long = "stash", help = "Stash uncommitted changes before checkout")]
        stash: bool,

        /// Branch name to checkout or create
        #[arg(value_name = "BRANCH")]
        branch_name: String,

        /// Repository name patterns to filter
        patterns: Vec<String>,
    },

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
}

/// Generate tool validation help text
fn get_tool_validation_help() -> String {
    let mut help = String::new();

    // Check git version
    let git_status = check_tool_version("git", "--version", "2.20.0");
    help.push_str("REQUIRED TOOLS:\n");
    help.push_str(&format!("  {} {:<3} {:>12}\n", git_status.status_icon, "git", git_status.version));

    // Check gh version
    let gh_status = check_tool_version("gh", "--version", "2.0.0");
    help.push_str(&format!("  {} {:<3} {:>12}\n", gh_status.status_icon, "gh", gh_status.version));

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

            let meets_requirement = if version.starts_with("v") {
                version_compare(&version[1..], min_version)
            } else {
                version_compare(&version, min_version)
            };

            ToolStatus {
                version: if version.is_empty() { "unknown".to_string() } else { version },
                status_icon: if meets_requirement { "✅" } else { "⚠️" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "❌".to_string(),
        }
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
    let parse_version = |v: &str| -> Vec<u32> {
        v.split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };

    let v1 = parse_version(version);
    let v2 = parse_version(min_version);

    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b { return true; }
        if a < b { return false; }
    }

    v1.len() >= v2.len()
}
