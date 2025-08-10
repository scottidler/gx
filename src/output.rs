use crate::git::{RepoStatus, StatusChanges, RemoteStatus, CheckoutResult, CheckoutAction, CloneResult, CloneAction};
use colored::*;
use std::io::{self, Write};

#[derive(Debug)]
pub struct StatusOptions {
    pub show_all: bool,
    pub detailed: bool,
    pub use_emoji: bool,
    pub use_colors: bool,
}

impl Default for StatusOptions {
    fn default() -> Self {
        Self {
            show_all: false,
            detailed: false,
            use_emoji: true,
            use_colors: true,
        }
    }
}

/// Display status results with summary
pub fn display_status_results(results: Vec<RepoStatus>, opts: &StatusOptions) {
    let mut clean_count = 0;
    let mut dirty_count = 0;
    let mut error_count = 0;

    for result in &results {
        match &result.error {
            Some(err) => {
                display_error_status(result, err, opts);
                error_count += 1;
            }
            None if result.is_clean => {
                clean_count += 1;
                if opts.show_all {
                    display_clean_status(result, opts);
                }
            }
            None => {
                dirty_count += 1;
                if opts.detailed {
                    display_detailed_status(result, opts);
                } else {
                    display_compact_status(result, opts);
                }
            }
        }
    }

    // Display summary
    display_summary(clean_count, dirty_count, error_count, opts);
}

/// Display compact one-line status with slam-style alignment
fn display_compact_status(status: &RepoStatus, opts: &StatusOptions) {
    let changes = &status.changes;

    // Branch name with fixed width for alignment (like slam)
    let branch = status.branch.as_deref().unwrap_or("unknown");
    let branch_display = if opts.use_colors {
        format!("{:>6}", branch.green())
    } else {
        format!("{:>6}", branch)
    };

    // Commit hash (7 characters or spaces if not available)
    let commit_display = status.commit_sha.as_deref().unwrap_or("       ");

    // Status emoji - determine the primary status indicator
    let status_emoji = if !status.is_clean {
        // Show file change status for dirty repos
        if changes.untracked > 0 {
            if opts.use_emoji { "❓" } else { "?" }
        } else if changes.modified > 0 {
            if opts.use_emoji { "📝" } else { "M" }
        } else if changes.added > 0 {
            if opts.use_emoji { "➕" } else { "A" }
        } else if changes.deleted > 0 {
            if opts.use_emoji { "❌" } else { "D" }
        } else if changes.staged > 0 {
            if opts.use_emoji { "🎯" } else { "S" }
        } else {
            if opts.use_emoji { "📝" } else { "M" }
        }
    } else {
        // Show remote status for clean repos
        match &status.remote_status {
            RemoteStatus::UpToDate => if opts.use_emoji { "🟢" } else { "=" },
            RemoteStatus::Ahead(_) => if opts.use_emoji { "⬆️" } else { "↑" },
            RemoteStatus::Behind(_) => if opts.use_emoji { "⬇️" } else { "↓" },
            RemoteStatus::Diverged(_, _) => if opts.use_emoji { "🔀" } else { "±" },
            RemoteStatus::NoRemote => if opts.use_emoji { "📍" } else { "~" },
            RemoteStatus::Error(_) => if opts.use_emoji { "⚠️" } else { "!" },
        }
    };

    // Repository slug
    let repo_display = status.repo.slug.as_ref().unwrap_or(&status.repo.name);
    let repo_name = if opts.use_colors {
        repo_display.cyan().to_string()
    } else {
        repo_display.clone()
    };

    // Format: branch commit_hash emoji repo_slug
    println!("{} {} {} {}", branch_display, commit_display, status_emoji, repo_name);
}

