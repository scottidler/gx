use eyre::{Context, Result};
use log::debug;
use std::fs;
use std::process::Command;

/// Get all repositories for a user/org from GitHub API
pub fn get_user_repos(user_or_org: &str, include_archived: bool) -> Result<Vec<String>> {
    debug!("Getting repos for user/org: {}, include_archived: {}", user_or_org, include_archived);

    // Read token from ~/.config/github/tokens/{user_or_org} (plain text)
    let token_path = dirs::config_dir()
        .unwrap_or_default()
        .join("github")
        .join("tokens")
        .join(user_or_org);

    let token = fs::read_to_string(&token_path)
        .context(format!("Failed to read token from {}", token_path.display()))?
        .trim()
        .to_string();

    debug!("Using token from: {}", token_path.display());

    // Query GitHub API - try both user and org endpoints
    let archived_filter = if include_archived {
        ""
    } else {
        " | select(.archived == false)"
    };

    // First try as an organization
    let org_query = format!("orgs/{}/repos", user_or_org);
    let org_result = query_github_repos(&org_query, &token, archived_filter);

    if let Ok(repos) = org_result {
        if !repos.is_empty() {
            debug!("Found {} repos for org: {}", repos.len(), user_or_org);
            return Ok(repos);
        }
    }

    // If org query failed or returned no results, try as a user
    let user_query = format!("users/{}/repos", user_or_org);
    let user_result = query_github_repos(&user_query, &token, archived_filter);

    match user_result {
        Ok(repos) => {
            debug!("Found {} repos for user: {}", repos.len(), user_or_org);
            Ok(repos)
        }
        Err(e) => {
            // If both failed, return the user error (more common case)
            Err(e).context(format!("Failed to get repositories for {}", user_or_org))
        }
    }
}

/// Query GitHub API for repositories
fn query_github_repos(query: &str, token: &str, archived_filter: &str) -> Result<Vec<String>> {
    let output = Command::new("gh")
        .env("GH_TOKEN", token)
        .args([
            "api",
            query,
            "--paginate",
            "--jq",
            &format!(".[]{}  | .full_name", archived_filter),
        ])
        .output()
        .context("Failed to execute gh command")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("GitHub API query failed: {}", error));
    }

    let repos = String::from_utf8(output.stdout)?
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    Ok(repos)
}

/// Get default branch for a repository
pub fn get_default_branch(repo_slug: &str, token: &str) -> Result<String> {
    debug!("Getting default branch for repo: {}", repo_slug);

    let output = Command::new("gh")
        .env("GH_TOKEN", token)
        .args([
            "api",
            &format!("repos/{}", repo_slug),
            "--jq",
            ".default_branch",
        ])
        .output()
        .context("Failed to get default branch")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to get default branch: {}", error));
    }

    let branch = String::from_utf8(output.stdout)?.trim().to_string();
    debug!("Default branch for {}: {}", repo_slug, branch);
    Ok(branch)
}

/// Read GitHub token for a user/org
pub fn read_token(user_or_org: &str) -> Result<String> {
    let token_path = dirs::config_dir()
        .unwrap_or_default()
        .join("github")
        .join("tokens")
        .join(user_or_org);

    let token = fs::read_to_string(&token_path)
        .context(format!("Failed to read token from {}", token_path.display()))?
        .trim()
        .to_string();

    Ok(token)
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_query_parsing() {
        // Test that we can parse repository names correctly
        let test_output = "owner/repo1\nowner/repo2\nowner/repo3\n";
        let repos: Vec<String> = test_output
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();

        assert_eq!(repos.len(), 3);
        assert_eq!(repos[0], "owner/repo1");
        assert_eq!(repos[1], "owner/repo2");
        assert_eq!(repos[2], "owner/repo3");
    }
}
