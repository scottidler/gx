use gx::git::{RemoteStatus, RepoStatus, StatusChanges};
use gx::output::{AlignmentWidths, StatusOptions, UnifiedDisplay};
use local::repo::Repo;

/// Test that exposes the alignment issue with different emoji widths
#[test]
fn test_emoji_alignment_consistency() {
    // Create test repos with different emoji statuses
    let test_repos = vec![
        create_test_status("🟢", "scottidler/test1"),  // 2 width
        create_test_status("↓1", "scottidler/test2"),  // 2 width
        create_test_status("↑12", "scottidler/test3"), // 3 width
        create_test_status("🚨 git", "scottidler/test4"), // 6 width
        create_test_status("🔀3↑2↓", "scottidler/test5"), // 6 width
    ];

    // Calculate alignment widths
    let widths = AlignmentWidths::calculate(&test_repos);
    let opts = StatusOptions::default();

    println!("Calculated emoji width: {}", widths.emoji_width);

    // Capture output for each repo using our custom padding
    let mut outputs = Vec::new();
    for repo in &test_repos {
        let emoji = repo.get_emoji(&opts);
        let output = gx::output::pad_to_width(&emoji, widths.emoji_width);
        let display_width = gx::output::calculate_display_width(&output);
        outputs.push((emoji.clone(), output.clone(), display_width));
        println!(
            "Emoji: '{}' -> Formatted: '{}' (display_width: {})",
            emoji, output, display_width
        );
    }

    // All formatted outputs should have the same display width
    let first_width = outputs[0].2;
    for (emoji, formatted, display_width) in outputs.iter() {
        assert_eq!(
            *display_width,
            first_width,
            "Emoji '{}' formatted as '{}' has display width {} but expected {}. This proves alignment is fucked!",
            emoji,
            formatted,
            display_width,
            first_width
        );
    }
}

fn create_test_status(emoji_type: &str, repo_slug: &str) -> RepoStatus {
    let repo = Repo::from_slug(repo_slug.to_string());

    // Create different remote statuses to generate different emojis
    let remote_status = match emoji_type {
        "🟢" => RemoteStatus::UpToDate,
        "↓1" => RemoteStatus::Behind(1),
        "↑12" => RemoteStatus::Ahead(12),
        "🚨 git" => RemoteStatus::Error("git ls-remote failed".to_string()),
        "🔀3↑2↓" => RemoteStatus::Diverged(3, 2),
        _ => RemoteStatus::UpToDate,
    };

    RepoStatus {
        repo,
        branch: Some("main".to_string()),
        commit_sha: Some("abc1234".to_string()),
        is_clean: true,
        changes: StatusChanges::default(),
        remote_status,
        error: None,
    }
}

/// Test the actual display width calculation
#[test]
fn test_emoji_display_width_calculation() {
    // Test individual width calculations using unicode-display-width values
    let test_cases = vec![
        ("🟢", 2),
        ("↓1", 2),     // bare arrow (1) + digit (1) = 2; crate width matches terminal
        ("↑12", 3),    // bare arrow (1) + "12" (2) = 3; crate width matches terminal
        ("🚨 git", 6), // emoji (2) + " git" (4) = 6
        ("🔀3↑2↓", 6),
    ];

    for (emoji, expected_width) in test_cases {
        let calculated = gx::output::calculate_display_width(emoji);
        println!(
            "Emoji '{}': calculated={}, expected={}",
            emoji, calculated, expected_width
        );

        // This test will fail and show us what's wrong
        assert_eq!(
            calculated, expected_width,
            "Display width calculation is fucked for emoji '{}'. Got {} but expected {}",
            emoji, calculated, expected_width
        );
    }
}
