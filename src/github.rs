use crate::config::Config;
use eyre::{Context, Result};
use log::{debug, info, warn};
use serde::Deserialize;
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Maximum number of retry attempts for network operations
const MAX_RETRIES: u32 = 3;
/// Base delay between retries in milliseconds
const RETRY_BASE_DELAY_MS: u64 = 1000;

/// Result of creating a PR, containing the PR info
#[derive(Debug, Clone)]
pub struct CreatePrResult {
    pub number: u64,
    pub url: String,
}

/// Get all repositories for a user/org from GitHub API
pub fn get_user_repos(
    user_or_org: &str,
    include_archived: bool,
    config: &Config,
) -> Result<Vec<String>> {
    debug!("Getting repos for user/org: {user_or_org}, include_archived: {include_archived}");

    let token = read_token(user_or_org, config)?;
    debug!("Using token for user/org: {user_or_org}");

    // Query GitHub API - try both user and org endpoints
    let archived_filter = if include_archived {
        ""
    } else {
        " | select(.archived == false)"
    };

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
        .args([
            "api",
            &format!("repos/{repo_slug}"),
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
        .context(format!(
            "Failed to read token from {}",
            token_path.display()
        ))?
        .trim()
        .to_string();

    if token.is_empty() {
        return Err(eyre::eyre!("Token file is empty: {}", token_path.display()));
    }

    Ok(token)
}

/// Create a pull request using GitHub CLI
/// Returns the PR number and URL on success
pub fn create_pr(
    repo_slug: &str,
    branch_name: &str,
    commit_message: &str,
    pr: &crate::cli::PR,
) -> Result<CreatePrResult> {
    debug!("Creating PR for repo: {repo_slug}, branch: {branch_name}");

    let title = branch_name.to_string();
    let body =
        format!("{commit_message}\n\ndocs: https://github.com/scottidler/gx/blob/main/README.md");

    let mut args = vec![
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
    ];

    if matches!(pr, crate::cli::PR::Draft) {
        args.push("--draft");
    }

    // Use retry logic for network operations
    let output = retry_command("gh", &args, MAX_RETRIES)?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("PR created: {url}");

        // Extract PR number from URL (e.g., https://github.com/org/repo/pull/123)
        let number = extract_pr_number_from_url(&url).unwrap_or(0);

        Ok(CreatePrResult { number, url })
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to create PR: {}", error))
    }
}

/// Extract PR number from a GitHub PR URL
fn extract_pr_number_from_url(url: &str) -> Option<u64> {
    // URL format: https://github.com/owner/repo/pull/123
    url.rsplit('/').next()?.parse().ok()
}

/// Execute a command with retry logic and exponential backoff
fn retry_command(cmd: &str, args: &[&str], max_retries: u32) -> Result<std::process::Output> {
    let mut last_error = None;

    for attempt in 0..max_retries {
        let output = Command::new(cmd)
            .args(args)
            .output()
            .context(format!("Failed to execute {cmd}"))?;

        if output.status.success() {
            return Ok(output);
        }

        let error = String::from_utf8_lossy(&output.stderr);

        // Check if this is a retryable error (network, timeout, rate limit)
        if is_retryable_error(&error) && attempt < max_retries - 1 {
            let delay = RETRY_BASE_DELAY_MS * 2u64.pow(attempt);
            warn!(
                "Attempt {} failed, retrying in {}ms: {}",
                attempt + 1,
                delay,
                error.trim()
            );
            thread::sleep(Duration::from_millis(delay));
            last_error = Some(error.to_string());
        } else {
            // Non-retryable error or last attempt
            return Ok(output);
        }
    }

    Err(eyre::eyre!(
        "Command failed after {} attempts: {}",
        max_retries,
        last_error.unwrap_or_default()
    ))
}

/// Check if an error message indicates a retryable condition
fn is_retryable_error(error: &str) -> bool {
    let retryable_patterns = [
        "timeout",
        "timed out",
        "connection refused",
        "connection reset",
        "network",
        "rate limit",
        "too many requests",
        "503",
        "502",
        "504",
        "ETIMEDOUT",
        "ECONNRESET",
        "ENOTFOUND",
    ];

    let error_lower = error.to_lowercase();
    retryable_patterns
        .iter()
        .any(|pattern| error_lower.contains(pattern))
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
}

/// GraphQL response wrapper
#[derive(Debug, Deserialize)]
struct GhGraphqlResponse {
    data: GhGraphqlData,
}

#[derive(Debug, Deserialize)]
struct GhGraphqlData {
    search: GhGraphqlSearch,
}

#[derive(Debug, Deserialize)]
struct GhGraphqlSearch {
    nodes: Vec<GhGraphqlPrItem>,
}

/// Raw PR data from GitHub GraphQL API
#[derive(Debug, Deserialize)]
struct GhGraphqlPrItem {
    number: u64,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    author: GhGraphqlAuthor,
    state: String,
    url: String,
    repository: GhGraphqlRepository,
}

#[derive(Debug, Deserialize)]
struct GhGraphqlAuthor {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhGraphqlRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

/// List PRs by change ID pattern using GraphQL API
pub fn list_prs_by_change_id(org: &str, change_id_pattern: &str) -> Result<Vec<PrInfo>> {
    debug!("Listing PRs for org: {org}, change ID pattern: {change_id_pattern}");

    let query = format!(
        r#"{{
          search(query: "org:{} is:pr is:open head:{}", type: ISSUE, first: 100) {{
            nodes {{
              ... on PullRequest {{
                number
                title
                headRefName
                author {{ login }}
                state
                url
                repository {{ nameWithOwner }}
              }}
            }}
          }}
        }}"#,
        org, change_id_pattern
    );

    let output = Command::new("gh")
        .args(["api", "graphql", "-f", &format!("query={}", query)])
        .output()
        .context("Failed to execute gh api graphql")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to search PRs: {}", error));
    }

    let json_output =
        String::from_utf8(output.stdout).context("Invalid UTF-8 in gh api graphql output")?;

    parse_graphql_prs_json(&json_output)
}

