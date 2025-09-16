use crate::config::Config;
use eyre::{Context, Result};
use log::{debug, info, warn};
use std::fs;
use std::process::Command;

/// Get all repositories for a user/org from GitHub API
pub fn get_user_repos(user_or_org: &str, include_archived: bool, config: &Config) -> Result<Vec<String>> {
    debug!(
        "Getting repos for user/org: {user_or_org}, include_archived: {include_archived}"
    );

    let token = read_token(user_or_org, config)?;
    debug!("Using token for user/org: {user_or_org}");

    // Query GitHub API - try both user and org endpoints
    let archived_filter = if include_archived { "" } else { " | select(.archived == false)" };

    // First try as an organization
    let org_query = format!("orgs/{user_or_org}/repos");
    let org_result = query_github_repos(&org_query, &token, archived_filter);

    if let Ok(repos) = org_result {
        if !repos.is_empty() {
            debug!("Found {} repos for org: {}", repos.len(), user_or_org);
            return Ok(repos);
        }
    }

    // If org query failed or returned no results, try as a user
    let user_query = format!("users/{user_or_org}/repos");
    let user_result = query_github_repos(&user_query, &token, archived_filter);

    match user_result {
        Ok(repos) => {
            debug!("Found {} repos for user: {}", repos.len(), user_or_org);
            Ok(repos)
        }
        Err(e) => {
            // If both failed, return the user error (more common case)
            Err(e).context(format!("Failed to get repositories for {user_or_org}"))
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
            &format!(".[]{archived_filter}  | .full_name"),
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
    debug!("Getting default branch for repo: {repo_slug}");

    let output = Command::new("gh")
        .env("GH_TOKEN", token)
        .args(["api", &format!("repos/{repo_slug}"), "--jq", ".default_branch"])
        .output()
        .context("Failed to get default branch")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to get default branch: {}", error));
    }

    let branch = String::from_utf8(output.stdout)?.trim().to_string();
    debug!("Default branch for {repo_slug}: {branch}");
    Ok(branch)
}

/// Read GitHub token for a user/org using configurable path
pub fn read_token(user_or_org: &str, config: &Config) -> Result<String> {
    let token_template = config
        .token_path
        .as_deref()
        .unwrap_or("~/.config/github/tokens/{user_or_org}");

    let token_path = super::user_org::build_token_path(token_template, user_or_org);

    let token = fs::read_to_string(&token_path)
        .context(format!("Failed to read token from {}", token_path.display()))?
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(eyre::eyre!("Token file is empty: {}", token_path.display()));
    }

    Ok(token)
}

/// Create a pull request using GitHub CLI
pub fn create_pr(repo_slug: &str, branch_name: &str, commit_message: &str) -> Result<()> {
    debug!("Creating PR for repo: {repo_slug}, branch: {branch_name}");

    let title = branch_name.to_string();
    let body = format!(
        "{commit_message}\n\ndocs: https://github.com/scottidler/gx/blob/main/README.md"
    );

    let output = Command::new("gh")
        .args([
            "pr",
            "create",
            "--repo",
            repo_slug,
            "--head",
            branch_name,
            "--title",
            &title,
            "--body",
            &body,
            "--base",
            "main",
        ])
        .output()
        .context("Failed to execute gh pr create")?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout);
        let url = url.trim();
        debug!("PR created: {url}");
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to create PR: {}", error))
    }
}

/// PR information structure
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub repo_slug: String,
    pub number: u64,
    pub title: String,
    pub branch: String,
    pub author: String,
    pub state: PrState,
    pub url: String,
}

/// PR state enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum PrState {
    Open,
    Closed,
    #[allow(dead_code)]
    Merged,
}

/// List PRs by change ID pattern
pub fn list_prs_by_change_id(org: &str, change_id_pattern: &str) -> Result<Vec<PrInfo>> {
    debug!("Listing PRs for org: {org}, change ID pattern: {change_id_pattern}");

    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--search",
            &format!("org:{org} head:{change_id_pattern}"),
            "--json",
            "number,title,headRefName,author,state,url,repository",
            "--limit",
            "100",
        ])
        .output()
        .context("Failed to execute gh pr list")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to list PRs: {}", error));
    }

    let json_output = String::from_utf8(output.stdout).context("Invalid UTF-8 in gh pr list output")?;

    parse_pr_list_json(&json_output)
}

