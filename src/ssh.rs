use eyre::{Context, Result};
use log::debug;
use std::process::Command;

/// SSH URL construction and validation
pub struct SshUrlBuilder;

impl SshUrlBuilder {
    /// Convert repository slug to SSH URL
    pub fn build_ssh_url(repo_slug: &str) -> Result<String> {
        // Validate repo slug format (should be "org/repo")
        let parts: Vec<&str> = repo_slug.split('/').collect();
        if parts.len() != 2 {
            return Err(eyre::eyre!(
                "Invalid repository slug format. Expected 'org/repo', got '{}'",
                repo_slug
            ));
        }

        // Validate parts are not empty
        if parts[0].is_empty() || parts[1].is_empty() {
            return Err(eyre::eyre!("Repository slug parts cannot be empty: '{}'", repo_slug));
        }

        Ok(format!("git@github.com:{repo_slug}.git"))
    }

    /// Validate SSH URL format
    pub fn validate_ssh_url(url: &str) -> Result<()> {
        if !url.starts_with("git@github.com:") {
            return Err(eyre::eyre!(
                "Invalid SSH URL format. Expected to start with 'git@github.com:', got '{}'",
                url
            ));
        }

        if !url.ends_with(".git") {
            return Err(eyre::eyre!(
                "Invalid SSH URL format. Expected to end with '.git', got '{}'",
                url
            ));
        }

        // Extract the repo part and validate
        let repo_part = url
            .strip_prefix("git@github.com:")
            .and_then(|s| s.strip_suffix(".git"))
            .ok_or_else(|| eyre::eyre!("Failed to extract repository part from URL: '{}'", url))?;

        let parts: Vec<&str> = repo_part.split('/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(eyre::eyre!("Invalid repository format in SSH URL: '{}'", repo_part));
        }

        Ok(())
    }
}

/// SSH command detection and configuration
pub struct SshCommandDetector;

impl SshCommandDetector {
    /// Get SSH command from git configuration
    pub fn get_ssh_command() -> Result<String> {
        let output = Command::new("git")
            .args(["config", "--get", "core.sshCommand"])
            .output()
            .context("Failed to execute git config command")?;

        if output.status.success() {
            let ssh_command = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !ssh_command.is_empty() {
                debug!("Using SSH command from git config: {ssh_command}");
                return Ok(ssh_command);
            }
        }

        // Fall back to default SSH
        debug!("Using default SSH command");
        Ok("ssh".to_string())
    }

    /// Test SSH connectivity to GitHub
    pub fn test_github_ssh_connection() -> Result<String> {
        debug!("Testing SSH connectivity to GitHub");

        let output = Command::new("ssh")
            .args(["-T", "git@github.com"])
            .output()
            .context("Failed to execute SSH test command")?;

        // SSH to GitHub should return exit code 1 with success message
        // Exit code 0 would mean shell access (which GitHub doesn't provide)
        // Exit code 255 or other would mean connection/auth failure
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        debug!("SSH test exit code: {:?}", output.status.code());
        debug!("SSH test stderr: {stderr}");
        debug!("SSH test stdout: {stdout}");

        // GitHub SSH test returns message on stderr like "Hi username! You've successfully authenticated..."
        if stderr.contains("You've successfully authenticated") {
            // Extract username from the message
            if let Some(username_start) = stderr.find("Hi ") {
                if let Some(username_end) = stderr[username_start + 3..].find('!') {
                    let username = &stderr[username_start + 3..username_start + 3 + username_end];
                    debug!("SSH authenticated as: {username}");
                    return Ok(username.to_string());
                }
            }
            // Fallback if we can't parse username
            return Ok("authenticated".to_string());
        }

        // Check for common SSH error patterns
        if stderr.contains("Permission denied") {
            return Err(eyre::eyre!(
                "SSH authentication failed: Permission denied. Check your SSH keys."
            ));
        }

        if stderr.contains("Host key verification failed") {
            return Err(eyre::eyre!("SSH host key verification failed. Run 'ssh-keyscan github.com >> ~/.ssh/known_hosts' to add GitHub's host key."));
        }

        if stderr.contains("Connection refused") || stderr.contains("Network is unreachable") {
            return Err(eyre::eyre!(
                "SSH network error: Cannot connect to GitHub. Check your network connection."
            ));
        }

        // Generic failure
        Err(eyre::eyre!(
            "SSH connection test failed. stderr: {}, stdout: {}",
            stderr.trim(),
            stdout.trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_ssh_url_valid() {
        let result = SshUrlBuilder::build_ssh_url("scottidler/gx");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "git@github.com:scottidler/gx.git");
    }

    #[test]
    fn test_build_ssh_url_valid_complex() {
        let result = SshUrlBuilder::build_ssh_url("tatari-tv/frontend-api");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "git@github.com:tatari-tv/frontend-api.git");
    }

    #[test]
    fn test_build_ssh_url_invalid_format() {
        let result = SshUrlBuilder::build_ssh_url("invalid");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid repository slug format"));
    }

    #[test]
    fn test_build_ssh_url_empty_parts() {
        let result = SshUrlBuilder::build_ssh_url("/repo");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Repository slug parts cannot be empty"));

        let result = SshUrlBuilder::build_ssh_url("org/");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Repository slug parts cannot be empty"));
    }

    #[test]
    fn test_build_ssh_url_too_many_parts() {
        let result = SshUrlBuilder::build_ssh_url("org/repo/extra");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid repository slug format"));
    }

    #[test]
    fn test_validate_ssh_url_valid() {
        let result = SshUrlBuilder::validate_ssh_url("git@github.com:scottidler/gx.git");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_ssh_url_invalid_prefix() {
        let result = SshUrlBuilder::validate_ssh_url("https://github.com/scottidler/gx.git");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Expected to start with 'git@github.com:'"));
    }

    #[test]
    fn test_validate_ssh_url_invalid_suffix() {
        let result = SshUrlBuilder::validate_ssh_url("git@github.com:scottidler/gx");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Expected to end with '.git'"));
    }

    #[test]
    fn test_validate_ssh_url_invalid_repo_format() {
        let result = SshUrlBuilder::validate_ssh_url("git@github.com:invalid.git");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid repository format"));
    }

    #[test]
    fn test_get_ssh_command_returns_string() {
        // This test just ensures the function runs without panicking
        // The actual git config may or may not be set
        let result = SshCommandDetector::get_ssh_command();
        assert!(result.is_ok());
        let ssh_command = result.unwrap();
        assert!(!ssh_command.is_empty());
    }
}
