use super::*;
use crate::state::{ChangeState, ChangeStatus};

#[test]
fn test_stuck_proposals_names_a_bare_proposal_campaign() {
    // Ringer addendum #3: `gx doctor` must name a campaign sitting at the
    // bare-proposal aggregate status (never applied or undone).
    let mut proposed = ChangeState::new("GX-stuck".to_string(), None);
    proposed.mark_proposed(
        "org/repo",
        "deadbeef".to_string(),
        vec!["README.md".to_string()],
        Some("/tmp/org/repo".to_string()),
    );
    assert_eq!(
        proposed.status,
        ChangeStatus::Proposed,
        "sanity: mark_proposed must set the aggregate status (the other half of this fix)"
    );

    let mut merged = ChangeState::new("GX-done".to_string(), None);
    merged.status = ChangeStatus::FullyMerged;

    let stuck = stuck_proposals(vec![merged, proposed]);
    // The bite: a naive "just list everything" would report GX-done too.
    assert_eq!(
        stuck
            .iter()
            .map(|s| s.change_id.as_str())
            .collect::<Vec<_>>(),
        vec!["GX-stuck"],
        "only the bare-proposal campaign is reported, sorted by change-id"
    );
}

#[test]
fn test_stuck_proposals_is_empty_with_no_bare_proposals() {
    let mut merged = ChangeState::new("GX-done".to_string(), None);
    merged.status = ChangeStatus::FullyMerged;
    assert!(stuck_proposals(vec![merged]).is_empty());
}

/// The structured read core `collect_report` (Phase 9, behind the MCP `doctor`
/// tool) must surface a bare proposal as stuck and always report the git/gh
/// tool checks. Isolated under a throwaway `XDG_DATA_HOME` so it reads its own
/// state, never the operator's.
#[test]
fn test_collect_report_surfaces_stuck_proposal_and_tools() {
    let guard = local::test_utils::env_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    let dir = tempfile::TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", dir.path()) };

    let mut st = ChangeState::new("GX-stuck-report".to_string(), None);
    st.mark_proposed(
        "org/repo",
        "deadbeef".to_string(),
        vec!["README.md".to_string()],
        Some("/tmp/org/repo".to_string()),
    );
    StateManager::new().unwrap().save(&st).unwrap();

    let report = collect_report().unwrap();
    assert!(
        report
            .stuck_proposals
            .iter()
            .any(|s| s.change_id == "GX-stuck-report"),
        "collect_report must surface the bare proposal as stuck"
    );
    assert!(
        report.tools.iter().any(|t| t.name == "git"),
        "collect_report always reports the git tool check"
    );

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

#[test]
fn test_version_compare_pads_shorter() {
    // The [A25] fix: "2.20" and "2.20.0" must compare equal (>= true).
    assert!(version_compare("2.20", "2.20.0"));
    assert!(version_compare("2.20.0", "2.20"));
}

#[test]
fn test_version_compare_ordering() {
    assert!(version_compare("2.34.1", "2.20.0"));
    assert!(!version_compare("1.9.0", "2.0.0"));
    assert!(version_compare("2.0.0", "2.0.0"));
    assert!(version_compare("2.0.1", "2.0.0"));
    assert!(!version_compare("2.0.0", "2.0.1"));
}

#[test]
fn test_extract_second_token_version() {
    assert_eq!(extract_second_token_version("ripgrep 14.1.1"), "14.1.1");
    assert_eq!(
        extract_second_token_version("ripgrep 13.0.0 (rev abc123)"),
        "13.0.0"
    );
    assert_eq!(extract_second_token_version(""), "unknown");
    assert_eq!(extract_second_token_version("ripgrep"), "unknown");
}

/// `gx doctor` must flag a MISSING `rg` (fail closed): a tool that cannot be
/// spawned reports `not found`, `ok = false`. Uses a bogus binary name so it
/// bites deterministically without mutating `$PATH` (which would race concurrent
/// git-spawning tests).
#[test]
fn test_check_tool_presence_reports_missing() {
    let check = check_tool_presence("gx-rg-definitely-not-installed-xyz", RG_MIN_VERSION);
    assert_eq!(check.name, "gx-rg-definitely-not-installed-xyz");
    assert!(!check.ok, "a missing tool must be reported not-ok");
    assert_eq!(check.version, "not found");
}

/// The doctor report always includes an `rg` tool check (present or not).
#[test]
fn test_collect_report_includes_rg_check() {
    let guard = local::test_utils::env_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();
    let dir = tempfile::TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", dir.path()) };

    let report = collect_report().unwrap();
    assert!(
        report.tools.iter().any(|t| t.name == "rg"),
        "collect_report must report the rg tool check"
    );

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

#[test]
fn test_extract_version() {
    assert_eq!(extract_version("git version 2.34.1"), "2.34.1");
    assert_eq!(extract_version("gh version 2.40.1 (2023-12-13)"), "2.40.1");
    assert_eq!(extract_version("no version here at all xyz"), "here");
    assert_eq!(extract_version(""), "unknown");
}