/// Display detailed file-by-file status (placeholder for now)
fn display_detailed_status(status: &RepoStatus, opts: &StatusOptions) {
    let repo_header = if opts.use_colors {
        format!("📁 {}", status.repo.name.cyan().bold())
    } else {
        format!("Repository: {}", status.repo.name)
    };

    println!("{}", repo_header);

    if let Some(branch) = &status.branch {
        let branch_info = if opts.use_colors {
            format!("  Branch: {}", branch.green())
        } else {
            format!("  Branch: {}", branch)
        };
        println!("{}", branch_info);
    }

    // Remote status in detailed view
    let remote_info = match &status.remote_status {
        RemoteStatus::UpToDate => "  Remote: 🟢 Up to date".to_string(),
        RemoteStatus::Ahead(n) => format!("  Remote: ⬆️  Ahead by {} commit{}", n, if *n == 1 { "" } else { "s" }),
        RemoteStatus::Behind(n) => format!("  Remote: ⬇️  Behind by {} commit{}", n, if *n == 1 { "" } else { "s" }),
        RemoteStatus::Diverged(ahead, behind) => format!("  Remote: 🔀 Ahead by {}, behind by {}", ahead, behind),
        RemoteStatus::NoRemote => "  Remote: 📍 No tracking branch".to_string(),
        RemoteStatus::Error(e) => format!("  Remote: ⚠️  Error: {}", e),
    };

    if opts.use_colors {
        let colored_remote = match &status.remote_status {
            RemoteStatus::UpToDate => remote_info.green().to_string(),
            RemoteStatus::Ahead(_) => remote_info.blue().to_string(),
            RemoteStatus::Behind(_) => remote_info.yellow().to_string(),
            RemoteStatus::Diverged(_, _) => remote_info.magenta().to_string(),
            RemoteStatus::NoRemote => remote_info.dimmed().to_string(),
            RemoteStatus::Error(_) => remote_info.red().to_string(),
        };
        println!("{}", colored_remote);
    } else {
        // Non-emoji fallback for detailed view
        let plain_remote = match &status.remote_status {
            RemoteStatus::UpToDate => "  Remote: Up to date".to_string(),
            RemoteStatus::Ahead(n) => format!("  Remote: Ahead by {} commit{}", n, if *n == 1 { "" } else { "s" }),
            RemoteStatus::Behind(n) => format!("  Remote: Behind by {} commit{}", n, if *n == 1 { "" } else { "s" }),
            RemoteStatus::Diverged(ahead, behind) => format!("  Remote: Ahead by {}, behind by {}", ahead, behind),
            RemoteStatus::NoRemote => "  Remote: No tracking branch".to_string(),
            RemoteStatus::Error(e) => format!("  Remote: Error: {}", e),
        };
        println!("{}", plain_remote);
    }

    // For detailed view, we'd need to run git status without --porcelain
    // For now, show the summary
    display_changes_summary(&status.changes, opts, "  ");
    println!(); // Empty line between repos
}

/// Display clean repository status using slam-style alignment
fn display_clean_status(status: &RepoStatus, opts: &StatusOptions) {
    // Branch name with fixed width for alignment
    let branch = status.branch.as_deref().unwrap_or("unknown");
    let branch_display = if opts.use_colors {
        format!("{:>6}", branch.green())
    } else {
        format!("{:>6}", branch)
    };

    // Commit hash (7 characters or spaces if not available)
    let commit_display = status.commit_sha.as_deref().unwrap_or("       ");

    // Status emoji for clean repos (show remote status)
    let status_emoji = match &status.remote_status {
        RemoteStatus::UpToDate => if opts.use_emoji { "🟢" } else { "=" },
        RemoteStatus::Ahead(_) => if opts.use_emoji { "⬆️" } else { "↑" },
        RemoteStatus::Behind(_) => if opts.use_emoji { "⬇️" } else { "↓" },
        RemoteStatus::Diverged(_, _) => if opts.use_emoji { "🔀" } else { "±" },
        RemoteStatus::NoRemote => if opts.use_emoji { "📍" } else { "~" },
        RemoteStatus::Error(_) => if opts.use_emoji { "⚠️" } else { "!" },
    };

    // Repository slug
    let repo_display = status.repo.slug.as_ref().unwrap_or(&status.repo.name);
    let repo_name = if opts.use_colors {
        repo_display.cyan().to_string()
    } else {
        repo_display.clone()
    };

    // Format: branch commit_hash emoji repo_slug
    println!("{} {} {} {}", branch_display, commit_display, status_emoji, repo_name);
}

/// Display error status
fn display_error_status(status: &RepoStatus, error: &str, opts: &StatusOptions) {
    let error_indicator = if opts.use_emoji { "❌" } else { "ERROR" };
    let repo_name = if opts.use_colors {
        status.repo.name.red().to_string()
    } else {
        status.repo.name.clone()
    };

    let error_msg = if opts.use_colors {
        error.red().to_string()
    } else {
        error.to_string()
    };

    println!("{} {} {}", repo_name, error_indicator, error_msg);
}

