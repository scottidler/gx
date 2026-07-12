use eyre::{Context, Result};
use log::debug;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// XDG config dir, honoring `$XDG_CONFIG_HOME` and falling back to `$HOME/.config`.
fn xdg_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    dirs::home_dir().map(|h| h.join(".config"))
}

/// XDG data dir, honoring `$XDG_DATA_HOME` and falling back to `$HOME/.local/share`.
///
/// We deliberately do NOT use the `dirs` config/data helpers: those honor
/// `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` only on Linux. On macOS they resolve via system
/// APIs and return `~/Library/...`, ignoring the env vars. These helpers resolve to the
/// same XDG layout on every platform.
pub fn xdg_data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    dirs::home_dir().map(|h| h.join(".local").join("share"))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "default-user-org")]
    pub default_user_org: Option<String>,
    pub jobs: Option<String>, // Can be "nproc" or a number
    #[serde(rename = "token-path")]
    pub token_path: Option<String>,
    pub output: Option<OutputConfig>,
    #[serde(rename = "repo-discovery")]
    pub repo_discovery: Option<RepoDiscoveryConfig>,
    pub logging: Option<LoggingConfig>,
    #[serde(rename = "remote-status")]
    pub remote_status: Option<RemoteStatusConfig>,
    pub create: Option<CreateConfig>,
    pub github: Option<GithubConfig>,
}

/// GitHub-related configuration.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    /// Template for PR bodies. `{commit_message}` is substituted.
    #[serde(rename = "pr-body-template")]
    pub pr_body_template: Option<String>,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            pr_body_template: Some(DEFAULT_PR_BODY_TEMPLATE.to_string()),
        }
    }
}

/// Default PR body: just the commit message.
pub const DEFAULT_PR_BODY_TEMPLATE: &str = "{commit_message}";

/// Configuration for the `create` command.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CreateConfig {
    /// Prompt before committing when more repositories than this are targeted.
    #[serde(rename = "confirm-threshold")]
    pub confirm_threshold: Option<usize>,
    /// Settings for the `llm` change type (agent-per-repo propose/apply).
    pub llm: Option<LlmConfig>,
}

impl Default for CreateConfig {
    fn default() -> Self {
        Self {
            confirm_threshold: Some(DEFAULT_CONFIRM_THRESHOLD),
            llm: Some(LlmConfig::default()),
        }
    }
}

/// Default confirm-threshold: prompt when committing to more repos than this.
pub const DEFAULT_CONFIRM_THRESHOLD: usize = 5;

/// Configuration for the `gx create ... llm` change type (design doc
/// `2026-07-12-llm-propose-apply-and-mcp-server.md`, Config additions): the
/// agent command run per repo in a throwaway worktree, and the wall-clock
/// timeout after which its whole process group is killed.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    /// Command line for the agent; the prompt is appended as the final argument
    /// and the CWD is the temp worktree.
    #[serde(rename = "agent-command")]
    pub agent_command: Option<String>,
    /// Wall-clock timeout per repo, in seconds. On expiry the agent's entire
    /// process group is killed.
    #[serde(rename = "timeout-seconds")]
    pub timeout_seconds: Option<u64>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            agent_command: Some(DEFAULT_LLM_AGENT_COMMAND.to_string()),
            timeout_seconds: Some(DEFAULT_LLM_TIMEOUT_SECONDS),
        }
    }
}

/// Default agent command. The design doc's bare `claude -p --output-format
/// text` is INSUFFICIENT: Phase 0's live spike proved that in print (`-p`) mode
/// Claude Code will not edit files without an edit-granting permission mode, so
/// every propose would be a false "empty" outcome. `--permission-mode
/// acceptEdits` is the least-privilege fix (grants file edits without
/// auto-approving arbitrary Bash/network); recorded as the Phase 0 deviation.
pub const DEFAULT_LLM_AGENT_COMMAND: &str =
    "claude -p --output-format text --permission-mode acceptEdits";

