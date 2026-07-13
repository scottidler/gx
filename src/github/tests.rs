use super::*;
use crate::test_utils::env_lock;

/// `read_token` resolves the persona env var NAME then reads that var's
/// VALUE, and fails loudly (naming both var and org) when it is unset.
/// Exercises the org-`scottidler` -> `$GITHUB_PAT_HOME` built-in floor, so
/// `$GH_PERSONA` must be unset for the classification to apply; both env
/// vars are saved and restored under the process-wide env lock.
#[test]
fn test_read_token_home_persona_set_and_unset() {
    let _guard = env_lock();
    let prior_persona = std::env::var("GH_PERSONA").ok();
    let prior_home = std::env::var("GITHUB_PAT_HOME").ok();

    // No GH_PERSONA override: scottidler classifies to the HOME floor.
    unsafe { std::env::remove_var("GH_PERSONA") };

    let config = Config::default();

    // Set -> read_token returns the trimmed value verbatim.
    unsafe { std::env::set_var("GITHUB_PAT_HOME", "  home-token-value  ") };
    let token = read_token("scottidler", &config).unwrap();
    assert_eq!(token, "home-token-value");

    // Unset -> loud Err naming BOTH the var and the org that selected it.
    unsafe { std::env::remove_var("GITHUB_PAT_HOME") };
    let err = read_token("scottidler", &config).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("GITHUB_PAT_HOME"),
        "error must name the missing var: {msg}"
    );
    assert!(
        msg.contains("scottidler"),
        "error must name the org that selected it: {msg}"
    );

    match prior_home {
        Some(v) => unsafe { std::env::set_var("GITHUB_PAT_HOME", v) },
        None => unsafe { std::env::remove_var("GITHUB_PAT_HOME") },
    }
    match prior_persona {
        Some(v) => unsafe { std::env::set_var("GH_PERSONA", v) },
        None => unsafe { std::env::remove_var("GH_PERSONA") },
    }
}

/// The one behavior change of Phase 4: a mutating `gh_command` call whose
/// selected persona var is unset must fail loudly (`Err`), never silently
/// build an ambient-auth `Command` (design doc "Fail-loud vs the current
/// swallow"). Exercises the same `scottidler` -> `$GITHUB_PAT_HOME` floor as
/// above, but asserts on `gh_command` itself rather than `read_token`, so an
/// accidental re-introduction of the old `match ... Err(e) => debug!(...)`
/// swallow in `gh_command` fails this test even if `read_token` is untouched.
#[test]
fn test_gh_command_fails_loud_when_persona_token_unset() {
    let _guard = env_lock();
    let prior_persona = std::env::var("GH_PERSONA").ok();
    let prior_home = std::env::var("GITHUB_PAT_HOME").ok();

    unsafe { std::env::remove_var("GH_PERSONA") };
    unsafe { std::env::remove_var("GITHUB_PAT_HOME") };

    let config = Config::default();

    let err = gh_command("scottidler", &config)
        .expect_err("gh_command must fail loudly, never fall back to ambient gh auth");
    let msg = err.to_string();
    assert!(
        msg.contains("GITHUB_PAT_HOME"),
        "error must name the missing var: {msg}"
    );
    assert!(
        msg.contains("scottidler"),
        "error must name the org that selected it: {msg}"
    );

    match prior_home {
        Some(v) => unsafe { std::env::set_var("GITHUB_PAT_HOME", v) },
        None => unsafe { std::env::remove_var("GITHUB_PAT_HOME") },
    }
    match prior_persona {
        Some(v) => unsafe { std::env::set_var("GH_PERSONA", v) },
        None => unsafe { std::env::remove_var("GH_PERSONA") },
    }
}

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
        "repository": {"nameWithOwner": "org/repo"},
        "baseRefName": "main"
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
    assert_eq!(result[0].base_ref_name, "main");
    assert_eq!(result[0].merged_at, None);
    assert_eq!(result[0].merge_commit_oid, None);
}

