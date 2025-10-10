use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
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
}

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
#[serde(default)]
pub struct OutputConfig {
    pub verbosity: Option<OutputVerbosity>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RepoDiscoveryConfig {
    #[serde(rename = "max-depth")]
    pub max_depth: Option<usize>,
    #[serde(rename = "ignore-patterns")]
    pub ignore_patterns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: Option<String>,
    pub file: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
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
            ignore_patterns: Some(vec![
                "node_modules".to_string(),
                ".git".to_string(),
                "target".to_string(),
                "build".to_string(),
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
    /// Load configuration with fallback chain
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        // If explicit config path provided, try to load it
        if let Some(path) = config_path {
            return Self::load_from_file(path)
                .context(format!("Failed to load config from {}", path.display()));
        }

        // Try primary location: ~/.config/<project>/<project>.yml
        if let Some(config_dir) = dirs::config_dir() {
            let project_name = env!("CARGO_PKG_NAME");
            let primary_config = config_dir
                .join(project_name)
                .join(format!("{project_name}.yml"));
            if primary_config.exists() {
                match Self::load_from_file(&primary_config) {
                    Ok(config) => return Ok(config),
                    Err(e) => {
                        log::warn!(
                            "Failed to load config from {}: {}",
                            primary_config.display(),
                            e
                        );
                    }
                }
            }
        }

        // Try fallback location: ./<project>.yml
        let project_name = env!("CARGO_PKG_NAME");
        let fallback_config = PathBuf::from(format!("{project_name}.yml"));
        if fallback_config.exists() {
            match Self::load_from_file(&fallback_config) {
                Ok(config) => return Ok(config),
                Err(e) => {
                    log::warn!(
                        "Failed to load config from {}: {}",
                        fallback_config.display(),
                        e
                    );
                }
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