/// Parse JSON output from gh api graphql
fn parse_graphql_prs_json(json_output: &str) -> Result<Vec<PrInfo>> {
    let trimmed = json_output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let response: GhGraphqlResponse =
        serde_json::from_str(trimmed).context("Failed to parse GraphQL response JSON")?;

    let prs: Vec<PrInfo> = response
        .data
        .search
        .nodes
        .into_iter()
        .map(|gh_pr| PrInfo {
            repo_slug: gh_pr.repository.name_with_owner,
            number: gh_pr.number,
            title: gh_pr.title,
            branch: gh_pr.head_ref_name,
            author: gh_pr.author.login,
            state: match gh_pr.state.to_uppercase().as_str() {
                "OPEN" => PrState::Open,
                _ => PrState::Closed,
            },
            url: gh_pr.url,
        })
        .collect();

    debug!("Parsed {} PRs from GraphQL response", prs.len());
    Ok(prs)
}

/// Approve and merge a PR
pub fn approve_and_merge_pr(repo_slug: &str, pr_number: u64, admin_override: bool) -> Result<()> {
    debug!("Approving and merging PR #{pr_number} in {repo_slug}");

    // First approve the PR
    let approve_output = Command::new("gh")
        .args([
            "pr",
            "review",
            &pr_number.to_string(),
            "--repo",
            repo_slug,
            "--approve",
        ])
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
    use super::*;

    #[test]
    fn test_query_parsing() {
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

    #[test]
    fn test_parse_graphql_prs_json_empty_string() {
        let result = parse_graphql_prs_json("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_graphql_prs_json_empty_nodes() {
        let json = r#"{"data":{"search":{"nodes":[]}}}"#;
        let result = parse_graphql_prs_json(json).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_graphql_prs_json_single_pr() {
        let json = r#"{"data":{"search":{"nodes":[{
            "number": 123,
            "title": "GX-2024-01-15: Update configs",
            "headRefName": "GX-2024-01-15",
            "author": {"login": "testuser"},
            "state": "OPEN",
            "url": "https://github.com/org/repo/pull/123",
            "repository": {"nameWithOwner": "org/repo"}
        }]}}}"#;

        let result = parse_graphql_prs_json(json).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].number, 123);
        assert_eq!(result[0].title, "GX-2024-01-15: Update configs");
        assert_eq!(result[0].branch, "GX-2024-01-15");
        assert_eq!(result[0].author, "testuser");
        assert_eq!(result[0].repo_slug, "org/repo");
        assert_eq!(result[0].state, PrState::Open);
        assert_eq!(result[0].url, "https://github.com/org/repo/pull/123");
    }

    #[test]
    fn test_parse_graphql_prs_json_multiple_prs() {
        let json = r#"{"data":{"search":{"nodes":[
            {
                "number": 1,
                "title": "PR 1",
                "headRefName": "GX-branch1",
                "author": {"login": "user1"},
                "state": "OPEN",
                "url": "https://github.com/org/repo1/pull/1",
                "repository": {"nameWithOwner": "org/repo1"}
            },
            {
                "number": 2,
                "title": "PR 2",
                "headRefName": "GX-branch2",
                "author": {"login": "user2"},
                "state": "CLOSED",
                "url": "https://github.com/org/repo2/pull/2",
                "repository": {"nameWithOwner": "org/repo2"}
            }
        ]}}}"#;

        let result = parse_graphql_prs_json(json).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].number, 1);
        assert_eq!(result[0].branch, "GX-branch1");
        assert_eq!(result[0].state, PrState::Open);
        assert_eq!(result[1].number, 2);
        assert_eq!(result[1].branch, "GX-branch2");
        assert_eq!(result[1].state, PrState::Closed);
    }

    #[test]
    fn test_parse_graphql_prs_json_invalid_json() {
        let json = "not valid json";
        let result = parse_graphql_prs_json(json);
        assert!(result.is_err());
    }
}
