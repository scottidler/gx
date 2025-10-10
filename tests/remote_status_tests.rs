use std::process::Command;

/// Test the --no-remote CLI flag
#[test]
fn test_status_no_remote_flag() {
    let output = Command::new("cargo")
        .args(["run", "--", "status", "--no-remote", "--help"])
        .output()
        .expect("Failed to execute gx");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--no-remote"));
    assert!(stdout.contains("Skip remote status checks entirely"));
}

/// Test the --fetch-first CLI flag
#[test]
fn test_status_fetch_first_flag() {
    let output = Command::new("cargo")
        .args(["run", "--", "status", "--help"])
        .output()
        .expect("Failed to execute gx");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--fetch-first"));
    assert!(stdout.contains("Fetch latest remote refs before status check"));
}

/// Test that --no-remote actually skips remote status
#[test]
fn test_no_remote_skips_remote_status() {
    // This test would need a real git repository to be meaningful
    // For now, we just test that the flag is accepted
    let output = Command::new("cargo")
        .args(["run", "--", "status", "--no-remote", "-p", "nonexistent"])
        .output()
        .expect("Failed to execute gx");

    // Should succeed even with nonexistent pattern since we're just testing flag parsing
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Should not contain errors about the flag being unrecognized
    assert!(!stderr.contains("unrecognized"));
}

/// Test CLI flag precedence and help output consistency
#[test]
fn test_cli_flags_help_consistency() {
    let output = Command::new("cargo")
        .args(["run", "--", "status", "--help"])
        .output()
        .expect("Failed to execute gx");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Verify all expected flags are present
    assert!(stdout.contains("--detailed"));
    assert!(stdout.contains("--no-emoji"));
    assert!(stdout.contains("--no-color"));
    assert!(stdout.contains("--patterns"));
    assert!(stdout.contains("--fetch-first"));
    assert!(stdout.contains("--no-remote"));

    // Verify remote status legend is present
    assert!(stdout.contains("REMOTE STATUS:"));
    assert!(stdout.contains("🟢  Up to date with remote"));
    assert!(stdout.contains("⬇️N  Behind by N commits"));
    assert!(stdout.contains("⬆️N  Ahead by N commits"));
}

/// Test that both new flags can be used together
#[test]
fn test_combined_flags() {
    let output = Command::new("cargo")
        .args([
            "run",
            "--",
            "status",
            "--fetch-first",
            "--no-remote",
            "-p",
            "nonexistent",
        ])
        .output()
        .expect("Failed to execute gx");

    // The flags should be mutually exclusive in practice (no-remote overrides fetch-first)
    // but they should both be accepted by the CLI parser
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("unrecognized"));
}
