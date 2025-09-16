use gx::test_utils::*;
use std::process::Command;

#[test]
fn test_status_discovers_repositories() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find our test repositories
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("backend"));
    assert!(stdout.contains("api"));

    // Should succeed
    assert!(output.status.success());
}

#[test]
fn test_status_shows_commit_hash() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "-p", "frontend"], workspace.path());
    assert!(output.status.success(), "gx status command should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show 7-character commit hash
    let lines: Vec<&str> = stdout.lines().collect();
    let status_line = lines.iter().find(|line| line.contains("frontend"));

    if status_line.is_none() {
        panic!("Could not find frontend in output. Full stdout:\n{stdout}");
    }

    let status_line = status_line.unwrap();

    // Should have format: "branch commit_hash emoji path/reposlug"
    let parts: Vec<&str> = status_line.split_whitespace().collect();

    if parts.len() < 4 {
        panic!(
            "Expected at least 4 parts in status line, got {} parts: {:?}\nFull line: '{}'\nFull output:\n{}",
            parts.len(),
            parts,
            status_line,
            stdout
        );
    }

    // Second part should be 7-character commit hash
    let commit_hash = parts[1];
    assert_eq!(
        commit_hash.len(),
        7,
        "Commit hash should be 7 characters, got: '{commit_hash}'"
    );
    assert!(
        commit_hash.chars().all(|c| c.is_ascii_hexdigit()),
        "Commit hash should be hex digits, got: '{commit_hash}'"
    );
}

#[test]
fn test_status_filtering_by_pattern() {
    let workspace = create_test_workspace();

    let output = run_gx_command(&["status", "-p", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show only frontend
    assert!(stdout.contains("frontend"));
    assert!(!stdout.contains("backend"));
    assert!(!stdout.contains("api"));

    // Should succeed
    assert!(output.status.success());
}

#[test]
fn test_status_parallel_option() {
    let workspace = create_test_workspace();

    // Use the current directory to run gx with global flags
    let output = Command::new(get_gx_binary_path())
        .args(["--jobs", "2", "status"])
        .current_dir(workspace.path())
        .output()
        .unwrap();

    // Should succeed with custom parallelism
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("frontend"));
}

#[test]
fn test_status_help_output() {
    let output = run_gx_command(&["status", "--help"], &std::env::current_dir().unwrap());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show help content
    assert!(stdout.contains("Show git status across multiple repositories"));
    assert!(stdout.contains("--detailed"));
    assert!(stdout.contains("--no-emoji"));

    // Should succeed
    assert!(output.status.success());
}