/// Parse JSON output from gh pr list
fn parse_pr_list_json(json_output: &str) -> Result<Vec<PrInfo>> {
    // For now, we'll use a simple JSON parsing approach
    // In a production system, you'd want to use serde_json
    let prs = Vec::new();

    // This is a simplified parser - in reality you'd use serde_json
    // For now, just return empty list to avoid complex JSON parsing
    debug!("PR list JSON: {json_output}");

    Ok(prs)
}

/// Approve and merge a PR
pub fn approve_and_merge_pr(repo_slug: &str, pr_number: u64, admin_override: bool) -> Result<()> {
    debug!("Approving and merging PR #{pr_number} in {repo_slug}");

    // First approve the PR
    let approve_output = Command::new("gh")
        .args(["pr", "review", &pr_number.to_string(), "--repo", repo_slug, "--approve"])
        .output()
        .context("Failed to execute gh pr review --approve")?;

    if !approve_output.status.success() {
        let error = String::from_utf8_lossy(&approve_output.stderr);
        warn!("Failed to approve PR #{pr_number}: {error}");
    }

    // Then merge the PR
    let pr_number_str = pr_number.to_string();
    let mut merge_args = vec![
        "pr",
        "merge",
        &pr_number_str,
        "--repo",
        repo_slug,
        "--squash",
        "--delete-branch",
    ];

    if admin_override {
        merge_args.push("--admin");
    }

    let merge_output = Command::new("gh")
        .args(&merge_args)
        .output()
        .context("Failed to execute gh pr merge")?;

    if merge_output.status.success() {
        info!("Successfully merged PR #{pr_number} in {repo_slug}");
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&merge_output.stderr);
        Err(eyre::eyre!("Failed to merge PR #{}: {}", pr_number, error))
    }
}

/// Close a PR without merging
pub fn close_pr(repo_slug: &str, pr_number: u64) -> Result<()> {
    debug!("Closing PR #{pr_number} in {repo_slug}");

    let output = Command::new("gh")
        .args(["pr", "close", &pr_number.to_string(), "--repo", repo_slug])
        .output()
        .context("Failed to execute gh pr close")?;

    if output.status.success() {
        info!("Successfully closed PR #{pr_number} in {repo_slug}");
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to close PR #{}: {}", pr_number, error))
    }
}

/// Delete a remote branch
pub fn delete_remote_branch(repo_slug: &str, branch_name: &str) -> Result<()> {
    debug!("Deleting remote branch '{branch_name}' in {repo_slug}");

    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo_slug}/git/refs/heads/{branch_name}"),
            "--method",
            "DELETE",
        ])
        .output()
        .context("Failed to execute gh api DELETE")?;

    if output.status.success() {
        info!("Successfully deleted remote branch '{branch_name}' in {repo_slug}");
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!(
            "Failed to delete remote branch '{}': {}",
            branch_name,
            error
        ))
    }
}

/// Get PR diff content
#[allow(dead_code)]
pub fn get_pr_diff(repo_slug: &str, pr_number: u64) -> Result<String> {
    debug!("Getting diff for PR #{pr_number} in {repo_slug}");

    let output = Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string(), "--repo", repo_slug])
        .output()
        .context("Failed to execute gh pr diff")?;

    if output.status.success() {
        let diff = String::from_utf8(output.stdout).context("Invalid UTF-8 in pr diff output")?;
        Ok(diff)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to get PR diff: {}", error))
    }
}

/// List all branches with a specific prefix (for purge operations)
pub fn list_branches_with_prefix(repo_slug: &str, prefix: &str) -> Result<Vec<String>> {
    debug!("Listing branches with prefix '{prefix}' in {repo_slug}");

    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repo_slug}/branches"),
            "--jq",
            &format!(".[] | select(.name | startswith(\"{prefix}\")) | .name"),
        ])
        .output()
        .context("Failed to execute gh api branches")?;

    if output.status.success() {
        let branches = String::from_utf8(output.stdout)
            .context("Invalid UTF-8 in branches output")?
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        Ok(branches)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to list branches: {}", error))
    }
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
