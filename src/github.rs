use crate::config::Config;
use crate::subprocess::{run_checked, subprocess_timeout};
use eyre::{Context, Result};
use log::{debug, info, warn};
use serde::Deserialize;
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
    let output = run_checked(
        Command::new("gh").env("GH_TOKEN", token).args([
            "api",
            query,
            "--paginate",
            "--jq",
            &format!(".[]{archived_filter}  | .full_name"),
        ]),
        subprocess_timeout(),
    )
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

    let output = run_checked(
        Command::new("gh").env("GH_TOKEN", token).args([
            "api",
            &format!("repos/{repo_slug}"),
            "--jq",
            ".default_branch",
        ]),
        subprocess_timeout(),
    )
    .context("Failed to get default branch")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(eyre::eyre!("Failed to get default branch: {}", error));
    }

    let branch = String::from_utf8(output.stdout)?.trim().to_string();
    debug!("Default branch for {repo_slug}: {branch}");
    Ok(branch)
}

/// Read the GitHub token for `user_or_org` from its persona env var.
///
/// Resolves the env-var NAME per org (see [`crate::persona::resolve_token_env`])
/// then reads that var's value. Signature is unchanged from the retired
/// file-scheme version, so every call site is untouched. A missing or
/// trimmed-empty var is a LOUD error naming both the var and the org that
/// selected it -- never a silent empty string, never a silent ambient
/// fallback (design doc `2026-07-12-persona-aware-github-auth.md`, Phase 3).
pub fn read_token(user_or_org: &str, config: &Config) -> Result<String> {
    debug!("read_token: user_or_org={user_or_org}");

    let var_name = crate::persona::resolve_token_env(user_or_org, config)?;

    let token = std::env::var(&var_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            eyre::eyre!(
                "GitHub token env var {var_name} is unset or empty (selected for org {user_or_org}); \
                 decrypt it with `manifest age` or set the correct persona"
            )
        })?;

    debug!(
        "read_token: user_or_org={user_or_org} resolved {var_name} (len={})",
        token.len()
    );
    Ok(token)
}

/// The org/owner portion of a repo slug (`org/repo` -> `org`).
fn org_of(repo_slug: &str) -> &str {
    repo_slug.split('/').next().unwrap_or(repo_slug)
}

/// Build a `gh` command with per-org auth: resolve `org`'s persona token via
/// [`read_token`] and set `GH_TOKEN` from it, so every gh call uses the same
/// resolved identity instead of a mix of personas or ambient `gh auth`
/// ([A18]). A missing/empty persona token is a LOUD `Err` -- never a silent
/// fallback to ambient auth, which is exactly the wrong-identity trap this
/// change exists to close (design doc `2026-07-12-persona-aware-github-auth.md`,
/// "Fail-loud vs the current swallow", Phase 4).
fn gh_command(org: &str, config: &Config) -> Result<Command> {
    let token = read_token(org, config)?;
    let mut cmd = Command::new("gh");
    cmd.env("GH_TOKEN", token);
    Ok(cmd)
}

/// Create a pull request using GitHub CLI.
/// Returns the PR number and URL on success.
pub fn create_pr(
    repo_slug: &str,
    branch_name: &str,
    commit_message: &str,
    base_branch: &str,
    pr: &crate::cli::PR,
    config: &Config,
) -> Result<CreatePrResult> {
    debug!("create_pr: repo={repo_slug} branch={branch_name} base={base_branch}");

    let title = branch_name.to_string();
    let body = config
        .pr_body_template()
        .replace("{commit_message}", commit_message);

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
        base_branch,
    ];

    if matches!(pr, crate::cli::PR::Draft) {
        args.push("--draft");
    }

    // Retry network operations, rebuilding the (token-authed) command each try.
    let org = org_of(repo_slug).to_string();
    let output = retry_gh(&org, config, &args, MAX_RETRIES)?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("PR created: {url}");

        // Extract PR number from URL (e.g., https://github.com/org/repo/pull/123).
        // A parse failure is a real error, never a stored PR #0 ([A19]).
        let number = extract_pr_number_from_url(&url)
            .ok_or_else(|| eyre::eyre!("Could not parse PR number from URL: {url}"))?;

        Ok(CreatePrResult { number, url })
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to create PR: {}", error))
    }
}