/// Default per-repo agent timeout (design Performance: 300s per repo).
pub const DEFAULT_LLM_TIMEOUT_SECONDS: u64 = 300;

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum OutputVerbosity {
    Compact, // only summary output for the repos that had any errors; skip successful ones in the output
    #[default]
    Summary, // only the summary of every repo, success or failure
    Detailed, // show the detailed output only for failures, successes still remain as summary
    Full,    // show the detailed output for all repos irrespective of errors or not
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub verbosity: Option<OutputVerbosity>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepoDiscoveryConfig {
    #[serde(rename = "max-depth")]
    pub max_depth: Option<usize>,
    #[serde(rename = "ignore-patterns")]
    pub ignore_patterns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: Option<String>,
    pub file: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteStatusConfig {
    pub enabled: Option<bool>,
    #[serde(rename = "fetch-first")]
    pub fetch_first: Option<bool>,
    #[serde(rename = "timeout-seconds")]
    pub timeout_seconds: Option<u32>,
}

impl Default for RemoteStatusConfig {
    fn default() -> Self {
        Self {
            enabled: Some(true),
            fetch_first: Some(false),
            timeout_seconds: Some(10),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_user_org: None,
            jobs: None,
            token_path: Some("~/.config/github/tokens/{user_or_org}".to_string()),
            output: None,
            repo_discovery: Some(RepoDiscoveryConfig::default()),
            logging: None,
            remote_status: Some(RemoteStatusConfig::default()),
            create: Some(CreateConfig::default()),
            github: Some(GithubConfig::default()),
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            verbosity: Some(OutputVerbosity::Summary),
        }
    }
}

impl Default for RepoDiscoveryConfig {
    fn default() -> Self {
        Self {
            max_depth: Some(3), // Default changed from 2 to 3
            // The documented defaults; formerly hardcoded in is_ignored_directory
            // ([A27]). NOTE: `.git` must NOT appear here - discovery detects repos
            // by walking into `.git` directories, so ignoring them by name would
            // make every repo undiscoverable.
            ignore_patterns: Some(vec![
                "node_modules".to_string(),
                "target".to_string(),
                "build".to_string(),
                ".next".to_string(),
                "dist".to_string(),
                "vendor".to_string(),
            ]),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: Some("info".to_string()),
            file: Some("~/.local/share/gx/logs/gx.log".to_string()),
        }
    }
}

impl Config {
    /// Effective repo-discovery ignore patterns: the configured list, or the
    /// documented defaults when unset.
    pub fn ignore_patterns(&self) -> Vec<String> {
        self.repo_discovery
            .as_ref()
            .and_then(|rd| rd.ignore_patterns.clone())
            .unwrap_or_else(|| {
                RepoDiscoveryConfig::default()
                    .ignore_patterns
                    .unwrap_or_default()
            })
    }

    /// Effective confirm-threshold for the create command.
    pub fn confirm_threshold(&self) -> usize {
        self.create
            .as_ref()
            .and_then(|c| c.confirm_threshold)
            .unwrap_or(DEFAULT_CONFIRM_THRESHOLD)
    }

    /// Effective agent command for the `llm` change type.
    pub fn llm_agent_command(&self) -> String {
        self.create
            .as_ref()
            .and_then(|c| c.llm.as_ref())
            .and_then(|l| l.agent_command.clone())
            .unwrap_or_else(|| DEFAULT_LLM_AGENT_COMMAND.to_string())
    }

    /// Effective per-repo agent timeout (seconds) for the `llm` change type.
    pub fn llm_timeout_seconds(&self) -> u64 {
        self.create
            .as_ref()
            .and_then(|c| c.llm.as_ref())
            .and_then(|l| l.timeout_seconds)
            .unwrap_or(DEFAULT_LLM_TIMEOUT_SECONDS)
    }

    /// Effective PR body template (`{commit_message}` is substituted).
    pub fn pr_body_template(&self) -> String {
        self.github
            .as_ref()
            .and_then(|g| g.pr_body_template.clone())
            .unwrap_or_else(|| DEFAULT_PR_BODY_TEMPLATE.to_string())
    }

    /// Load configuration with fallback chain
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        debug!("Config::load: config_path={config_path:?}");
        // If explicit config path provided, try to load it
        if let Some(path) = config_path {
            return Self::load_from_file(path)
                .context(format!("Failed to load config from {}", path.display()));
        }

        // Primary (and only) location: $XDG_CONFIG_HOME/<project>/<project>.yml.
        // There is deliberately NO `./<project>.yml` CWD fallback - any directory
        // could otherwise reconfigure the tool (e.g. redirect token-path) ([A23]).
        if let Some(config_dir) = xdg_config_dir() {
            let project_name = env!("CARGO_PKG_NAME");
            let primary_config = config_dir
                .join(project_name)
                .join(format!("{project_name}.yml"));
            if primary_config.exists() {
                // A file that exists but fails to parse (a typo'd key under
                // `deny_unknown_fields`, bad YAML, ...) must fail loudly, not
                // be swallowed into a silent default - that was the exact bug
                // this house rule exists to close.
                return Self::load_from_file(&primary_config).context(format!(
                    "Failed to load config from {}",
                    primary_config.display()
                ));
            }
        }

        // No config file found, use defaults
        log::info!("No config file found, using defaults");
        Ok(Self::default())
    }

    fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(&path).context("Failed to read config file")?;

        let config: Self = serde_yaml::from_str(&content).context("Failed to parse config file")?;

        log::info!("Loaded config from: {}", path.as_ref().display());
        Ok(config)
    }
}

#[cfg(test)]
mod tests;
