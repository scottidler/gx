use gx::config::OutputVerbosity;
use gx::git::{CheckoutAction, CheckoutResult, RepoStatus};
use gx::output::{AlignmentWidths, StatusOptions, UnifiedDisplay};
use gx::repo::Repo;
use std::path::PathBuf;

#[test]
fn test_unified_display_trait_for_repo_status() {
    let repo = Repo {
        path: PathBuf::from("/tmp/test-repo"),
        name: "test-repo".to_string(),
        slug: Some("user/test-repo".to_string()),
    };

    let status = RepoStatus {
        repo,
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate,
        error: None,
    };

    // Test trait methods
    assert_eq!(status.get_branch(), Some("main"));
    assert_eq!(status.get_commit_sha(), Some("abc1234"));
    assert_eq!(status.get_repo().name, "test-repo");
    assert_eq!(status.get_error(), None);

    let opts = StatusOptions::default();
    assert_eq!(status.get_emoji(&opts), "🟢".to_string()); // Clean repo with up-to-date remote
}

#[test]
fn test_unified_display_trait_for_checkout_result() {
    let repo = Repo {
        path: PathBuf::from("/tmp/test-repo"),
        name: "test-repo".to_string(),
        slug: Some("user/test-repo".to_string()),
    };

    let result = CheckoutResult {
        repo,
        branch_name: "feature".to_string(),
        commit_sha: Some("def5678".to_string()),
        action: CheckoutAction::CheckedOutSynced,
        error: None,
    };

    // Test trait methods
    assert_eq!(result.get_branch(), Some("feature"));
    assert_eq!(result.get_commit_sha(), Some("def5678"));
    assert_eq!(result.get_repo().name, "test-repo");
    assert_eq!(result.get_error(), None);

    let opts = StatusOptions::default();
    assert_eq!(result.get_emoji(&opts), "📥".to_string()); // Checked out and synced
}

#[test]
fn test_alignment_widths_calculation() {
    let repo1 = Repo {
        path: PathBuf::from("/tmp/short"),
        name: "short".to_string(),
        slug: Some("user/short".to_string()),
    };

    let repo2 = Repo {
        path: PathBuf::from("/tmp/very-long-repository-name"),
        name: "very-long-repository-name".to_string(),
        slug: Some("user/very-long-repository-name".to_string()),
    };

    let status1 = RepoStatus {
        repo: repo1,
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate,
        error: None,
    };

    let status2 = RepoStatus {
        repo: repo2,
        branch: Some("feature-branch-with-long-name".to_string()),
        commit_sha: Some("def5678".to_string()),
        is_clean: false,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate,
        error: None,
    };

    let items = vec![&status1, &status2];
    let widths = AlignmentWidths::calculate(&items);

    // Branch width should be the length of the longest branch name
    assert_eq!(widths.branch_width, "feature-branch-with-long-name".len());
    // SHA width should always be 7
    assert_eq!(widths.sha_width, 7);
    // Emoji width should be at least 2 (calculated dynamically based on actual emoji widths)
    assert!(widths.emoji_width >= 2);
}

#[test]
fn test_unified_format_consistency() {
    // Create test data
    let repo = Repo {
        path: PathBuf::from("/tmp/test-repo"),
        name: "test-repo".to_string(),
        slug: Some("user/test-repo".to_string()),
    };

    let status = RepoStatus {
        repo: repo.clone(),
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate,
        error: None,
    };

    let checkout = CheckoutResult {
        repo: repo.clone(),
        branch_name: "main".to_string(),
        commit_sha: Some("abc1234".to_string()),
        action: CheckoutAction::CheckedOutSynced,
        error: None,
    };

    let _opts = StatusOptions::default();

    // Both should have the same branch and commit SHA
    assert_eq!(status.get_branch(), checkout.get_branch());
    assert_eq!(status.get_commit_sha(), checkout.get_commit_sha());
    assert_eq!(status.get_repo().name, checkout.get_repo().name);

    // Test that alignment widths work with individual types
    let status_items = vec![&status];
    let checkout_items = vec![&checkout];

    let status_widths = AlignmentWidths::calculate(&status_items);
    let checkout_widths = AlignmentWidths::calculate(&checkout_items);

    // Both should have consistent structure
    assert_eq!(status_widths.sha_width, checkout_widths.sha_width);
    assert_eq!(status_widths.emoji_width, checkout_widths.emoji_width);
    assert!(status_widths.branch_width >= 4); // "main".len()
    assert!(checkout_widths.branch_width >= 4); // "main".len()
}

