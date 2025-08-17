use crate::git::{RepoStatus, RemoteStatus, CheckoutResult, CheckoutAction, CloneResult, CloneAction};
use crate::config::OutputVerbosity;
use colored::*;
use std::io::{self, Write};
use std::path::Path;
use std::env;

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
        if let Some(_) = &self.error {
            if opts.use_emoji { "❌".to_string() } else { "ERROR".to_string() }
        } else if !self.is_clean {
            // File change status logic
            if self.changes.untracked > 0 {
                if opts.use_emoji { "❓".to_string() } else { "?".to_string() }
            } else if self.changes.modified > 0 {
                if opts.use_emoji { "📝".to_string() } else { "M".to_string() }
            } else if self.changes.added > 0 {
                if opts.use_emoji { "➕".to_string() } else { "A".to_string() }
            } else if self.changes.deleted > 0 {
                if opts.use_emoji { "❌".to_string() } else { "D".to_string() }
            } else if self.changes.staged > 0 {
                if opts.use_emoji { "🎯".to_string() } else { "S".to_string() }
            } else {
                if opts.use_emoji { "📝".to_string() } else { "M".to_string() }
            }
        } else {
            // Remote status logic for clean repos
            match &self.remote_status {
                RemoteStatus::UpToDate => if opts.use_emoji { "🟢".to_string() } else { "=".to_string() },
                RemoteStatus::Ahead(n) => if opts.use_emoji { format!("⬆️{}", n) } else { format!("↑{}", n) },
                RemoteStatus::Behind(n) => if opts.use_emoji { format!("⬇️{}", n) } else { format!("↓{}", n) },
                RemoteStatus::Diverged(ahead, behind) => if opts.use_emoji { format!("🔀{}↑{}↓", ahead, behind) } else { format!("±{}↑{}↓", ahead, behind) },
                RemoteStatus::NoRemote => if opts.use_emoji { "📍".to_string() } else { "~".to_string() },
                RemoteStatus::Error(e) => if opts.use_emoji { format!("⚠️{}", e.chars().take(3).collect::<String>()) } else { format!("!{}", e.chars().take(3).collect::<String>()) },
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
        if let Some(_) = &self.error {
            if opts.use_emoji { "❌".to_string() } else { "ERROR".to_string() }
        } else {
            match self.action {
                CheckoutAction::CheckedOutSynced => if opts.use_emoji { "📥".to_string() } else { "OK".to_string() },
                CheckoutAction::CreatedFromRemote => if opts.use_emoji { "✨".to_string() } else { "NEW".to_string() },
                CheckoutAction::Stashed => if opts.use_emoji { "📦".to_string() } else { "STASH".to_string() },
                CheckoutAction::HasUntracked => if opts.use_emoji { "⚠️".to_string() } else { "WARN".to_string() },
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
        if let Some(_) = &self.error {
            if opts.use_emoji { "❌".to_string() } else { "ERROR".to_string() }
        } else if !self.is_clean {
            // File change status logic
            if self.changes.untracked > 0 {
                if opts.use_emoji { "❓".to_string() } else { "?".to_string() }
            } else if self.changes.modified > 0 {
                if opts.use_emoji { "📝".to_string() } else { "M".to_string() }
            } else if self.changes.added > 0 {
                if opts.use_emoji { "➕".to_string() } else { "A".to_string() }
            } else if self.changes.deleted > 0 {
                if opts.use_emoji { "❌".to_string() } else { "D".to_string() }
            } else if self.changes.staged > 0 {
                if opts.use_emoji { "🎯".to_string() } else { "S".to_string() }
            } else {
                if opts.use_emoji { "📝".to_string() } else { "M".to_string() }
            }
        } else {
            // Remote status logic for clean repos
            match &self.remote_status {
                RemoteStatus::UpToDate => if opts.use_emoji { "🟢".to_string() } else { "=".to_string() },
                RemoteStatus::Ahead(n) => if opts.use_emoji { format!("⬆️{}", n) } else { format!("↑{}", n) },
                RemoteStatus::Behind(n) => if opts.use_emoji { format!("⬇️{}", n) } else { format!("↓{}", n) },
                RemoteStatus::Diverged(ahead, behind) => if opts.use_emoji { format!("🔀{}↑{}↓", ahead, behind) } else { format!("±{}↑{}↓", ahead, behind) },
                RemoteStatus::NoRemote => if opts.use_emoji { "📍".to_string() } else { "~".to_string() },
                RemoteStatus::Error(e) => if opts.use_emoji { format!("⚠️{}", e.chars().take(3).collect::<String>()) } else { format!("!{}", e.chars().take(3).collect::<String>()) },
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
        if let Some(_) = &self.error {
            if opts.use_emoji { "❌".to_string() } else { "ERROR".to_string() }
        } else {
            match self.action {
                CheckoutAction::CheckedOutSynced => if opts.use_emoji { "📥".to_string() } else { "OK".to_string() },
                CheckoutAction::CreatedFromRemote => if opts.use_emoji { "✨".to_string() } else { "NEW".to_string() },
                CheckoutAction::Stashed => if opts.use_emoji { "📦".to_string() } else { "STASH".to_string() },
                CheckoutAction::HasUntracked => if opts.use_emoji { "⚠️".to_string() } else { "WARN".to_string() },
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
        let branch_width = items.iter()
            .filter_map(|item| item.get_branch())
            .map(|branch| branch.len())
            .max()
            .unwrap_or(7) // "unknown".len() + padding
            .max(7); // Minimum width for readability

        let sha_width = 7; // Always 7 characters for SHA
        let emoji_width = 2; // Most emojis are 2 chars wide

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
    repo_path.file_name()
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
        format!("{:>width$}", branch.green(), width = widths.branch_width)
    } else {
        format!("{:>width$}", branch, width = widths.branch_width)
    };

    // Commit SHA (fixed width)
    let commit_display = item.get_commit_sha().unwrap_or("-------");
    let sha_display = format!("{:width$}", commit_display, width = widths.sha_width);

    // Emoji/Status indicator
    let emoji = item.get_emoji(opts);
    let emoji_display = format!("{:width$}", emoji, width = widths.emoji_width);

    // Repository path/slug
    let repo = item.get_repo();
    let repo_slug = repo.slug.as_ref().unwrap_or(&repo.name);
    let repo_display = format_repo_path_with_colors(
        &repo.path,
        repo_slug,
        opts.use_colors
    );

    // Final format: <branch> <sha> <emoji> <repo>
    println!("{} {} {} {}", branch_display, sha_display, emoji_display, repo_display);

    // Handle error display
    if let Some(error) = item.get_error() {
        let error_msg = if opts.use_colors {
            format!("  Error: {}", error.red())
        } else {
            format!("  Error: {}", error)
        };
        println!("{}", error_msg);
    }
}

/// Display multiple items using unified formatting
pub fn display_unified_results<T: UnifiedDisplay>(
    items: &[T],
    opts: &StatusOptions,
) {
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

/// Display status results with summary using unified formatting
pub fn display_status_results(results: Vec<RepoStatus>, opts: &StatusOptions) {
    let mut clean_count = 0;
    let mut dirty_count = 0;
    let mut error_count = 0;

    // Filter results based on verbosity (existing logic)
    let filtered_results: Vec<&RepoStatus> = results.iter()
        .filter(|result| {
            match (&result.error, result.is_clean, opts.verbosity) {
                (Some(_), _, _) => { error_count += 1; true }, // Always show errors
                (None, true, OutputVerbosity::Compact) => { clean_count += 1; false }, // Skip clean in compact
                (None, true, _) => { clean_count += 1; true }, // Show clean in other modes
                (None, false, _) => { dirty_count += 1; true }, // Always show dirty
            }
        })
        .collect();

    // Use unified display for filtered results
    display_unified_results(&filtered_results, opts);

    // Display summary (existing logic)
    display_summary(clean_count, dirty_count, error_count, opts);
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
    let opts = StatusOptions::default(); // Use default options for immediate display
    let widths = AlignmentWidths::calculate(std::slice::from_ref(result));

    display_unified_format(result, &opts, &widths);
    io::stdout().flush().expect("Failed to flush stdout");
}