/// Open a revert PR for a merged change (`gx undo` Phase 6 [F4]). The `revert/
/// <change-id>` branch (already pushed by the caller) is opened against the
/// original base branch, with a body linking the original PR so the reversal is
/// traceable. Never merges anything and never touches the base branch directly.
pub fn create_revert_pr(
    repo_slug: &str,
    branch_name: &str,
    base_branch: &str,
    change_id: &str,
    original_pr: Option<u64>,
    config: &Config,
) -> Result<CreatePrResult> {
    debug!(
        "create_revert_pr: repo={repo_slug} branch={branch_name} base={base_branch} change_id={change_id} original_pr={original_pr:?}"
    );

    let title = format!("Revert {change_id}");
    let body = match original_pr {
        Some(n) => format!(
            "Reverts #{n}\n\nAutomated revert of merged change `{change_id}`, opened by `gx undo`."
        ),
        None => {
            format!("Automated revert of merged change `{change_id}`, opened by `gx undo`.")
        }
    };

    let args = vec![
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
        base_branch,
    ];

    let org = org_of(repo_slug).to_string();
    let output = retry_gh(&org, config, &args, MAX_RETRIES)?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        debug!("Revert PR created: {url}");
        let number = extract_pr_number_from_url(&url)
            .ok_or_else(|| eyre::eyre!("Could not parse PR number from revert PR URL: {url}"))?;
        Ok(CreatePrResult { number, url })
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to create revert PR: {}", error))
    }
}

/// Extract PR number from a GitHub PR URL
fn extract_pr_number_from_url(url: &str) -> Option<u64> {
    // URL format: https://github.com/owner/repo/pull/123
    url.rsplit('/').next()?.parse().ok()
}

/// Execute a `gh` command (token-authed for `org`) with retry + exponential
/// backoff on retryable network errors.
fn retry_gh(
    org: &str,
    config: &Config,
    args: &[&str],
    max_retries: u32,
) -> Result<std::process::Output> {
    let mut last_error = None;

    for attempt in 0..max_retries {
        let output = run_checked(gh_command(org, config)?.args(args), subprocess_timeout())
            .context("Failed to execute gh")?;

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
    /// When the PR was merged, if it was (Phase 4 [F11]: `gx review sync`
    /// needs this to reconcile state against GitHub reality).
    pub merged_at: Option<String>,
    /// The SHA of the merge commit, if merged. Feeds the Phase 6 revert path
    /// (parent-count dispatch between squash/rebase vs. a true merge commit).
    pub merge_commit_oid: Option<String>,
    /// The PR's base branch name.
    pub base_ref_name: String,
    /// GitHub's mergeability verdict for the PR head against its base
    /// (production-hardening doc, Phase 4). Modeled as an enum, not a string
    /// (`rust.md`); `is_mergeable` consults it to fail closed on anything but a
    /// proven-mergeable PR before `review approve` merges it.
    pub mergeable: Mergeability,
}

/// GitHub's `PullRequest.mergeable` verdict (production-hardening doc, Phase 0
/// verified the field + value domain against real PRs). `Unknown` is GitHub's
/// lazily-computed state: a freshly-opened PR returns it until the merge commit
/// is enqueued. An unrecognized or absent value maps to `Unknown` so the
/// mergeable gate fails CLOSED (never merges on uncertainty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeability {
    /// `MERGEABLE`: GitHub proved the PR merges cleanly.
    Mergeable,
    /// `CONFLICTING`: the PR conflicts with its base and cannot merge as-is.
    Conflicting,
    /// `UNKNOWN` / absent / unrecognized: mergeability not (yet) determinable.
    Unknown,
}

impl Mergeability {
    /// Parse GitHub's `mergeable` enum string, failing closed to `Unknown` on
    /// anything but the two known mergeable/conflicting values.
    fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::to_uppercase).as_deref() {
            Some("MERGEABLE") => Mergeability::Mergeable,
            Some("CONFLICTING") => Mergeability::Conflicting,
            _ => Mergeability::Unknown,
        }
    }
}

/// Whether a PR is safe to merge: only a proven `Mergeability::Mergeable` returns
/// true. `Conflicting` and `Unknown` both return false (fail closed) - the
/// production-hardening doc pins that gx never merges on uncertainty
/// (production-hardening doc, Phase 4 API Design).
pub fn is_mergeable(pr: &PrInfo) -> bool {
    matches!(pr.mergeable, Mergeability::Mergeable)
}

/// PR state enumeration. GitHub's GraphQL `PullRequest.state` is one of
/// OPEN/CLOSED/MERGED; `Merged` is distinct from `Closed` so `gx review sync`
/// can tell a landed PR apart from an abandoned one (Phase 4 [F11]).
#[derive(Debug, Clone, PartialEq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
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
    #[serde(rename = "pageInfo")]
    page_info: Option<GhPageInfo>,
}