/// Display changes summary with optional prefix
fn display_changes_summary(changes: &StatusChanges, opts: &StatusOptions, prefix: &str) {
    if opts.use_emoji {
        if changes.modified > 0 {
            println!("{}📝 {} modified", prefix, changes.modified);
        }
        if changes.added > 0 {
            println!("{}➕ {} added", prefix, changes.added);
        }
        if changes.deleted > 0 {
            println!("{}❌ {} deleted", prefix, changes.deleted);
        }
        if changes.untracked > 0 {
            println!("{}❓ {} untracked", prefix, changes.untracked);
        }
        if changes.staged > 0 {
            println!("{}🎯 {} staged", prefix, changes.staged);
        }
        if changes.renamed > 0 {
            println!("{}🔄 {} renamed", prefix, changes.renamed);
        }
    } else {
        if changes.modified > 0 {
            println!("{}Modified: {}", prefix, changes.modified);
        }
        if changes.added > 0 {
            println!("{}Added: {}", prefix, changes.added);
        }
        if changes.deleted > 0 {
            println!("{}Deleted: {}", prefix, changes.deleted);
        }
        if changes.untracked > 0 {
            println!("{}Untracked: {}", prefix, changes.untracked);
        }
        if changes.staged > 0 {
            println!("{}Staged: {}", prefix, changes.staged);
        }
        if changes.renamed > 0 {
            println!("{}Renamed: {}", prefix, changes.renamed);
        }
    }
}

/// Display final summary
fn display_summary(clean_count: usize, dirty_count: usize, error_count: usize, opts: &StatusOptions) {
    if clean_count == 0 && dirty_count == 0 && error_count == 0 {
        let msg = if opts.use_emoji {
            "🔍 No repositories found"
        } else {
            "No repositories found"
        };
        println!("\n{}", msg);
        return;
    }

    let summary = if opts.use_emoji {
        format!("\n📊 {} clean, {} dirty, {} errors", clean_count, dirty_count, error_count)
    } else {
        format!("\nSummary: {} clean, {} dirty, {} errors", clean_count, dirty_count, error_count)
    };

    if opts.use_colors {
        println!("\n📊 {} clean, {} dirty, {} errors",
                 clean_count.to_string().green(),
                 dirty_count.to_string().yellow(),
                 error_count.to_string().red());
    } else {
        println!("{}", summary);
    }
}







/// Display a single clone result immediately (for streaming output like slam)
pub fn display_clone_result_immediate(result: &CloneResult) {
    match &result.error {
        Some(err) => {
            println!("⚠️  {} Failed: {}", result.repo_slug.red().bold(), err.red());
        }
        None => {
            let (emoji, _action) = match result.action {
                CloneAction::Cloned => ("📥", "Cloned"),
                CloneAction::Updated => ("📥", "Updated"),
                CloneAction::Stashed => ("📥", "Updated (stashed)"),
                CloneAction::DirectoryNotGitRepo => ("🏠", "Directory exists but not git"),
                CloneAction::DifferentRemote => ("🔗", "Different remote URL"),
            };
            println!("{} {}", emoji, result.repo_slug.cyan().bold());
        }
    }
    io::stdout().flush().expect("Failed to flush stdout");
}

/// Display a single checkout result immediately (for streaming output like slam)
pub fn display_checkout_result_immediate(result: &CheckoutResult) {
    match &result.error {
        Some(err) => {
            let repo_display = result.repo.slug.as_ref().unwrap_or(&result.repo.name);
            println!("❌ {} failed to checkout {}: {}",
                repo_display.red().bold(),
                result.branch_name.red(),
                err
            );
        }
        None => {
            let repo_display = result.repo.slug.as_ref().unwrap_or(&result.repo.name);
            let (emoji, _action_text) = match result.action {
                CheckoutAction::CheckedOutSynced => ("📥", "checked out and synced"),
                CheckoutAction::CreatedFromRemote => ("✨", "created from remote"),
                CheckoutAction::Stashed => ("📦", "stashed and checked out"),
                CheckoutAction::HasUntracked => ("⚠️", "checked out (has untracked files)"),
            };
            println!("{} {} {}", emoji, repo_display.cyan().bold(), result.branch_name.green());
        }
    }
    io::stdout().flush().expect("Failed to flush stdout");
}