#[test]
fn test_parse_graphql_prs_json_multiple_prs() {
    let json = r#"{"data":{"search":{"nodes":[
        {
            "number": 1,
            "title": "GX-branch1: PR 1",
            "headRefName": "GX-branch1",
            "author": {"login": "user1"},
            "state": "OPEN",
            "url": "https://github.com/org/repo1/pull/1",
            "repository": {"nameWithOwner": "org/repo1"},
            "baseRefName": "main"
        },
        {
            "number": 2,
            "title": "GX-branch2: PR 2",
            "headRefName": "GX-branch2",
            "author": {"login": "user2"},
            "state": "CLOSED",
            "url": "https://github.com/org/repo2/pull/2",
            "repository": {"nameWithOwner": "org/repo2"},
            "baseRefName": "main"
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
fn test_parse_graphql_prs_json_merged_pr_fields() {
    // Phase 4 [F11]: `gx review sync` needs `state: MERGED` distinguished
    // from `Closed`, plus `mergedAt`/`mergeCommit.oid`/`baseRefName`.
    let json = r#"{"data":{"search":{"nodes":[{
        "number": 99,
        "title": "GX-merged: PR",
        "headRefName": "GX-merged",
        "author": {"login": "user1"},
        "state": "MERGED",
        "url": "https://github.com/org/repo/pull/99",
        "repository": {"nameWithOwner": "org/repo"},
        "mergedAt": "2026-07-11T00:00:00Z",
        "mergeCommit": {"oid": "deadbeefcafe"},
        "baseRefName": "main"
    }]}}}"#;

    let result = parse_graphql_prs_json(json).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].state, PrState::Merged);
    assert_eq!(result[0].merged_at.as_deref(), Some("2026-07-11T00:00:00Z"));
    assert_eq!(result[0].merge_commit_oid.as_deref(), Some("deadbeefcafe"));
    assert_eq!(result[0].base_ref_name, "main");
}

#[test]
fn test_parse_graphql_prs_json_parses_mergeable_field() {
    // Phase 4 (production hardening): the `mergeable` enum parses from the
    // GraphQL field. MERGEABLE -> Mergeable, CONFLICTING -> Conflicting.
    let json = r#"{"data":{"search":{"nodes":[
        {
            "number": 1,
            "title": "GX-ok: PR",
            "headRefName": "GX-ok",
            "author": {"login": "u"},
            "state": "OPEN",
            "url": "https://github.com/org/repo/pull/1",
            "repository": {"nameWithOwner": "org/repo"},
            "baseRefName": "main",
            "mergeable": "MERGEABLE"
        },
        {
            "number": 2,
            "title": "GX-conflict: PR",
            "headRefName": "GX-conflict",
            "author": {"login": "u"},
            "state": "OPEN",
            "url": "https://github.com/org/repo/pull/2",
            "repository": {"nameWithOwner": "org/repo"},
            "baseRefName": "main",
            "mergeable": "CONFLICTING"
        }
    ]}}}"#;

    let result = parse_graphql_prs_json(json).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].mergeable, Mergeability::Mergeable);
    assert!(is_mergeable(&result[0]), "MERGEABLE -> is_mergeable true");
    assert_eq!(result[1].mergeable, Mergeability::Conflicting);
    assert!(
        !is_mergeable(&result[1]),
        "CONFLICTING -> is_mergeable false (fail closed)"
    );
}

#[test]
fn test_parse_graphql_mergeable_fails_closed_to_unknown() {
    // A PR with `mergeable: UNKNOWN` (GitHub's lazily-computed state) AND a PR
    // that omits the field entirely BOTH map to `Mergeability::Unknown`, and
    // `is_mergeable` returns false for them -- never merge on uncertainty.
    let json = r#"{"data":{"search":{"nodes":[
        {
            "number": 1,
            "title": "GX-unknown: PR",
            "headRefName": "GX-unknown",
            "author": {"login": "u"},
            "state": "OPEN",
            "url": "https://github.com/org/repo/pull/1",
            "repository": {"nameWithOwner": "org/repo"},
            "baseRefName": "main",
            "mergeable": "UNKNOWN"
        },
        {
            "number": 2,
            "title": "GX-absent: PR",
            "headRefName": "GX-absent",
            "author": {"login": "u"},
            "state": "OPEN",
            "url": "https://github.com/org/repo/pull/2",
            "repository": {"nameWithOwner": "org/repo"},
            "baseRefName": "main"
        }
    ]}}}"#;

    let result = parse_graphql_prs_json(json).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].mergeable, Mergeability::Unknown);
    assert_eq!(
        result[1].mergeable,
        Mergeability::Unknown,
        "an absent mergeable field must fail closed to Unknown"
    );
    assert!(!is_mergeable(&result[0]));
    assert!(!is_mergeable(&result[1]));
}

#[test]
fn test_pr_search_string_has_no_open_filter() {
    // Phase 4 [F11] bite-proof: the old query filtered `is:open`, so
    // `gx review sync` could never see merged/closed PRs. Broadened here;
    // open-only consumers (approve/delete) filter locally on PrState::Open.
    let q = pr_search_string("acme", "GX-123");
    assert!(
        !q.contains("is:open"),
        "search must not filter to open-only"
    );
    assert!(q.contains("org:acme"));
    assert!(q.contains("head:GX-123"));
}