#[derive(Debug, Deserialize, Clone)]
struct GhPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
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
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
    #[serde(rename = "mergeCommit")]
    merge_commit: Option<GhGraphqlMergeCommit>,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    /// GitHub's `mergeable` enum (`MERGEABLE`/`CONFLICTING`/`UNKNOWN`).
    /// `#[serde(default)]` so a hand-written test shim (or an older cached
    /// response) that omits it deserializes to `None` -> `Mergeability::Unknown`
    /// (fail closed), never a parse error.
    #[serde(default)]
    mergeable: Option<String>,
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

#[derive(Debug, Deserialize)]
struct GhGraphqlMergeCommit {
    oid: String,
}

/// GraphQL query with pagination. The search string is passed as a JSON-encoded
/// variable (`$q`), never spliced into the query text, closing an injection path
/// via crafted org/change-id values ([A13]). `mergedAt`/`mergeCommit { oid }`/
/// `baseRefName` feed `gx review sync` (Phase 4 [F11]) and the Phase 6 revert path.
const PR_SEARCH_QUERY: &str = r#"query($q: String!, $cursor: String) {
  search(query: $q, type: ISSUE, first: 100, after: $cursor) {
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        number
        title
        headRefName
        author { login }
        state
        url
        repository { nameWithOwner }
        mergedAt
        mergeCommit { oid }
        baseRefName
        mergeable
      }
    }
  }
}"#;

/// The GitHub search query text for PRs whose head branch matches `pattern` in
/// `org`. No `is:open` filter (Phase 4 [F11]): `gx review sync` needs to see
/// merged/closed PRs too, not just open ones; existing open-only consumers
/// (`review approve`/`delete`) already filter locally on `PrState::Open`.
fn pr_search_string(org: &str, pattern: &str) -> String {
    format!("org:{org} is:pr head:{pattern}")
}

