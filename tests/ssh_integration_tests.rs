use gx::ssh::{SshUrlBuilder, SshCommandDetector};

#[test]
fn test_ssh_url_construction_integration() {
    // Test various real repository slugs
    let test_cases = vec![
        "scottidler/gx",
        "tatari-tv/frontend-api",
        "microsoft/vscode",
        "rust-lang/rust",
    ];

    for repo_slug in test_cases {
        let result = SshUrlBuilder::build_ssh_url(repo_slug);
        assert!(result.is_ok(), "Failed to build SSH URL for {}", repo_slug);

        let url = result.unwrap();
        assert!(url.starts_with("git@github.com:"), "SSH URL should start with git@github.com: for {}", repo_slug);
        assert!(url.ends_with(".git"), "SSH URL should end with .git for {}", repo_slug);
        assert!(url.contains(repo_slug), "SSH URL should contain repo slug for {}", repo_slug);

        // Validate the generated URL
        let validation_result = SshUrlBuilder::validate_ssh_url(&url);
        assert!(validation_result.is_ok(), "Generated SSH URL should be valid for {}", repo_slug);
    }
}

#[test]
fn test_ssh_url_validation_comprehensive() {
    // Valid SSH URLs
    let valid_urls = vec![
        "git@github.com:scottidler/gx.git",
        "git@github.com:tatari-tv/frontend-api.git",
        "git@github.com:microsoft/vscode.git",
        "git@github.com:a/b.git", // Minimal valid case
    ];

    for url in valid_urls {
        let result = SshUrlBuilder::validate_ssh_url(url);
        assert!(result.is_ok(), "URL should be valid: {}", url);
    }

    // Invalid SSH URLs
    let invalid_urls = vec![
        "https://github.com/scottidler/gx.git", // HTTPS instead of SSH
        "git@github.com:scottidler/gx", // Missing .git
        "git@gitlab.com:scottidler/gx.git", // Wrong host
        "git@github.com:scottidler.git", // Missing org/repo format
        "git@github.com:/gx.git", // Empty org
        "git@github.com:scottidler/.git", // Empty repo
        "", // Empty string
        "invalid", // Completely invalid
    ];

    for url in invalid_urls {
        let result = SshUrlBuilder::validate_ssh_url(url);
        assert!(result.is_err(), "URL should be invalid: {}", url);
    }
}

#[test]
fn test_ssh_command_detection_integration() {
    // This test ensures the SSH command detection works without panicking
    // The actual git config may or may not be set, but it should return a valid command
    let result = SshCommandDetector::get_ssh_command();
    assert!(result.is_ok(), "SSH command detection should not fail");

    let ssh_command = result.unwrap();
    assert!(!ssh_command.is_empty(), "SSH command should not be empty");

    // Should either be "ssh" or a custom command
    assert!(
        ssh_command == "ssh" || ssh_command.contains("ssh"),
        "SSH command should be 'ssh' or contain 'ssh', got: {}",
        ssh_command
    );
}

#[test]
fn test_ssh_url_roundtrip() {
    // Test that we can build a URL and then validate it successfully
    let repo_slugs = vec![
        "scottidler/gx",
        "tatari-tv/frontend-api",
        "org/repo-with-dashes",
        "user/repo_with_underscores",
    ];

    for repo_slug in repo_slugs {
        // Build SSH URL
        let build_result = SshUrlBuilder::build_ssh_url(repo_slug);
        assert!(build_result.is_ok(), "Should build SSH URL for {}", repo_slug);

        let ssh_url = build_result.unwrap();

        // Validate the built URL
        let validate_result = SshUrlBuilder::validate_ssh_url(&ssh_url);
        assert!(validate_result.is_ok(), "Built SSH URL should be valid for {}", repo_slug);

        // Verify the URL contains the expected parts
        assert!(ssh_url.contains(repo_slug), "SSH URL should contain repo slug");
        assert_eq!(ssh_url, format!("git@github.com:{}.git", repo_slug));
    }
}

#[test]
fn test_ssh_error_handling() {
    // Test various error conditions

    // Invalid repo slug formats
    let invalid_slugs = vec![
        "", // Empty
        "no-slash", // No slash
        "too/many/slashes", // Too many slashes
        "/repo", // Empty org
        "org/", // Empty repo
        "org//repo", // Double slash
    ];

    for slug in invalid_slugs {
        let result = SshUrlBuilder::build_ssh_url(slug);
        assert!(result.is_err(), "Should fail for invalid slug: {}", slug);

        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("Invalid repository slug") ||
            error_msg.contains("Repository slug parts cannot be empty"),
            "Error message should be descriptive for slug: {}, got: {}",
            slug, error_msg
        );
    }
}

#[test]
fn test_ssh_url_special_characters() {
    // Test repo slugs with special but valid characters
    let special_cases = vec![
        ("org-name/repo-name", "git@github.com:org-name/repo-name.git"),
        ("user_name/repo_name", "git@github.com:user_name/repo_name.git"),
        ("123org/456repo", "git@github.com:123org/456repo.git"),
        ("a/b", "git@github.com:a/b.git"), // Minimal case
    ];

    for (slug, expected_url) in special_cases {
        let result = SshUrlBuilder::build_ssh_url(slug);
        assert!(result.is_ok(), "Should build URL for special slug: {}", slug);

        let url = result.unwrap();
        assert_eq!(url, expected_url, "URL should match expected for slug: {}", slug);

        // Validate the URL
        let validation = SshUrlBuilder::validate_ssh_url(&url);
        assert!(validation.is_ok(), "Special URL should be valid: {}", url);
    }
}
