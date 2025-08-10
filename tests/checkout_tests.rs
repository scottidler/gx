use gx::test_utils::*;

#[test]
fn test_checkout_existing_branch() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create a feature branch first
    run_git_command(&["checkout", "-b", "feature-branch"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    // Test gx checkout to existing branch
    let output = run_gx_command(&["checkout", "feature-branch", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show successful checkout
    assert!(stdout.contains("📥") || stdout.contains("frontend"));
    assert!(stdout.contains("frontend"));
    assert!(stdout.contains("feature-branch"));

    // Most importantly, verify branch was actually switched
    assert_eq!(get_current_branch(&frontend_path), "feature-branch");

    // Verify command succeeded
    assert!(output.status.success(), "Command should have succeeded");
}

#[test]
fn test_checkout_create_new_branch() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Test creating new branch with -b flag
    let output = run_gx_command(&["checkout", "-b", "new-feature", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show branch creation
    assert!(stdout.contains("✨") || stdout.contains("created"));
    assert!(stdout.contains("frontend"));

    // Verify branch was created and switched
    assert_eq!(get_current_branch(&frontend_path), "new-feature");

    // Verify command succeeded
    assert!(output.status.success(), "Command should have succeeded");
}

#[test]
fn test_checkout_with_stash() {
    let workspace = create_full_test_workspace();
    let frontend_path = workspace.path().join("frontend");

    // Create a feature branch first
    run_git_command(&["checkout", "-b", "feature"], &frontend_path);
    run_git_command(&["checkout", "main"], &frontend_path);

    // Create uncommitted changes on main branch
    std::fs::write(frontend_path.join("modified.txt"), "modified content").unwrap();
    run_git_command(&["add", "modified.txt"], &frontend_path);
    std::fs::write(frontend_path.join("modified.txt"), "more changes").unwrap();

    // Verify we have changes to stash
    let status_output = run_git_command(&["status", "--porcelain"], &frontend_path);
    let status_text = String::from_utf8(status_output.stdout).unwrap();
    assert!(!status_text.trim().is_empty(), "Should have uncommitted changes to stash");

    // Test checkout with stash flag
    let output = run_gx_command(&["checkout", "feature", "-s", "frontend"], workspace.path());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show stash operation
    assert!(stdout.contains("📦") || stdout.contains("stashed"));
    assert!(stdout.contains("frontend"));

    // Verify branch was switched
    assert_eq!(get_current_branch(&frontend_path), "feature");

    // Verify stash was created
    let stash_output = run_git_command(&["stash", "list"], &frontend_path);
    let stash_list = String::from_utf8(stash_output.stdout).unwrap();
    assert!(stash_list.contains("gx auto-stash"));
}

#[test]
fn test_checkout_help_output() {
    let output = run_gx_command(&["checkout", "--help"], &std::env::current_dir().unwrap());

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should show help content
    assert!(stdout.contains("Checkout branches across multiple repositories"));
    assert!(stdout.contains("-b"));
    assert!(stdout.contains("-s"));

    // Should succeed
    assert!(output.status.success());
}
