//! Common utility functions for gx subcommands

use crate::config::Config;

/// Get jobs from config, handling "nproc" string
pub fn get_jobs_from_config(config: &Config) -> Option<usize> {
    match config.jobs.as_deref()? {
        "nproc" => get_nproc(),
        jobs_str => jobs_str.parse().ok(),
    }
}

/// Get max depth from config
pub fn get_max_depth_from_config(config: &Config) -> Option<usize> {
    config.repo_discovery.as_ref()?.max_depth
}

/// Get number of processors using num_cpus crate
pub fn get_nproc() -> Option<usize> {
    Some(num_cpus::get())
}

/// Indent text by a specified number of spaces
pub fn indent(text: &str, spaces: usize) -> String {
    let padding = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{padding}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
