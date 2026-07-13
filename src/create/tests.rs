use super::*;

#[test]
fn test_colorize_patch_preserves_every_line_in_order() {
    let patch = "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n-old line\n+new line\n context\n";
    let out = colorize_patch(patch);
    assert_eq!(
        out.lines().count(),
        patch.lines().count(),
        "colorizing must not drop or add lines"
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("--- a/file"));
    assert!(lines[1].contains("+++ b/file"));
    assert!(lines[2].contains("@@ -1,2 +1,2 @@"));
    assert!(lines[3].contains("-old line"));
    assert!(lines[4].contains("+new line"));
    assert!(lines[5].contains(" context"));
}

#[test]
fn test_colorize_patch_handles_empty_input() {
    assert_eq!(colorize_patch(""), "");
}

/// Build a minimal `CreateResult` for a given repo/action/error, filling every
/// other field with an inert default. Test helper only.
fn make_result(slug: &str, action: CreateAction, error: Option<&str>) -> CreateResult {
    CreateResult {
        repo: Repo::from_slug(slug.to_string()),
        change_id: "GX-test".to_string(),
        action,
        files_affected: Vec::new(),
        substitution_stats: None,
        pr_number: None,
        pr_url: None,
        original_branch: None,
        base_sha: None,
        diff: None,
        error: error.map(str::to_string),
    }
}

// Phase 1 (2026-07-12-gx-production-hardening): airtight, scriptable
// reporting. Break-the-guard: reverting `count_errors` to always return 0
// (the pre-Phase-1 `create` behavior, which always ended `Ok(())`) makes
// this test fail.
#[test]
fn test_count_errors_counts_only_failing_results() {
    let results = vec![
        make_result("org/ok", CreateAction::Committed, None),
        make_result("org/broken", CreateAction::DryRun, Some("boom")),
        make_result("org/also-ok", CreateAction::PrCreated, None),
    ];
    assert_eq!(count_errors(&results), 1);
}

#[test]
fn test_count_errors_is_zero_when_nothing_failed() {
    let results = vec![
        make_result("org/ok", CreateAction::Committed, None),
        make_result("org/also-ok", CreateAction::DryRun, None),
    ];
    assert_eq!(count_errors(&results), 0);
}

#[test]
fn test_phase_label_matches_every_create_action() {
    assert_eq!(phase_label(&CreateAction::DryRun), "dry-run");
    assert_eq!(phase_label(&CreateAction::Committed), "committed");
    assert_eq!(phase_label(&CreateAction::PrCreated), "pr-created");
}

// Break-the-guard: a `build_run_report` that (wrongly) includes successful
// repos, or drops the failing one, fails this assertion on both count and
// content.
#[test]
fn test_build_run_report_lists_only_failing_repos_with_phase_and_error() {
    let results = vec![
        make_result("org/ok", CreateAction::Committed, None),
        make_result("org/broken", CreateAction::Committed, Some("push rejected")),
    ];
    let report = build_run_report(&results);
    assert_eq!(report.len(), 1, "only the failing repo should be reported");
    assert_eq!(report[0].repo, "org/broken");
    assert_eq!(report[0].phase, "committed");
    assert_eq!(report[0].error, "push rejected");
}

#[test]
fn test_build_run_report_is_empty_when_every_repo_succeeds() {
    let results = vec![
        make_result("org/a", CreateAction::DryRun, None),
        make_result("org/b", CreateAction::PrCreated, None),
    ];
    assert!(build_run_report(&results).is_empty());
}

#[test]
fn test_write_run_report_produces_parseable_json_naming_the_failure() {
    let dir = tempfile::TempDir::new().unwrap();
    let report_path = dir.path().join("report.json");
    let results = vec![make_result(
        "org/broken",
        CreateAction::DryRun,
        Some("simulated failure"),
    )];
    let report = build_run_report(&results);

    write_run_report(&report_path, &report).expect("write_run_report should succeed");

    let contents = std::fs::read_to_string(&report_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&contents).expect("report file must parse as JSON");
    let entries = parsed.as_array().expect("report is a JSON array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["repo"], "org/broken");
    assert_eq!(entries[0]["phase"], "dry-run");
    assert_eq!(entries[0]["error"], "simulated failure");
}

#[test]
fn test_write_run_report_writes_empty_array_when_nothing_failed() {
    let dir = tempfile::TempDir::new().unwrap();
    let report_path = dir.path().join("report.json");
    write_run_report(&report_path, &Vec::new()).expect("write_run_report should succeed");

    let contents = std::fs::read_to_string(&report_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 0);
}