/// List PRs by change ID pattern using GraphQL, following pagination to
/// exhaustion (no longer capped at the first 100 results) ([A13]).
pub fn list_prs_by_change_id(
    org: &str,
    change_id_pattern: &str,
    config: &Config,
) -> Result<Vec<PrInfo>> {
    debug!("list_prs_by_change_id: org={org} pattern={change_id_pattern}");

    let search = pr_search_string(org, change_id_pattern);
    let mut cursor: Option<String> = None;
    let mut all = Vec::new();

    loop {
        let mut args = vec![
            "api".to_string(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={PR_SEARCH_QUERY}"),
            "-f".to_string(),
            format!("q={search}"),
        ];
        if let Some(ref c) = cursor {
            args.push("-f".to_string());
            args.push(format!("cursor={c}"));
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let output = run_checked(
            gh_command(org, config)?.args(&arg_refs),
            subprocess_timeout(),
        )
        .context("Failed to execute gh api graphql")?;

        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(eyre::eyre!("Failed to search PRs: {}", error));
        }

        let json_output =
            String::from_utf8(output.stdout).context("Invalid UTF-8 in gh api graphql output")?;

        let (mut page, page_info) = parse_graphql_prs_page(&json_output, change_id_pattern)?;
        all.append(&mut page);

        match page_info {
            Some(info) if info.has_next_page => {
                cursor = info.end_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            _ => break,
        }
    }

    debug!("list_prs_by_change_id: {} PRs total", all.len());
    Ok(all)
}

/// Parse JSON output from gh api graphql (test helper that uses default GX- pattern)
#[cfg(test)]
fn parse_graphql_prs_json(json_output: &str) -> Result<Vec<PrInfo>> {
    Ok(parse_graphql_prs_page(json_output, "GX-")?.0)
}

/// Parse one GraphQL page: returns the filtered PRs and the page info (for
/// pagination). The same `GX-`-prefix filtering as before is applied.
fn parse_graphql_prs_page(
    json_output: &str,
    pattern: &str,
) -> Result<(Vec<PrInfo>, Option<GhPageInfo>)> {
    let trimmed = json_output.trim();
    if trimmed.is_empty() {
        return Ok((Vec::new(), None));
    }

    let response: GhGraphqlResponse =
        serde_json::from_str(trimmed).context("Failed to parse GraphQL response JSON")?;

    let page_info = response.data.search.page_info.clone();

    let prs: Vec<PrInfo> = response
        .data
        .search
        .nodes
        .into_iter()
        .filter(|gh_pr| {
            // GX naming convention: BOTH branch AND title must start with "GX-" to avoid false positives
            // (e.g., branch "gx-alerts" or title mentioning "GX" in the middle)
            // Then filter branch name against the specific pattern for exact matching
            let has_gx_prefix =
                gh_pr.head_ref_name.starts_with("GX-") && gh_pr.title.starts_with("GX-");
            let matches_pattern = gh_pr.head_ref_name.starts_with(pattern);
            has_gx_prefix && matches_pattern
        })
        .map(|gh_pr| PrInfo {
            repo_slug: gh_pr.repository.name_with_owner,
            number: gh_pr.number,
            title: gh_pr.title,
            branch: gh_pr.head_ref_name,
            author: gh_pr.author.login,
            state: match gh_pr.state.to_uppercase().as_str() {
                "OPEN" => PrState::Open,
                "MERGED" => PrState::Merged,
                _ => PrState::Closed,
            },
            url: gh_pr.url,
            merged_at: gh_pr.merged_at,
            merge_commit_oid: gh_pr.merge_commit.map(|m| m.oid),
            base_ref_name: gh_pr.base_ref_name,
            mergeable: Mergeability::parse(gh_pr.mergeable.as_deref()),
        })
        .collect();

    debug!(
        "Parsed {} PRs from GraphQL response (after filtering for pattern '{}')",
        prs.len(),
        pattern
    );
    Ok((prs, page_info))
}

/// Approve and merge a PR
pub fn approve_and_merge_pr(
    repo_slug: &str,
    pr_number: u64,
    admin_override: bool,
    auto_merge: bool,
    config: &Config,
) -> Result<()> {
    debug!("Approving and merging PR #{pr_number} in {repo_slug}");
    let org = org_of(repo_slug);

    // First approve the PR
    let approve_output = run_checked(
        gh_command(org, config)?.args([
            "pr",
            "review",
            &pr_number.to_string(),
            "--repo",
            repo_slug,
            "--approve",
        ]),
        subprocess_timeout(),
    )
    .context("Failed to execute gh pr review --approve")?;

    // A failed `--approve` ABORTS this PR's merge (production-hardening doc,
    // Phase 4): previously the failure was only warned and the merge proceeded,
    // landing a PR that never got its approval. Fail closed instead.
    if !approve_output.status.success() {
        let error = String::from_utf8_lossy(&approve_output.stderr);
        warn!("Failed to approve PR #{pr_number}: {error}");
        return Err(eyre::eyre!(
            "Aborting merge of PR #{pr_number}: approval step failed: {error}"
        ));
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

    if auto_merge {
        merge_args.push("--auto");
    }

    let merge_output = run_checked(
        gh_command(org, config)?.args(&merge_args),
        subprocess_timeout(),
    )
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
pub fn close_pr(repo_slug: &str, pr_number: u64, config: &Config) -> Result<()> {
    debug!("Closing PR #{pr_number} in {repo_slug}");

    let output = run_checked(
        gh_command(org_of(repo_slug), config)?.args([
            "pr",
            "close",
            &pr_number.to_string(),
            "--repo",
            repo_slug,
        ]),
        subprocess_timeout(),
    )
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
pub fn delete_remote_branch(repo_slug: &str, branch_name: &str, config: &Config) -> Result<()> {
    debug!("Deleting remote branch '{branch_name}' in {repo_slug}");

    let output = run_checked(
        gh_command(org_of(repo_slug), config)?.args([
            "api",
            &format!("repos/{repo_slug}/git/refs/heads/{branch_name}"),
            "--method",
            "DELETE",
        ]),
        subprocess_timeout(),
    )
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

/// List all branches with a specific prefix (for purge operations). Paginates
/// past 100 via `--paginate` so large repos are fully covered ([A12]).
pub fn list_branches_with_prefix(
    repo_slug: &str,
    prefix: &str,
    config: &Config,
) -> Result<Vec<String>> {
    debug!("Listing branches with prefix '{prefix}' in {repo_slug}");

    let output = run_checked(
        gh_command(org_of(repo_slug), config)?.args([
            "api",
            "--paginate",
            &format!("repos/{repo_slug}/branches"),
            "--jq",
            &format!(".[] | select(.name | startswith(\"{prefix}\")) | .name"),
        ]),
        subprocess_timeout(),
    )
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

/// Head-ref branch names of all open PRs in a repo (paginated). Used by purge to
/// refuse deleting a branch that still has an open PR ([A12], design Q3).
pub fn list_open_pr_branches(repo_slug: &str, config: &Config) -> Result<Vec<String>> {
    debug!("Listing open-PR head branches in {repo_slug}");

    let output = run_checked(
        gh_command(org_of(repo_slug), config)?.args([
            "api",
            "--paginate",
            &format!("repos/{repo_slug}/pulls?state=open&per_page=100"),
            "--jq",
            ".[].head.ref",
        ]),
        subprocess_timeout(),
    )
    .context("Failed to execute gh api pulls")?;

    if output.status.success() {
        let branches = String::from_utf8(output.stdout)
            .context("Invalid UTF-8 in pulls output")?
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        Ok(branches)
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("Failed to list open PRs: {}", error))
    }
}

#[cfg(test)]
mod tests;