#[test]
fn test_parse_graphql_prs_json_filters_non_gx_prs() {
    // This test verifies that PRs without proper GX- prefix on BOTH branch AND title are filtered out
    // This prevents false positives like "gx-alerts" branch or "[MCORE-1276] Enable Slack alerts for GX checks" title
    let json = r#"{"data":{"search":{"nodes":[
        {
            "number": 1,
            "title": "GX-2024-01-15: Proper GX PR",
            "headRefName": "GX-2024-01-15",
            "author": {"login": "user1"},
            "state": "OPEN",
            "url": "https://github.com/org/repo1/pull/1",
            "repository": {"nameWithOwner": "org/repo1"},
            "baseRefName": "main"
        },
        {
            "number": 2,
            "title": "[MCORE-1276] Enable Slack alerts for GX checks",
            "headRefName": "gx-alerts",
            "author": {"login": "user2"},
            "state": "OPEN",
            "url": "https://github.com/org/repo2/pull/2",
            "repository": {"nameWithOwner": "org/repo2"},
            "baseRefName": "main"
        },
        {
            "number": 3,
            "title": "Some other PR with GX in title",
            "headRefName": "feature-branch",
            "author": {"login": "user3"},
            "state": "OPEN",
            "url": "https://github.com/org/repo3/pull/3",
            "repository": {"nameWithOwner": "org/repo3"},
            "baseRefName": "main"
        },
        {
            "number": 4,
            "title": "Non-GX title",
            "headRefName": "GX-2024-01-16",
            "author": {"login": "user4"},
            "state": "OPEN",
            "url": "https://github.com/org/repo4/pull/4",
            "repository": {"nameWithOwner": "org/repo4"},
            "baseRefName": "main"
        }
    ]}}}"#;

    let result = parse_graphql_prs_json(json).unwrap();
    // Only the first PR should match (both branch AND title start with GX-)
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].number, 1);
    assert_eq!(result[0].branch, "GX-2024-01-15");
    assert_eq!(result[0].title, "GX-2024-01-15: Proper GX PR");
}

#[test]
fn test_parse_graphql_prs_json_invalid_json() {
    let json = "not valid json";
    let result = parse_graphql_prs_json(json);
    assert!(result.is_err());
}

#[test]
fn test_parse_graphql_page_returns_page_info() {
    // A page with hasNextPage=true must surface the cursor for pagination ([A13]).
    let json = r#"{"data":{"search":{
        "pageInfo": {"hasNextPage": true, "endCursor": "CURSOR123"},
        "nodes":[{
            "number": 1,
            "title": "GX-x: PR",
            "headRefName": "GX-x",
            "author": {"login": "u"},
            "state": "OPEN",
            "url": "https://github.com/o/r/pull/1",
            "repository": {"nameWithOwner": "o/r"},
            "baseRefName": "main"
        }]
    }}}"#;
    let (prs, page_info) = parse_graphql_prs_page(json, "GX-").unwrap();
    assert_eq!(prs.len(), 1);
    let info = page_info.expect("page info present");
    assert!(info.has_next_page);
    assert_eq!(info.end_cursor.as_deref(), Some("CURSOR123"));
}

#[test]
fn test_search_query_uses_variables() {
    // The query is parameterized ($q, $cursor), never string-interpolated ([A13]).
    assert!(PR_SEARCH_QUERY.contains("$q: String!"));
    assert!(PR_SEARCH_QUERY.contains("$cursor: String"));
    assert!(PR_SEARCH_QUERY.contains("hasNextPage"));
}

#[test]
fn test_pr_body_template_substitution() {
    let config = crate::config::Config::default();
    let body = config
        .pr_body_template()
        .replace("{commit_message}", "my commit");
    assert_eq!(body, "my commit");
    assert!(!body.contains("scottidler/gx"));
}

/// Installs a fake `gh` shim on `dir` executable as `gh`. Every invocation
/// appends its args to `$GX_TEST_APPROVE_LOG` before exiting; `review`
/// sub-invocations exit with `review_status`, `merge` sub-invocations exit 0.
fn install_approve_shim(dir: &std::path::Path, review_status: i32) {
    let gh_path = dir.join("gh");
    let script = format!(
        r#"#!/bin/sh
echo "$@" >> "$GX_TEST_APPROVE_LOG"
if [ "$1" = "pr" ] && [ "$2" = "review" ]; then
  exit {review_status}
fi
exit 0
"#
    );
    std::fs::write(&gh_path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&gh_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&gh_path, perms).unwrap();
}

