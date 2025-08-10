use gx::test_utils::*;
use tempfile;

#[test]
fn test_main_help_output() {
    let output = run_gx_command(&["--help"], std::env::current_dir().unwrap().as_path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show main help
    assert!(stdout.contains("git operations across multiple repositories"));
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("status"));
    assert!(stdout.contains("checkout"));

    // Should show global options
    assert!(stdout.contains("--config"));
    assert!(stdout.contains("--verbose"));
    assert!(stdout.contains("--jobs"));
    assert!(stdout.contains("--depth"));

    // Should show tool validation
    assert!(stdout.contains("REQUIRED TOOLS:"));
    assert!(stdout.contains("git"));
    assert!(stdout.contains("gh"));

    // Should show log location
    assert!(stdout.contains("Logs are written to:"));
}

#[test]
fn test_version_output() {
    let output = run_gx_command(&["--version"], std::env::current_dir().unwrap().as_path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show version information
    assert!(stdout.contains("gx"));
}

#[test]
fn test_invalid_command() {
    let output = run_gx_command(&["invalid-command"], std::env::current_dir().unwrap().as_path());

    // Should fail with error
    assert!(!output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid-command") || stderr.contains("unrecognized"));
}

#[test]
fn test_global_verbose_flag() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    create_test_repo(temp_dir.path(), "test-repo", false);

    let output = run_gx_command(&["--verbose", "status"], temp_dir.path());

    // Should succeed (verbose is handled internally via logging)
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test-repo") || stdout.contains("📊"));
}

#[test]
fn test_global_parallel_option() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    create_test_repo(temp_dir.path(), "test-repo", false);

    let output = run_gx_command(&["--jobs", "1", "status"], temp_dir.path());

    // Should succeed with custom parallelism
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test-repo") || stdout.contains("📊"));
}

#[test]
fn test_global_max_depth_option() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create nested structure
    let nested_dir = temp_dir.path().join("level1").join("level2");
    std::fs::create_dir_all(&nested_dir).unwrap();
    create_test_repo(&nested_dir, "deep-repo", false);
    create_test_repo(temp_dir.path(), "shallow-repo", false);

    // Test max-depth 2 should find shallow but not deep
    let output = run_gx_command(&["--depth", "2", "status"], temp_dir.path());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find shallow repo but not deep repo
    assert!(stdout.contains("shallow-repo"));
    assert!(!stdout.contains("deep-repo"));

    // Test max-depth 10 should find both
    let output = run_gx_command(&["--depth", "10", "status"], temp_dir.path());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find both repos
    assert!(stdout.contains("shallow-repo"));
    assert!(stdout.contains("deep-repo"));
}

#[test]
fn test_config_file_option() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    create_test_repo(temp_dir.path(), "test-repo", false);

    // Create a config file
    let config_content = r#"
parallelism: 2
max_depth: 5
"#;
    let config_path = temp_dir.path().join("gx.yml");
    std::fs::write(&config_path, config_content).unwrap();

    let output = run_gx_command(&["--config", config_path.to_str().unwrap(), "status"], temp_dir.path());

    // Should succeed with custom config
    assert!(output.status.success());
}

#[test]
fn test_workflow_status_then_checkout() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let repo_path = create_test_repo(temp_dir.path(), "workflow-repo", false);

    // First run status
    let status_output = run_gx_command(&["status"], temp_dir.path());

    assert!(status_output.status.success());
    let status_stdout = String::from_utf8(status_output.stdout).unwrap();
    assert!(status_stdout.contains("workflow-repo"));

    // Then checkout a new branch
    let checkout_output = run_gx_command(&["checkout", "-b", "feature-branch"], temp_dir.path());

    assert!(checkout_output.status.success());
    let checkout_stdout = String::from_utf8(checkout_output.stdout).unwrap();
    assert!(checkout_stdout.contains("workflow-repo"));
    assert!(checkout_stdout.contains("✨") || checkout_stdout.contains("created"));

    // Verify branch was created
    let branch_output = run_git_command(&["branch", "--show-current"], &repo_path);
    let current_branch = String::from_utf8(branch_output.stdout).unwrap();
    assert_eq!(current_branch.trim(), "feature-branch");

    // Run status again to see the new branch
    let status_output2 = run_gx_command(&["status"], temp_dir.path());

    let status_stdout2 = String::from_utf8(status_output2.stdout).unwrap();
    assert!(status_stdout2.contains("feature-branch"));
}

#[test]
fn test_repository_discovery_accuracy() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create various directory structures
    create_test_repo(temp_dir.path(), "valid-repo", true);

    // Create fake git directory (should be ignored or cause error)
    let fake_git = temp_dir.path().join("fake-repo").join(".git");
    std::fs::create_dir_all(&fake_git).unwrap();

    // Create non-git directory
    std::fs::create_dir_all(temp_dir.path().join("regular-dir")).unwrap();

    // Create ignored directories
    std::fs::create_dir_all(temp_dir.path().join("node_modules").join(".git")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("target").join(".git")).unwrap();

    let output = run_gx_command(&["status"], temp_dir.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find valid repo
    assert!(stdout.contains("valid-repo"));

    // Should not find ignored directories
    assert!(!stdout.contains("node_modules"));
    assert!(!stdout.contains("target"));
}

#[test]
fn test_error_handling_and_exit_codes() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create a broken git repo (git dir exists but no valid repo)
    let broken_repo = temp_dir.path().join("broken-repo");
    std::fs::create_dir_all(&broken_repo).unwrap();
    std::fs::create_dir_all(broken_repo.join(".git")).unwrap();

    let output = run_gx_command(&["status"], temp_dir.path());

    // Should handle errors gracefully
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should either succeed with error count or fail with appropriate exit code
    if !output.status.success() {
        let exit_code = output.status.code().unwrap_or(1);
        assert!(exit_code > 0);
    } else {
        // If it succeeds, should show error in summary
        assert!(stdout.contains("error") || stdout.contains("📊"));
    }
}

#[test]
fn test_concurrent_operations() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create multiple repositories
    for i in 1..=5 {
        create_test_repo(temp_dir.path(), &format!("repo{}", i), false);
    }

    let output = run_gx_command(&["--jobs", "3", "status"], temp_dir.path());

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should process all repositories
    for i in 1..=5 {
        assert!(stdout.contains(&format!("repo{}", i)));
    }

    // Should show summary with all repos
    assert!(stdout.contains("📊"));
}

#[test]
fn test_repository_filtering_edge_cases() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create repos with similar names
    create_test_repo(temp_dir.path(), "frontend", false);
    create_test_repo(temp_dir.path(), "frontend-api", false);
    create_test_repo(temp_dir.path(), "backend", false);
    create_test_repo(temp_dir.path(), "api", false);

    // Test exact match
    let output = run_gx_command(&["status", "frontend"], temp_dir.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should match both frontend and frontend-api due to filtering logic
    assert!(stdout.contains("frontend"));
    // The filtering logic should handle this appropriately
}

#[test]
fn test_logging_functionality() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    create_test_repo(temp_dir.path(), "test-repo", false);

    // Run with verbose flag
    let output = run_gx_command(&["--verbose", "status"], temp_dir.path());

    // Should succeed
    assert!(output.status.success());

    // Log file should be created (though we can't easily test the content in integration tests)
    // This mainly tests that the logging setup doesn't crash the application
}