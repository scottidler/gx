use crate::config::OutputVerbosity;
use crate::create::{CreateAction, CreateResult};
use crate::git::{
    CheckoutAction, CheckoutResult, CloneAction, CloneResult, RemoteStatus, RepoStatus,
};
use crate::review::{ReviewAction, ReviewResult};
use colored::*;
use eyre::{Context, Result};
use std::env;
use std::io::{self, Write};
use std::path::Path;
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
pub struct StatusOptions {
    pub verbosity: OutputVerbosity,
    pub use_emoji: bool,
    pub use_colors: bool,
}

impl Default for StatusOptions {
    fn default() -> Self {
        Self {
            verbosity: OutputVerbosity::Summary,
            use_emoji: true,
            use_colors: true,
        }
    }
}

/// Unified display trait for consistent formatting across different result types
pub trait UnifiedDisplay {
    fn get_branch(&self) -> Option<&str>;
    fn get_commit_sha(&self) -> Option<&str>;
    fn get_repo(&self) -> &crate::repo::Repo;
    fn get_emoji(&self, opts: &StatusOptions) -> String;
    fn get_error(&self) -> Option<&str>;
}

/// Implementation of UnifiedDisplay for RepoStatus
impl UnifiedDisplay for RepoStatus {
    fn get_branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    fn get_commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else if !self.is_clean {
            // File change status logic
            if self.changes.untracked > 0 {
                if opts.use_emoji {
                    "❓".to_string()
                } else {
                    "?".to_string()
                }
            } else if self.changes.modified > 0 {
                if opts.use_emoji {
                    "📝".to_string()
                } else {
                    "M".to_string()
                }
            } else if self.changes.added > 0 {
                if opts.use_emoji {
                    "➕".to_string()
                } else {
                    "A".to_string()
                }
            } else if self.changes.deleted > 0 {
                if opts.use_emoji {
                    "❌".to_string()
                } else {
                    "D".to_string()
                }
            } else if self.changes.staged > 0 {
                if opts.use_emoji {
                    "🎯".to_string()
                } else {
                    "S".to_string()
                }
            } else if opts.use_emoji {
                "📝".to_string()
            } else {
                "M".to_string()
            }
        } else {
            // Remote status logic for clean repos
            match &self.remote_status {
                RemoteStatus::UpToDate => {
                    if opts.use_emoji {
                        "🟢".to_string()
                    } else {
                        "=".to_string()
                    }
                }
                RemoteStatus::Ahead(n) => {
                    if opts.use_emoji {
                        format!("⬆️{n}")
                    } else {
                        format!("↑{n}")
                    }
                }
                RemoteStatus::Behind(n) => {
                    if opts.use_emoji {
                        format!("⬇️{n}")
                    } else {
                        format!("↓{n}")
                    }
                }
                RemoteStatus::Diverged(ahead, behind) => {
                    if opts.use_emoji {
                        format!("🔀{ahead}↑{behind}↓")
                    } else {
                        format!("±{ahead}↑{behind}↓")
                    }
                }
                RemoteStatus::NoRemote => {
                    if opts.use_emoji {
                        "📍".to_string()
                    } else {
                        "~".to_string()
                    }
                }
                RemoteStatus::Error(e) => {
                    if opts.use_emoji {
                        format!("⚠️{}", e.chars().take(3).collect::<String>())
                    } else {
                        format!("!{}", e.chars().take(3).collect::<String>())
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for CheckoutResult
impl UnifiedDisplay for CheckoutResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.branch_name)
    }

    fn get_commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else {
            match self.action {
                CheckoutAction::CheckedOutSynced => {
                    if opts.use_emoji {
                        "📥".to_string()
                    } else {
                        "OK".to_string()
                    }
                }
                CheckoutAction::CreatedFromRemote => {
                    if opts.use_emoji {
                        "✨".to_string()
                    } else {
                        "NEW".to_string()
                    }
                }
                CheckoutAction::Stashed => {
                    if opts.use_emoji {
                        "📦".to_string()
                    } else {
                        "STASH".to_string()
                    }
                }
                CheckoutAction::HasUntracked => {
                    if opts.use_emoji {
                        "⚠️".to_string()
                    } else {
                        "WARN".to_string()
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for &RepoStatus
impl UnifiedDisplay for &RepoStatus {
    fn get_branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    fn get_commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else if !self.is_clean {
            // File change status logic
            if self.changes.untracked > 0 {
                if opts.use_emoji {
                    "❓".to_string()
                } else {
                    "?".to_string()
                }
            } else if self.changes.modified > 0 {
                if opts.use_emoji {
                    "📝".to_string()
                } else {
                    "M".to_string()
                }
            } else if self.changes.added > 0 {
                if opts.use_emoji {
                    "➕".to_string()
                } else {
                    "A".to_string()
                }
            } else if self.changes.deleted > 0 {
                if opts.use_emoji {
                    "❌".to_string()
                } else {
                    "D".to_string()
                }
            } else if self.changes.staged > 0 {
                if opts.use_emoji {
                    "🎯".to_string()
                } else {
                    "S".to_string()
                }
            } else if opts.use_emoji {
                "📝".to_string()
            } else {
                "M".to_string()
            }
        } else {
            // Remote status logic for clean repos
            match &self.remote_status {
                RemoteStatus::UpToDate => {
                    if opts.use_emoji {
                        "🟢".to_string()
                    } else {
                        "=".to_string()
                    }
                }
                RemoteStatus::Ahead(n) => {
                    if opts.use_emoji {
                        format!("⬆️{n}")
                    } else {
                        format!("↑{n}")
                    }
                }
                RemoteStatus::Behind(n) => {
                    if opts.use_emoji {
                        format!("⬇️{n}")
                    } else {
                        format!("↓{n}")
                    }
                }
                RemoteStatus::Diverged(ahead, behind) => {
                    if opts.use_emoji {
                        format!("🔀{ahead}↑{behind}↓")
                    } else {
                        format!("±{ahead}↑{behind}↓")
                    }
                }
                RemoteStatus::NoRemote => {
                    if opts.use_emoji {
                        "📍".to_string()
                    } else {
                        "~".to_string()
                    }
                }
                RemoteStatus::Error(e) => {
                    if opts.use_emoji {
                        format!("⚠️{}", e.chars().take(3).collect::<String>())
                    } else {
                        format!("!{}", e.chars().take(3).collect::<String>())
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for &CheckoutResult
impl UnifiedDisplay for &CheckoutResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.branch_name)
    }

    fn get_commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else {
            match self.action {
                CheckoutAction::CheckedOutSynced => {
                    if opts.use_emoji {
                        "📥".to_string()
                    } else {
                        "OK".to_string()
                    }
                }
                CheckoutAction::CreatedFromRemote => {
                    if opts.use_emoji {
                        "✨".to_string()
                    } else {
                        "NEW".to_string()
                    }
                }
                CheckoutAction::Stashed => {
                    if opts.use_emoji {
                        "📦".to_string()
                    } else {
                        "STASH".to_string()
                    }
                }
                CheckoutAction::HasUntracked => {
                    if opts.use_emoji {
                        "⚠️".to_string()
                    } else {
                        "WARN".to_string()
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for CreateResult
impl UnifiedDisplay for CreateResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.change_id)
    }

    fn get_commit_sha(&self) -> Option<&str> {
        None // Create results don't have commit SHA in the same way
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else {
            match self.action {
                CreateAction::DryRun => {
                    if opts.use_emoji {
                        "👁️".to_string()
                    } else {
                        "DRY".to_string()
                    }
                }

                CreateAction::Committed => {
                    if opts.use_emoji {
                        "💾".to_string()
                    } else {
                        "COMMIT".to_string()
                    }
                }
                CreateAction::PrCreated => {
                    if opts.use_emoji {
                        "📥".to_string()
                    } else {
                        "PR".to_string()
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for &CreateResult
impl UnifiedDisplay for &CreateResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.change_id)
    }

    fn get_commit_sha(&self) -> Option<&str> {
        None
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else {
            match self.action {
                CreateAction::DryRun => {
                    if opts.use_emoji {
                        "👁️".to_string()
                    } else {
                        "DRY".to_string()
                    }
                }

                CreateAction::Committed => {
                    if opts.use_emoji {
                        "💾".to_string()
                    } else {
                        "COMMIT".to_string()
                    }
                }
                CreateAction::PrCreated => {
                    if opts.use_emoji {
                        "📥".to_string()
                    } else {
                        "PR".to_string()
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for ReviewResult
impl UnifiedDisplay for ReviewResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.change_id)
    }

    fn get_commit_sha(&self) -> Option<&str> {
        // Use this field to display PR number instead of commit SHA
        None // We'll need a different approach due to lifetime issues
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else {
            match self.action {
                ReviewAction::Listed => {
                    if opts.use_emoji {
                        "📋".to_string()
                    } else {
                        "LIST".to_string()
                    }
                }
                ReviewAction::Cloned => {
                    if opts.use_emoji {
                        "📥".to_string()
                    } else {
                        "CLONE".to_string()
                    }
                }
                ReviewAction::Approved => {
                    if opts.use_emoji {
                        "✅".to_string()
                    } else {
                        "APPROVE".to_string()
                    }
                }
                ReviewAction::Deleted => {
                    if opts.use_emoji {
                        "❌".to_string()
                    } else {
                        "DELETE".to_string()
                    }
                }
                ReviewAction::Purged => {
                    if opts.use_emoji {
                        "🧹".to_string()
                    } else {
                        "PURGE".to_string()
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Implementation of UnifiedDisplay for &ReviewResult
impl UnifiedDisplay for &ReviewResult {
    fn get_branch(&self) -> Option<&str> {
        Some(&self.change_id)
    }

    fn get_commit_sha(&self) -> Option<&str> {
        None
    }

    fn get_repo(&self) -> &crate::repo::Repo {
        &self.repo
    }

    fn get_emoji(&self, opts: &StatusOptions) -> String {
        if self.error.is_some() {
            if opts.use_emoji {
                "❌".to_string()
            } else {
                "ERROR".to_string()
            }
        } else {
            match self.action {
                ReviewAction::Listed => {
                    if opts.use_emoji {
                        "📋".to_string()
                    } else {
                        "LIST".to_string()
                    }
                }
                ReviewAction::Cloned => {
                    if opts.use_emoji {
                        "📥".to_string()
                    } else {
                        "CLONE".to_string()
                    }
                }
                ReviewAction::Approved => {
                    if opts.use_emoji {
                        "✅".to_string()
                    } else {
                        "APPROVE".to_string()
                    }
                }
                ReviewAction::Deleted => {
                    if opts.use_emoji {
                        "❌".to_string()
                    } else {
                        "DELETE".to_string()
                    }
                }
                ReviewAction::Purged => {
                    if opts.use_emoji {
                        "🧹".to_string()
                    } else {
                        "PURGE".to_string()
                    }
                }
            }
        }
    }

    fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Alignment widths for unified formatting
#[derive(Debug)]
pub struct AlignmentWidths {
    pub branch_width: usize,
    pub sha_width: usize,
    pub emoji_width: usize,
}

impl AlignmentWidths {
    /// Calculate alignment widths for a collection of UnifiedDisplay items
    pub fn calculate<T: UnifiedDisplay>(items: &[T]) -> Self {
        let branch_width = items
            .iter()
            .filter_map(|item| item.get_branch())
            .map(|branch| branch.len())
            .max()
            .unwrap_or(7) // "unknown".len() + padding
            .max(7); // Minimum width for readability

        let sha_width = 7; // Always 7 characters for SHA

        // Calculate actual emoji width by measuring all emoji combinations
        let emoji_width = items
            .iter()
            .map(|item| {
                let opts = StatusOptions::default();
                let emoji = item.get_emoji(&opts);
                emoji.width()
            })
            .max()
            .unwrap_or(2) // Fallback to 2 if no items
            .max(2); // Minimum width for readability

        AlignmentWidths {
            branch_width,
            sha_width,
            emoji_width,
        }
    }
}

/// Calculate relative path from current directory to repository
fn get_relative_repo_path(repo_path: &Path) -> String {
    if let Ok(current_dir) = env::current_dir() {
        if let Ok(relative) = repo_path.strip_prefix(&current_dir) {
            return relative.to_string_lossy().to_string();
        }
    }
    // Fallback to just the repo name if relative path calculation fails
    repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Format repository path with separate colors for path and repo slug
fn format_repo_path_with_colors(repo_path: &Path, repo_slug: &str, use_colors: bool) -> String {
    let relative_path = get_relative_repo_path(repo_path);

    // Handle the case where we're in the repo directory itself (relative path is empty or just ".")
    let display_path = if relative_path.is_empty() || relative_path == "." {
        repo_slug.to_string()
    } else {
        relative_path
    };

    if use_colors {
        // Find where the repo slug appears in the display path
        if let Some(slug_start) = display_path.rfind(repo_slug) {
            let path_prefix = &display_path[..slug_start];
            let slug_portion = &display_path[slug_start..];

            let colored_path = if path_prefix.is_empty() {
                slug_portion.cyan().to_string()
            } else {
                format!("{}{}", path_prefix.white(), slug_portion.cyan())
            };

            // Left-justify the path (no padding needed for left alignment)
            colored_path
        } else {
            // Fallback: if repo slug not found in path, color the whole thing
            display_path.cyan().to_string()
        }
    } else {
        display_path
    }
}

/// Display a single item using unified formatting
pub fn display_unified_format<T: UnifiedDisplay>(
    item: &T,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) {
    // Branch (right-justified)
    let branch = item.get_branch().unwrap_or("unknown");
    let branch_display = if opts.use_colors {
        format!("{:>width$}", branch.magenta(), width = widths.branch_width)
    } else {
        format!("{:>width$}", branch, width = widths.branch_width)
    };

    // Commit SHA (fixed width) - special handling for ReviewResult
    let commit_display = item.get_commit_sha().unwrap_or("-------");
    let sha_display = if opts.use_colors {
        format!(
            "{:width$}",
            commit_display.bright_black(),
            width = widths.sha_width
        )
    } else {
        format!("{:width$}", commit_display, width = widths.sha_width)
    };

    // Emoji/Status indicator (left-aligned)
    let emoji = item.get_emoji(opts);
    let emoji_display = format!("{:<width$}", emoji, width = widths.emoji_width);

    // Repository path/slug
    let repo = item.get_repo();
    let repo_slug = &repo.slug;
    let repo_display = format_repo_path_with_colors(&repo.path, repo_slug, opts.use_colors);

    // Final format: <branch> <sha> <emoji> <repo>
    println!("{branch_display} {sha_display} {emoji_display} {repo_display}");

    // Handle error display
    if let Some(error) = item.get_error() {
        let error_msg = if opts.use_colors {
            format!("  Error: {}", error.red())
        } else {
            format!("  Error: {error}")
        };
        println!("{error_msg}");
    }
}

/// Display a ReviewResult with PR number information
pub fn display_review_result(
    result: &ReviewResult,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) {
    // Branch (right-justified) - show change ID
    let branch_display = if opts.use_colors {
        format!(
            "{:>width$}",
            result.change_id.magenta(),
            width = widths.branch_width
        )
    } else {
        format!("{:>width$}", result.change_id, width = widths.branch_width)
    };

    // PR number (fixed width) - use SHA field for PR number
    let pr_ref = result.pr_reference();
    let pr_display = if opts.use_colors {
        format!("{:width$}", pr_ref.bright_black(), width = widths.sha_width)
    } else {
        format!("{:width$}", pr_ref, width = widths.sha_width)
    };

    // Emoji/Status indicator (left-aligned)
    let emoji = result.get_emoji(opts);
    let emoji_display = format!("{:<width$}", emoji, width = widths.emoji_width);

    // Repository path/slug
    let repo = &result.repo;
    let repo_slug = &repo.slug;
    let repo_display = format_repo_path_with_colors(&repo.path, repo_slug, opts.use_colors);

    // Final format: <change_id> <PR#> <emoji> <repo>
    println!("{branch_display} {pr_display} {emoji_display} {repo_display}");

    // Handle error display
    if let Some(error) = &result.error {
        let error_msg = if opts.use_colors {
            format!("  Error: {}", error.red())
        } else {
            format!("  Error: {error}")
        };
        println!("{error_msg}");
    }
}

/// Display multiple items using unified formatting
pub fn display_unified_results<T: UnifiedDisplay>(items: &[T], opts: &StatusOptions) {
    if items.is_empty() {
        return;
    }

    // Calculate alignment widths
    let widths = AlignmentWidths::calculate(items);

    // Display each item
    for item in items {
        display_unified_format(item, opts, &widths);
    }
}

/// Display multiple ReviewResult items with PR number information
pub fn display_review_results(results: &[ReviewResult], opts: &StatusOptions) {
    if results.is_empty() {
        return;
    }

    // Calculate alignment widths based on ReviewResult data
    let widths = AlignmentWidths::calculate(results);

    // Display each result using specialized function
    for result in results {
        display_review_result(result, opts, &widths);
    }
}

/// Display unified summary matching status format (clean/dirty/errors)
pub fn display_unified_summary(
    clean_count: usize,
    dirty_count: usize,
    error_count: usize,
    opts: &StatusOptions,
) {
    if clean_count == 0 && dirty_count == 0 && error_count == 0 {
        let msg = if opts.use_emoji {
            "🔍 No repositories found"
        } else {
            "No repositories found"
        };
        println!("\n{msg}");
        return;
    }

    let summary = if opts.use_emoji {
        format!("\n📊 {clean_count} clean, {dirty_count} dirty, {error_count} errors")
    } else {
        format!("\nSummary: {clean_count} clean, {dirty_count} dirty, {error_count} errors")
    };

    if opts.use_colors {
        println!(
            "\n📊 {} clean, {} dirty, {} errors",
            clean_count.to_string().green(),
            dirty_count.to_string().yellow(),
            error_count.to_string().red()
        );
    } else {
        println!("{summary}");
    }
}

/// Display a single clone result immediately (for streaming output like slam)
pub fn display_clone_result_immediate(result: &CloneResult) -> Result<()> {
    match &result.error {
        Some(err) => {
            println!(
                "⚠️  {} Failed: {}",
                result.repo_slug.red().bold(),
                err.red()
            );
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
    io::stdout().flush().context("Failed to flush stdout")?;
    Ok(())
}

/// Display a single checkout result immediately (for streaming output like slam)
pub fn display_checkout_result_immediate(result: &CheckoutResult) -> Result<()> {
    let opts = StatusOptions::default(); // Use default options for immediate display
    let widths = AlignmentWidths::calculate(std::slice::from_ref(result));

    display_unified_format(result, &opts, &widths);
    io::stdout().flush().context("Failed to flush stdout")?;
    Ok(())
}

/// Get current branch name quickly (no network calls, no status parsing)
fn get_current_branch_name_fast(repo: &crate::repo::Repo) -> String {
    use std::process::Command;

    Command::new("git")
        .args([
            "-C",
            &repo.path.to_string_lossy(),
            "branch",
            "--show-current",
        ])
        .output()
        .map(|output| {
            if output.status.success() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                "unknown".to_string()
            }
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Calculate alignment widths quickly using fast git commands (no expensive operations)
pub fn calculate_alignment_widths_fast(repos: &[crate::repo::Repo]) -> AlignmentWidths {
    use rayon::prelude::*;

    // Branch width: Fast git command, no network calls
    let branch_width = repos
        .par_iter()
        .map(|repo| get_current_branch_name_fast(repo).len())
        .max()
        .unwrap_or(7)
        .max(7); // Minimum readable width

    // SHA width: Always fixed
    let sha_width = 7;

    // Emoji width: Set to maximum possible width to handle all cases
    // Covers: 🟢 (2), ⬇️1 (3), ⬆️12 (4), ⚠️abc (5), 🔀2↑3↓ (6)
    let emoji_width = 6; // Maximum width for diverged case

    AlignmentWidths {
        branch_width,
        sha_width,
        emoji_width,
    }
}

/// Display a single status result immediately with pre-calculated alignment
pub fn display_status_result_immediate(
    result: &crate::git::RepoStatus,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) -> Result<()> {
    // Apply verbosity filtering (same logic as batch display)
    let should_display = match (&result.error, result.is_clean, opts.verbosity) {
        (Some(_), _, _) => true,                         // Always show errors
        (None, true, OutputVerbosity::Compact) => false, // Skip clean in compact
        (None, true, _) => true,                         // Show clean in other modes
        (None, false, _) => true,                        // Always show dirty
    };

    if should_display {
        // Use existing unified formatting with fixed widths
        display_unified_format(result, opts, widths);

        // Ensure immediate visibility
        io::stdout().flush().context("Failed to flush stdout")?;
    }

    Ok(())
}
