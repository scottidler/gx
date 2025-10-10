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
use string_width::string_width;

/// Detect the current terminal type for width calculation adjustments
fn get_terminal_type() -> String {
    // Check various environment variables to determine terminal
    if let Ok(term_program) = env::var("TERM_PROGRAM") {
        return term_program.to_lowercase();
    }

    if let Ok(terminal_emulator) = env::var("TERMINAL_EMULATOR") {
        return terminal_emulator.to_lowercase();
    }

    if let Ok(term) = env::var("TERM") {
        return term.to_lowercase();
    }

    // Check for specific terminal indicators
    if env::var("ITERM_SESSION_ID").is_ok() {
        return "iterm2".to_string();
    }

    if env::var("GNOME_TERMINAL_SERVICE").is_ok() {
        return "gnome-terminal".to_string();
    }

    if env::var("KONSOLE_VERSION").is_ok() {
        return "konsole".to_string();
    }

    if env::var("ALACRITTY_SOCKET").is_ok() {
        return "alacritty".to_string();
    }

    "unknown".to_string()
}

/// Calculate display width for emoji strings with terminal-aware adjustments
/// This uses the string_width library but applies terminal-specific corrections
pub fn calculate_display_width(s: &str) -> usize {
    let base_width = string_width(s);
    let terminal_type = get_terminal_type();

    // Apply terminal-specific adjustments for emoji rendering differences
    match terminal_type.as_str() {
        "vscode" => {
            // VSCode integrated terminal has specific emoji rendering behavior
            // Based on empirical testing: all these patterns appear shifted right (too much space)
            if s.starts_with("⬆️ ") || s.starts_with("⬇️ ") || s.starts_with("⚠️ ") {
                // All these patterns need -1 adjustment (they appear shifted right)
                return base_width - 1;
            }
            base_width
        }

        // Many terminals have issues with arrow emoji + variation selector width calculations
        term if term.contains("xterm")
            || term.contains("gnome")
            || term.contains("konsole")
            || term.contains("alacritty")
            || term.contains("kitty") =>
        {
            // These terminals render these patterns with too much space (shifted right)
            if s.starts_with("⬆️ ") || s.starts_with("⬇️ ") || s.starts_with("⚠️ ") {
                return base_width - 1;
            }
            base_width
        }

        "iterm2" => {
            // iTerm2 has similar emoji rendering issues
            if s.starts_with("⬆️ ") || s.starts_with("⬇️ ") || s.starts_with("⚠️ ") {
                return base_width - 1;
            }
            base_width
        }

        _ => {
            // For unknown terminals, use conservative approach
            // Most terminals seem to have the same issue with these patterns
            if s.starts_with("⬆️ ") || s.starts_with("⬇️ ") || s.starts_with("⚠️ ") {
                return base_width - 1;
            }
            base_width
        }
    }
}

/// Pad a string to a specific display width, handling emoji properly
pub fn pad_to_width(s: &str, target_width: usize) -> String {
    let current_width = calculate_display_width(s);
    if current_width >= target_width {
        s.to_string()
    } else {
        let padding_needed = target_width - current_width;
        format!("{}{}", s, " ".repeat(padding_needed))
    }
}

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
                        format!("⬆️ {n}")
                    } else {
                        format!("↑{n}")
                    }
                }
                RemoteStatus::Behind(n) => {
                    if opts.use_emoji {
                        format!("⬇️ {n}")
                    } else {
                        format!("↓{n}")
                    }
                }
                RemoteStatus::Diverged(ahead, behind) => {
                    if opts.use_emoji {
                        format!("🔀 {ahead}↑{behind}↓")
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
                RemoteStatus::NoUpstream => {
                    if opts.use_emoji {
                        "📍".to_string()
                    } else {
                        "~".to_string()
                    }
                }
                RemoteStatus::DetachedHead => {
                    if opts.use_emoji {
                        "📍".to_string()
                    } else {
                        "~".to_string()
                    }
                }
                RemoteStatus::Error(e) => {
                    if opts.use_emoji {
                        format!("⚠️ {}", e.chars().take(3).collect::<String>())
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
                        format!("⬆️ {n}")
                    } else {
                        format!("↑{n}")
                    }
                }
                RemoteStatus::Behind(n) => {
                    if opts.use_emoji {
                        format!("⬇️ {n}")
                    } else {
                        format!("↓{n}")
                    }
                }
                RemoteStatus::Diverged(ahead, behind) => {
                    if opts.use_emoji {
                        format!("🔀 {ahead}↑{behind}↓")
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
                RemoteStatus::NoUpstream => {
                    if opts.use_emoji {
                        "📍".to_string()
                    } else {
                        "~".to_string()
                    }
                }
                RemoteStatus::DetachedHead => {
                    if opts.use_emoji {
                        "📍".to_string()
                    } else {
                        "~".to_string()
                    }
                }
                RemoteStatus::Error(e) => {
                    if opts.use_emoji {
                        format!("⚠️ {}", e.chars().take(3).collect::<String>())
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
        // We need to account for the fact that some emojis have zero width in terminals
        let emoji_width = items
            .iter()
            .map(|item| {
                let opts = StatusOptions::default();
                let emoji = item.get_emoji(&opts);
                // Use a more accurate width calculation for emojis
                calculate_display_width(&emoji)
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

/// Format repository path with separate colors for path and repo slug
fn format_repo_path_with_colors(_repo_path: &Path, repo_slug: &str, use_colors: bool) -> String {
    // Always use the repo slug for consistent display
    let display_path = repo_slug.to_string();

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
    let emoji_display = pad_to_width(&emoji, widths.emoji_width);

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
    let emoji_display = pad_to_width(&emoji, widths.emoji_width);

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

    // Emoji width: Calculate based on all possible emoji patterns that could appear
    // This is fast because we're not doing git operations, just measuring known emoji patterns
    let emoji_width = calculate_max_possible_emoji_width();

    AlignmentWidths {
        branch_width,
        sha_width,
        emoji_width,
    }
}

/// Calculate the maximum possible emoji width for all patterns that could appear in status output
/// This is fast because it only measures a small set of known patterns, no git operations
fn calculate_max_possible_emoji_width() -> usize {
    // All possible emoji patterns that could appear in gx status output
    let possible_patterns = vec![
        // Simple status emojis (2 width each)
        "🟢",
        "📝",
        "❓",
        "❌",
        "🎯",
        "➕",
        "📍",
        // Ahead/behind patterns (4-7 width) - now with spaces
        "⬆️ 1",
        "⬆️ 99",
        "⬆️ 999",
        "⬆️ 9999", // Up to 4 digits
        "⬇️ 1",
        "⬇️ 99",
        "⬇️ 999",
        "⬇️ 9999", // Up to 4 digits
        // Diverged patterns (7-11 width) - most complex, now with space
        "🔀 1↑1↓",
        "🔀 99↑99↓",
        "🔀 999↑999↓", // Up to 3 digits each
        // Error patterns (6-7 width)
        "⚠️ git",
        "⚠️ tim",
        "⚠️ net",
        "⚠️ abc",
        // Checkout patterns (2 width)
        "📥",
        "✨",
        "📦",
        "⚠️",
        // Create patterns (2 width)
        "👁️",
        "💾",
        // Review patterns (2 width)
        "📋",
        "✅",
        "🧹",
    ];

    // Find the maximum width among all possible patterns
    possible_patterns
        .iter()
        .map(|pattern| calculate_display_width(pattern))
        .max()
        .unwrap_or(2) // Fallback to minimum
        .max(2) // Ensure at least 2 for readability
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
