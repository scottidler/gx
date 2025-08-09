use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "gx",
    about = "git operations across multiple repositories",
    version = env!("GIT_DESCRIBE"),
    after_help = "Logs are written to: ~/.local/share/gx/logs/gx.log"
)]
pub struct Cli {
    /// Path to config file
    #[arg(short, long, help = "Path to config file")]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, help = "Enable verbose output")]
    pub verbose: bool,

    /// Override parallelism (default: nproc)
    #[arg(long, help = "Number of parallel operations")]
    pub parallel: Option<usize>,

    /// Override max repository discovery depth
    #[arg(long, help = "Maximum directory depth to scan")]
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
  📍  No remote branch          ⚠️   Remote check error

EXAMPLES:
  gx status                     # Show dirty repos only
  gx status --all              # Show all repos including clean
  gx status --detailed         # Show file-by-file details
  gx status frontend api       # Filter by repo patterns
  gx status --no-emoji         # Plain text for scripts")]
    Status {
        /// Show all repos including clean ones
        #[arg(short, long, help = "Show all repositories including clean ones")]
        all: bool,

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
}