#[test]
fn test_error_handling_in_unified_display() {
    let repo = Repo {
        path: PathBuf::from("/tmp/error-repo"),
        name: "error-repo".to_string(),
        slug: Some("user/error-repo".to_string()),
    };

    let error_status = RepoStatus {
        repo: repo.clone(),
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: false,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate,
        error: Some("Git command failed".to_string()),
    };

    let error_checkout = CheckoutResult {
        repo: repo.clone(),
        branch_name: "main".to_string(),
        commit_sha: None, // No SHA when there's an error
        action: CheckoutAction::CheckedOutSynced,
        error: Some("Checkout failed".to_string()),
    };

    let opts = StatusOptions::default();

    // Both should show error emoji
    assert_eq!(error_status.get_emoji(&opts), "❌".to_string());
    assert_eq!(error_checkout.get_emoji(&opts), "❌".to_string());

    // Both should have error messages
    assert_eq!(error_status.get_error(), Some("Git command failed"));
    assert_eq!(error_checkout.get_error(), Some("Checkout failed"));
}

#[test]
fn test_no_emoji_mode() {
    let repo = Repo {
        path: PathBuf::from("/tmp/test-repo"),
        name: "test-repo".to_string(),
        slug: Some("user/test-repo".to_string()),
    };

    let status = RepoStatus {
        repo: repo.clone(),
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate,
        error: None,
    };

    let checkout = CheckoutResult {
        repo: repo.clone(),
        branch_name: "main".to_string(),
        commit_sha: Some("abc1234".to_string()),
        action: CheckoutAction::CheckedOutSynced,
        error: None,
    };

    let opts = StatusOptions {
        verbosity: OutputVerbosity::Summary,
        use_emoji: false,
        use_colors: false,
    };

    // Should use text instead of emojis
    assert_eq!(status.get_emoji(&opts), "=".to_string()); // Up to date
    assert_eq!(checkout.get_emoji(&opts), "OK".to_string()); // Checked out synced
}

#[test]
fn test_emoji_width_calculation_with_complex_combinations() {
    let repo = Repo {
        path: PathBuf::from("/tmp/test-repo"),
        name: "test-repo".to_string(),
        slug: Some("user/test-repo".to_string()),
    };

    // Create statuses with different emoji combinations that have varying widths
    let simple_emoji_status = RepoStatus {
        repo: repo.clone(),
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::UpToDate, // 🟢 (2 chars)
        error: None,
    };

    let ahead_status = RepoStatus {
        repo: repo.clone(),
        branch: Some("feature".to_string()),
        commit_sha: Some("def5678".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::Ahead(15), // ⬆️15 (4 chars)
        error: None,
    };

    let diverged_status = RepoStatus {
        repo: repo.clone(),
        branch: Some("develop".to_string()),
        commit_sha: Some("ghi9012".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::Diverged(5, 3), // 🔀5↑3↓ (6 chars)
        error: None,
    };

    let error_status = RepoStatus {
        repo: repo.clone(),
        branch: Some("bugfix".to_string()),
        commit_sha: Some("jkl3456".to_string()),
        is_clean: true,
        changes: gx::git::StatusChanges::default(),
        remote_status: gx::git::RemoteStatus::Error("timeout".to_string()), // ⚠️tim (5 chars)
        error: None,
    };

    let items = vec![
        &simple_emoji_status,
        &ahead_status,
        &diverged_status,
        &error_status,
    ];
    let widths = AlignmentWidths::calculate(&items);

    // The emoji width should be calculated based on the widest emoji combination
    // 🔀5↑3↓ should be the widest at 6 characters
    assert!(
        widths.emoji_width >= 6,
        "Emoji width should be at least 6 for complex combinations, got {}",
        widths.emoji_width
    );

    // Verify individual emoji widths
    let opts = StatusOptions::default();
    assert_eq!(simple_emoji_status.get_emoji(&opts), "🟢");
    assert_eq!(ahead_status.get_emoji(&opts), "⬆️15");
    assert_eq!(diverged_status.get_emoji(&opts), "🔀5↑3↓");
    assert_eq!(error_status.get_emoji(&opts), "⚠️tim");
}