/// Break-the-guard (Part A, non-admin path): a failed `gh pr review --approve`
/// must ABORT the merge -- `gh pr merge` must NEVER be invoked. Remove the
/// abort-on-failed-approve guard (make the merge run regardless) and the
/// logged calls contain a `merge` invocation, failing this test.
#[test]
fn test_approve_and_merge_pr_non_admin_failed_approve_makes_zero_merge_calls() {
    let _guard = env_lock();
    let prior_path = std::env::var("PATH").ok();
    let prior_home = std::env::var("GITHUB_PAT_HOME").ok();
    let prior_log = std::env::var("GX_TEST_APPROVE_LOG").ok();

    let shim_dir = tempfile::TempDir::new().unwrap();
    // review_status=1 -> the approve step fails.
    install_approve_shim(shim_dir.path(), 1);
    let new_path = format!(
        "{}:{}",
        shim_dir.path().display(),
        prior_path.clone().unwrap_or_default()
    );
    unsafe { std::env::set_var("PATH", &new_path) };
    unsafe { std::env::set_var("GITHUB_PAT_HOME", "dummy-token-not-a-secret") };
    let log_path = shim_dir.path().join("approve.log");
    unsafe { std::env::set_var("GX_TEST_APPROVE_LOG", &log_path) };

    let config = crate::config::Config::default();
    let result = approve_and_merge_pr("scottidler/gx", 42, false, false, &config);

    assert!(
        result.is_err(),
        "a failed --approve must abort the merge on the non-admin path"
    );
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log.contains("review"),
        "the approve step must have been attempted: {log}"
    );
    assert!(
        !log.contains("merge"),
        "ZERO merge calls may run after a failed approve, but the log shows one: {log}"
    );

    match prior_path {
        Some(v) => unsafe { std::env::set_var("PATH", v) },
        None => unsafe { std::env::remove_var("PATH") },
    }
    match prior_home {
        Some(v) => unsafe { std::env::set_var("GITHUB_PAT_HOME", v) },
        None => unsafe { std::env::remove_var("GITHUB_PAT_HOME") },
    }
    match prior_log {
        Some(v) => unsafe { std::env::set_var("GX_TEST_APPROVE_LOG", v) },
        None => unsafe { std::env::remove_var("GX_TEST_APPROVE_LOG") },
    }
    drop(_guard);
}

/// Break-the-guard (Part A, `--admin` path): `admin_override=true` must SKIP
/// `gh pr review --approve` entirely (self-approval is categorically rejected
/// by GitHub) and merge via `gh pr merge --admin` regardless. Remove the
/// admin exemption (run `--approve` unconditionally) and this test fails
/// because the log gains a `review` call; revert the `--admin` flag wiring
/// and it fails because the merge call lacks `--admin`.
#[test]
fn test_approve_and_merge_pr_admin_override_skips_approve_and_merges_with_admin() {
    let _guard = env_lock();
    let prior_path = std::env::var("PATH").ok();
    let prior_home = std::env::var("GITHUB_PAT_HOME").ok();
    let prior_log = std::env::var("GX_TEST_APPROVE_LOG").ok();

    let shim_dir = tempfile::TempDir::new().unwrap();
    // review_status is irrelevant on the admin path since review must never run;
    // pin it to failure so an accidental approve call would abort loudly too.
    install_approve_shim(shim_dir.path(), 1);
    let new_path = format!(
        "{}:{}",
        shim_dir.path().display(),
        prior_path.clone().unwrap_or_default()
    );
    unsafe { std::env::set_var("PATH", &new_path) };
    unsafe { std::env::set_var("GITHUB_PAT_HOME", "dummy-token-not-a-secret") };
    let log_path = shim_dir.path().join("approve.log");
    unsafe { std::env::set_var("GX_TEST_APPROVE_LOG", &log_path) };

    let config = crate::config::Config::default();
    let result = approve_and_merge_pr("scottidler/gx", 42, true, false, &config);

    assert!(
        result.is_ok(),
        "admin_override must merge without ever attempting self-approve: {result:?}"
    );
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        !log.contains("review"),
        "NO approve call may run on the --admin path, but the log shows one: {log}"
    );
    let merge_line = log
        .lines()
        .find(|l| l.starts_with("pr merge"))
        .unwrap_or_else(|| panic!("expected a merge call in the log: {log}"));
    assert!(
        merge_line.contains("--admin"),
        "the merge call must carry --admin: {merge_line}"
    );

    match prior_path {
        Some(v) => unsafe { std::env::set_var("PATH", v) },
        None => unsafe { std::env::remove_var("PATH") },
    }
    match prior_home {
        Some(v) => unsafe { std::env::set_var("GITHUB_PAT_HOME", v) },
        None => unsafe { std::env::remove_var("GITHUB_PAT_HOME") },
    }
    match prior_log {
        Some(v) => unsafe { std::env::set_var("GX_TEST_APPROVE_LOG", v) },
        None => unsafe { std::env::remove_var("GX_TEST_APPROVE_LOG") },
    }
    drop(_guard);
}
