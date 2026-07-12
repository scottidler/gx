use crate::config::OutputVerbosity;
use crate::create::{CreateAction, CreateResult};
use crate::git::{
    CheckoutAction, CheckoutResult, CloneAction, CloneResult, RemoteStatus, RepoStatus,
};
use crate::repo::Layout;
use crate::review::{ReviewAction, ReviewResult};
use colored::*;
use eyre::{Context, Result};
use std::io::{self, Write};
use std::path::Path;
use unicode_display_width::width as unicode_width;

/// Catppuccin Mocha palette roles, matching Scott's starship prompt
/// (`custom.path`/`custom.branch`) verbatim. Applied ONLY on the
/// `layout_view() == Some` (currently `gx status` only) render path - the
/// `None` path keeps its existing cyan/magenta styling untouched.
const LAYOUT_SLUG_RGB: (u8, u8, u8) = (203, 166, 247);
const LAYOUT_BRANCH_RGB: (u8, u8, u8) = (166, 227, 161);
const LAYOUT_MATCHED_LEAF_RGB: (u8, u8, u8) = (142, 116, 173);
const LAYOUT_DIVERGED_RGB: (u8, u8, u8) = (250, 179, 135);

/// Render classification for a repo-identity column, derived at render time
/// from structural `Layout` plus the worktree leaf name vs the checked-out
/// branch - both already in hand, no extra git calls. Pure and directly
/// unit-testable (no stdout), unlike the printed-output rendering it drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutView<'a> {
    /// Normal flat clone (also covers `Layout::Unknown` and a `Bare` repo
    /// whose leaf name could not be determined, e.g. non-UTF-8): render
    /// exactly as before this feature.
    Flat,
    /// A bare-container worktree whose directory leaf matches the checked-out
    /// branch. The branch column is suppressed - the leaf already says it.
    WorktreeMatched { leaf: &'a str },
    /// A bare-container worktree whose directory leaf differs from the
    /// checked-out branch (including detached HEAD, which is always
    /// `HEAD@<sha>` per `git.rs`, never `None`). The branch column is shown.
    WorktreeDiverged { leaf: &'a str },
}

/// Classify how a status row's repo-identity column should render. Pure
/// function, no stdout - the alignment bug's earlier test could not bite
/// because it only validated printed output against the width crate.
pub fn classify_view<'a>(
    layout: Layout,
    leaf: Option<&'a str>,
    branch: Option<&str>,
) -> LayoutView<'a> {
    match (layout, leaf, branch) {
        (Layout::Bare, Some(l), Some(b)) if l == b => LayoutView::WorktreeMatched { leaf: l },
        (Layout::Bare, Some(l), _) => LayoutView::WorktreeDiverged { leaf: l },
        _ => LayoutView::Flat,
    }
}

/// Calculate display width for emoji strings using Unicode Standard width calculation
/// This uses unicode-display-width which provides consistent results across environments
pub fn calculate_display_width(s: &str) -> usize {
    unicode_width(s) as usize
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

    /// Opt-in seam for layout-aware rendering. `None` (the default) means
    /// this verb does not participate - `display_unified_format` takes the
    /// existing rendering path, byte-identical to before this feature. Only
    /// `RepoStatus` overrides this: `get_branch()` is a real checked-out
    /// branch there, whereas checkout/create/review overload it with a
    /// `change_id` that would misclassify against the leaf.
    fn layout_view(&self) -> Option<LayoutView<'_>> {
        None
    }
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
                RemoteStatus::Ahead(n) => format!("↑{n}"),
                RemoteStatus::Behind(n) => format!("↓{n}"),
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
                        format!("🚨 {}", e.chars().take(3).collect::<String>())
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

    fn layout_view(&self) -> Option<LayoutView<'_>> {
        Some(classify_view(
            self.repo.layout,
            self.repo.path.file_name().and_then(|n| n.to_str()),
            self.branch.as_deref(),
        ))
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
                        "🚨".to_string()
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
                RemoteStatus::Ahead(n) => format!("↑{n}"),
                RemoteStatus::Behind(n) => format!("↓{n}"),
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
                        format!("🚨 {}", e.chars().take(3).collect::<String>())
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
                        "🚨".to_string()
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
                    // Check if there were actual changes in this repo
                    let has_changes = self
                        .substitution_stats
                        .as_ref()
                        .map(|s| s.files_changed > 0)
                        .unwrap_or(false)
                        || !self.files_affected.is_empty();

                    if opts.use_emoji {
                        if has_changes {
                            "👀".to_string() // Would change
                        } else {
                            "➖".to_string() // No changes (skipped)
                        }
                    } else if has_changes {
                        "CHANGE".to_string()
                    } else {
                        "SKIP".to_string()
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
                    // Check if there were actual changes in this repo
                    let has_changes = self
                        .substitution_stats
                        .as_ref()
                        .map(|s| s.files_changed > 0)
                        .unwrap_or(false)
                        || !self.files_affected.is_empty();

                    if opts.use_emoji {
                        if has_changes {
                            "👀".to_string() // Would change
                        } else {
                            "➖".to_string() // No changes (skipped)
                        }
                    } else if has_changes {
                        "CHANGE".to_string()
                    } else {
                        "SKIP".to_string()
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

/// Format the repo-identity column for the layout-aware (`Some(view)`)
/// render path: connector glyph + leaf glued onto the slug, Catppuccin
/// palette applied only here. Pure, unit-testable independent of stdout.
fn format_layout_identity(repo_slug: &str, view: &LayoutView<'_>, use_colors: bool) -> String {
    let colored_slug = || -> String {
        let (r, g, b) = LAYOUT_SLUG_RGB;
        repo_slug.truecolor(r, g, b).bold().to_string()
    };

    match view {
        LayoutView::Flat => {
            if use_colors {
                colored_slug()
            } else {
                repo_slug.to_string()
            }
        }
        LayoutView::WorktreeMatched { leaf } => {
            if use_colors {
                let (cr, cg, cb) = LAYOUT_BRANCH_RGB;
                let (lr, lg, lb) = LAYOUT_MATCHED_LEAF_RGB;
                format!(
                    "{}{}{}",
                    colored_slug(),
                    "\u{2261}".truecolor(cr, cg, cb),
                    leaf.truecolor(lr, lg, lb)
                )
            } else {
                format!("{repo_slug}\u{2261}{leaf}")
            }
        }
        LayoutView::WorktreeDiverged { leaf } => {
            if use_colors {
                let (dr, dg, db) = LAYOUT_DIVERGED_RGB;
                format!(
                    "{}{}{}",
                    colored_slug(),
                    "\u{2248}".truecolor(dr, dg, db),
                    leaf.truecolor(dr, dg, db)
                )
            } else {
                format!("{repo_slug}\u{2248}{leaf}")
            }
        }
    }
}

/// Format the branch column for the layout-aware (`Some(view)`) render path:
/// blank for a matched worktree (the leaf already carries the signal via the
/// `\u{2261}` glue), the real branch (bold Catppuccin green) otherwise. Width
/// stays on the plain string (`width`), never the colored/glued form, so
/// `AlignmentWidths` needs no change regardless of how the glyphs measure.
fn format_layout_branch(
    branch: &str,
    view: &LayoutView<'_>,
    use_colors: bool,
    width: usize,
) -> String {
    match view {
        LayoutView::WorktreeMatched { .. } => format!("{:>width$}", "", width = width),
        LayoutView::Flat | LayoutView::WorktreeDiverged { .. } => {
            if use_colors {
                let (r, g, b) = LAYOUT_BRANCH_RGB;
                format!(
                    "{:>width$}",
                    branch.truecolor(r, g, b).bold(),
                    width = width
                )
            } else {
                format!("{:>width$}", branch, width = width)
            }
        }
    }
}

/// Render one item's status/result line, pure (no I/O) so it is directly
/// unit-testable. `display_unified_format` prints exactly this string.
///
/// `layout_view() == None` (checkout/create/review, and any future verb that
/// does not opt in) takes the original rendering path, byte-identical to
/// before this feature. `Some(view)` (currently `RepoStatus` only) takes the
/// Catppuccin/connector-glyph path instead.
fn render_unified_line<T: UnifiedDisplay>(
    item: &T,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) -> String {
    let branch = item.get_branch().unwrap_or("unknown");

    // Commit SHA (fixed width) - identical on both paths.
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

    // Emoji/Status indicator (left-aligned) - identical on both paths.
    let emoji = item.get_emoji(opts);
    let emoji_display = pad_to_width(&emoji, widths.emoji_width);

    let repo = item.get_repo();

    match item.layout_view() {
        None => {
            let branch_display = if opts.use_colors {
                format!("{:>width$}", branch.magenta(), width = widths.branch_width)
            } else {
                format!("{:>width$}", branch, width = widths.branch_width)
            };
            let repo_display =
                format_repo_path_with_colors(&repo.path, &repo.slug, opts.use_colors);
            format!("{branch_display} {sha_display} {emoji_display} {repo_display}")
        }
        Some(view) => {
            let branch_display =
                format_layout_branch(branch, &view, opts.use_colors, widths.branch_width);
            let repo_display = format_layout_identity(&repo.slug, &view, opts.use_colors);
            format!("{branch_display} {sha_display} {emoji_display} {repo_display}")
        }
    }
}

/// Display a single item using unified formatting
pub fn display_unified_format<T: UnifiedDisplay>(
    item: &T,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) {
    println!("{}", render_unified_line(item, opts, widths));

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
/// Renders a single `ReviewResult` line (plus an optional error line) to a
/// String. Review has its own layout - `<change_id> <PR#> <emoji> <repo>` - and
/// does NOT flow through `render_unified_line`; keeping this pure lets the
/// byte-identical regression test bite on review's real renderer.
fn render_review_line(
    result: &ReviewResult,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) -> String {
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
    let mut out = format!("{branch_display} {pr_display} {emoji_display} {repo_display}");

    // Handle error display
    if let Some(error) = &result.error {
        let error_msg = if opts.use_colors {
            format!("  Error: {}", error.red())
        } else {
            format!("  Error: {error}")
        };
        out.push('\n');
        out.push_str(&error_msg);
    }

    out
}

pub fn display_review_result(
    result: &ReviewResult,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) {
    println!("{}", render_review_line(result, opts, widths));
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
                "🚨  {} Failed: {}",
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
        // Ahead/behind patterns - bare width-1 arrows (crate width == terminal width)
        "↑1",
        "↑99",
        "↑999",
        "↑9999", // Up to 4 digits
        "↓1",
        "↓99",
        "↓999",
        "↓9999", // Up to 4 digits
        // Diverged patterns (7-11 width) - most complex, now with space
        "🔀 1↑1↓",
        "🔀 99↑99↓",
        "🔀 999↑999↓", // Up to 3 digits each
        // Error patterns (6-7 width)
        "🚨 git",
        "🚨 tim",
        "🚨 net",
        "🚨 abc",
        // Checkout patterns (2 width)
        "📥",
        "✨",
        "📦",
        "🚨",
        // Create patterns (2 width)
        "👀",
        "➖",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{RemoteStatus, RepoStatus, StatusChanges};
    use crate::repo::{Layout, Repo};
    use std::path::PathBuf;
    use std::sync::Mutex;

    // `colored`'s truecolor output falls back to the nearest 4-bit ANSI color
    // unless `COLORTERM=truecolor`/`24bit`, and its "should colorize" state
    // is a process-global - both need serializing across tests that touch
    // them, same pattern as the platform-path env tests (rust.md).
    static COLOR_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_truecolor_forced<T>(f: impl FnOnce() -> T) -> T {
        let guard = COLOR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("COLORTERM").ok();
        unsafe { std::env::set_var("COLORTERM", "truecolor") };
        colored::control::set_override(true);

        let result = f();

        colored::control::unset_override();
        match prior {
            Some(v) => unsafe { std::env::set_var("COLORTERM", v) },
            None => unsafe { std::env::remove_var("COLORTERM") },
        }
        drop(guard);
        result
    }

    fn ansi_fg(bold: bool, rgb: (u8, u8, u8)) -> String {
        let (r, g, b) = rgb;
        if bold {
            format!("\x1B[1;38;2;{r};{g};{b}m")
        } else {
            format!("\x1B[38;2;{r};{g};{b}m")
        }
    }

    fn bare_repo_status(leaf: &str, branch: Option<&str>) -> RepoStatus {
        RepoStatus {
            repo: Repo {
                path: PathBuf::from(format!("/repos/tatari-tv/clyde/{leaf}")),
                name: "clyde".to_string(),
                slug: "tatari-tv/clyde".to_string(),
                layout: Layout::Bare,
            },
            branch: branch.map(str::to_string),
            commit_sha: Some("a1b2c3d".to_string()),
            is_clean: true,
            changes: StatusChanges::default(),
            remote_status: RemoteStatus::UpToDate,
            error: None,
        }
    }

    fn flat_repo_status(branch: &str) -> RepoStatus {
        RepoStatus {
            repo: Repo {
                path: PathBuf::from("/repos/scottidler/otto"),
                name: "otto".to_string(),
                slug: "scottidler/otto".to_string(),
                layout: Layout::Flat,
            },
            branch: Some(branch.to_string()),
            commit_sha: Some("e4f5a6b".to_string()),
            is_clean: true,
            changes: StatusChanges::default(),
            remote_status: RemoteStatus::UpToDate,
            error: None,
        }
    }

    fn checkout_result_fixture() -> CheckoutResult {
        CheckoutResult {
            repo: Repo::from_slug("scottidler/otto".to_string()),
            branch_name: "feature-x".to_string(),
            commit_sha: Some("e4f5a6b".to_string()),
            action: CheckoutAction::CheckedOutSynced,
            error: None,
        }
    }

    fn create_result_fixture() -> CreateResult {
        CreateResult {
            repo: Repo::from_slug("scottidler/otto".to_string()),
            change_id: "GX-1234".to_string(),
            action: CreateAction::Committed,
            files_affected: Vec::new(),
            substitution_stats: None,
            pr_number: None,
            pr_url: None,
            original_branch: None,
            base_sha: None,
            error: None,
        }
    }

    fn review_result_fixture() -> ReviewResult {
        ReviewResult {
            repo: Repo::from_slug("scottidler/otto".to_string()),
            change_id: "GX-1234".to_string(),
            pr_number: None,
            action: ReviewAction::Listed,
            error: None,
        }
    }

    // ---- classify_view: pure classifier, all states + edges ----

    #[test]
    fn classify_view_matched_when_bare_leaf_equals_branch() {
        assert_eq!(
            classify_view(Layout::Bare, Some("main"), Some("main")),
            LayoutView::WorktreeMatched { leaf: "main" }
        );
    }

    #[test]
    fn classify_view_diverged_when_bare_leaf_differs_from_branch() {
        assert_eq!(
            classify_view(Layout::Bare, Some("main"), Some("feature-x")),
            LayoutView::WorktreeDiverged { leaf: "main" }
        );
    }

    #[test]
    fn classify_view_diverged_on_detached_head() {
        // git.rs:190 always returns Some("HEAD@<sha>"), never None, for a
        // detached HEAD.
        assert_eq!(
            classify_view(Layout::Bare, Some("main"), Some("HEAD@abc1234")),
            LayoutView::WorktreeDiverged { leaf: "main" }
        );
    }

    #[test]
    fn classify_view_flat_when_bare_leaf_is_none() {
        // A non-UTF-8 leaf reaches this function exactly as `leaf = None`
        // (`file_name().and_then(to_str)` yields `None` upstream) - safe
        // degrade to plain rendering.
        assert_eq!(
            classify_view(Layout::Bare, None, Some("main")),
            LayoutView::Flat
        );
        assert_eq!(classify_view(Layout::Bare, None, None), LayoutView::Flat);
    }

    #[test]
    fn classify_view_flat_for_flat_layout_regardless_of_leaf_branch() {
        assert_eq!(
            classify_view(Layout::Flat, Some("main"), Some("main")),
            LayoutView::Flat
        );
        assert_eq!(
            classify_view(Layout::Flat, Some("x"), Some("y")),
            LayoutView::Flat
        );
    }

    #[test]
    fn classify_view_flat_for_unknown_layout() {
        assert_eq!(
            classify_view(Layout::Unknown, Some("main"), Some("main")),
            LayoutView::Flat
        );
    }

    // ---- render: status rows, use_colors=false (glyph carries the signal) ----

    #[test]
    fn status_matched_no_color_glues_leaf_and_blanks_branch() {
        let result = bare_repo_status("main", Some("main"));
        let opts = StatusOptions {
            verbosity: OutputVerbosity::Summary,
            use_emoji: true,
            use_colors: false,
        };
        let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
        let line = render_unified_line(&result, &opts, &widths);

        assert!(!line.contains('\u{1b}'), "zero ANSI expected: {line}");
        assert!(line.contains("tatari-tv/clyde\u{2261}main"));
        let branch_field = line
            .get(..widths.branch_width)
            .expect("branch column is ascii-only in this fixture");
        assert_eq!(branch_field.trim(), "", "matched branch column is blank");
    }

    #[test]
    fn status_diverged_no_color_glues_leaf_and_shows_branch() {
        let result = bare_repo_status("main", Some("feature-x"));
        let opts = StatusOptions {
            verbosity: OutputVerbosity::Summary,
            use_emoji: true,
            use_colors: false,
        };
        let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
        let line = render_unified_line(&result, &opts, &widths);

        assert!(!line.contains('\u{1b}'), "zero ANSI expected: {line}");
        assert!(line.contains("tatari-tv/clyde\u{2248}main"));
        assert!(
            line.contains("feature-x"),
            "diverged branch is shown: {line}"
        );
    }

    #[test]
    fn status_diverged_detached_head_shows_head_at_sha() {
        let result = bare_repo_status("main", Some("HEAD@abc1234"));
        let opts = StatusOptions {
            verbosity: OutputVerbosity::Summary,
            use_emoji: true,
            use_colors: false,
        };
        let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
        let line = render_unified_line(&result, &opts, &widths);

        assert!(line.contains("tatari-tv/clyde\u{2248}main"));
        assert!(line.contains("HEAD@abc1234"));
    }

    #[test]
    fn status_flat_no_color_has_neither_glyph() {
        let result = flat_repo_status("main");
        let opts = StatusOptions {
            verbosity: OutputVerbosity::Summary,
            use_emoji: true,
            use_colors: false,
        };
        let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
        let line = render_unified_line(&result, &opts, &widths);

        assert!(!line.contains('\u{2261}') && !line.contains('\u{2248}'));
        assert!(line.contains("scottidler/otto"));
        assert!(line.contains("main"));
    }

    // ---- render: status rows, use_colors=true (Catppuccin roles) ----

    #[test]
    fn status_matched_color_applies_slug_connector_and_leaf_roles() {
        with_truecolor_forced(|| {
            let result = bare_repo_status("main", Some("main"));
            let opts = StatusOptions {
                verbosity: OutputVerbosity::Summary,
                use_emoji: true,
                use_colors: true,
            };
            let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
            let line = render_unified_line(&result, &opts, &widths);

            assert!(
                line.contains(&ansi_fg(true, LAYOUT_SLUG_RGB)),
                "slug is bold purple: {line}"
            );
            assert!(
                line.contains(&ansi_fg(false, LAYOUT_BRANCH_RGB)),
                "connector is non-bold green: {line}"
            );
            assert!(
                line.contains(&ansi_fg(false, LAYOUT_MATCHED_LEAF_RGB)),
                "matched leaf is non-bold dull-mauve: {line}"
            );
            assert!(line.contains('\u{2261}'));
        });
    }

    #[test]
    fn status_diverged_color_applies_slug_branch_and_peach_roles() {
        with_truecolor_forced(|| {
            let result = bare_repo_status("main", Some("feature-x"));
            let opts = StatusOptions {
                verbosity: OutputVerbosity::Summary,
                use_emoji: true,
                use_colors: true,
            };
            let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
            let line = render_unified_line(&result, &opts, &widths);

            assert!(
                line.contains(&ansi_fg(true, LAYOUT_SLUG_RGB)),
                "slug is bold purple: {line}"
            );
            assert!(
                line.contains(&ansi_fg(true, LAYOUT_BRANCH_RGB)),
                "trailing branch is bold green: {line}"
            );
            assert!(
                line.contains(&ansi_fg(false, LAYOUT_DIVERGED_RGB)),
                "connector + diverged leaf are non-bold peach: {line}"
            );
            assert!(line.contains('\u{2248}'));
        });
    }

    // ---- render: layout_view() seam - checkout/create/review unaffected ----

    #[test]
    fn review_result_layout_view_defaults_to_none() {
        // ReviewResult never overrides the seam; the default trait method
        // must still say "not participating".
        let result = ReviewResult {
            repo: Repo::from_slug("scottidler/otto".to_string()),
            change_id: "GX-1234".to_string(),
            pr_number: None,
            action: ReviewAction::Listed,
            error: None,
        };
        assert!(UnifiedDisplay::layout_view(&result).is_none());
    }

    #[test]
    fn checkout_result_render_byte_identical_to_pre_change_formula() {
        let result = checkout_result_fixture();
        assert!(UnifiedDisplay::layout_view(&result).is_none());

        for use_colors in [true, false] {
            let opts = StatusOptions {
                verbosity: OutputVerbosity::Summary,
                use_emoji: true,
                use_colors,
            };
            let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
            let rendered = render_unified_line(&result, &opts, &widths);
            let expected = pre_change_formula(&result, &opts, &widths);
            assert_eq!(rendered, expected);
        }
    }

    #[test]
    fn create_result_render_byte_identical_to_pre_change_formula() {
        let result = create_result_fixture();
        assert!(UnifiedDisplay::layout_view(&result).is_none());

        for use_colors in [true, false] {
            let opts = StatusOptions {
                verbosity: OutputVerbosity::Summary,
                use_emoji: true,
                use_colors,
            };
            let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
            let rendered = render_unified_line(&result, &opts, &widths);
            let expected = pre_change_formula(&result, &opts, &widths);
            assert_eq!(rendered, expected);
        }
    }

    #[test]
    fn review_result_render_byte_identical_to_pre_change_formula() {
        // Review renders through its own `render_review_line`, NOT the modified
        // `render_unified_line`, so it never carries the layout marker. This
        // recomputes review's `<change_id> <PR#> <emoji> <repo>` layout by hand
        // and asserts the real renderer matches it, in both color modes - a
        // future format edit fails here rather than silently changing output.
        let result = review_result_fixture();
        assert!(UnifiedDisplay::layout_view(&result).is_none());

        for use_colors in [true, false] {
            let opts = StatusOptions {
                verbosity: OutputVerbosity::Summary,
                use_emoji: true,
                use_colors,
            };
            let widths = AlignmentWidths::calculate(std::slice::from_ref(&result));
            let rendered = render_review_line(&result, &opts, &widths);
            let expected = pre_change_review_formula(&result, &opts, &widths);
            assert_eq!(rendered, expected);
        }
    }

    /// Recomputes review's `<change_id> <PR#> <emoji> <repo>` layout by hand
    /// (independent of `render_review_line`), so a regression in review's
    /// renderer fails a test rather than silently changing its output.
    fn pre_change_review_formula(
        result: &ReviewResult,
        opts: &StatusOptions,
        widths: &AlignmentWidths,
    ) -> String {
        let branch_display = if opts.use_colors {
            format!(
                "{:>width$}",
                result.change_id.magenta(),
                width = widths.branch_width
            )
        } else {
            format!("{:>width$}", result.change_id, width = widths.branch_width)
        };
        let pr_ref = result.pr_reference();
        let pr_display = if opts.use_colors {
            format!("{:width$}", pr_ref.bright_black(), width = widths.sha_width)
        } else {
            format!("{:width$}", pr_ref, width = widths.sha_width)
        };
        let emoji = result.get_emoji(opts);
        let emoji_display = pad_to_width(&emoji, widths.emoji_width);
        let repo = &result.repo;
        let repo_display = format_repo_path_with_colors(&repo.path, &repo.slug, opts.use_colors);
        let mut out = format!("{branch_display} {pr_display} {emoji_display} {repo_display}");
        if let Some(error) = &result.error {
            let error_msg = if opts.use_colors {
                format!("  Error: {}", error.red())
            } else {
                format!("  Error: {error}")
            };
            out.push('\n');
            out.push_str(&error_msg);
        }
        out
    }

    /// Recomputes the pre-Phase-2 `None`-path rendering formula by hand, so a
    /// regression in the branch-on-`layout_view()` refactor fails a test
    /// rather than silently changing checkout/create/review output.
    fn pre_change_formula<T: UnifiedDisplay>(
        item: &T,
        opts: &StatusOptions,
        widths: &AlignmentWidths,
    ) -> String {
        let branch = item.get_branch().unwrap_or("unknown");
        let branch_display = if opts.use_colors {
            format!("{:>width$}", branch.magenta(), width = widths.branch_width)
        } else {
            format!("{:>width$}", branch, width = widths.branch_width)
        };
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
        let emoji = item.get_emoji(opts);
        let emoji_display = pad_to_width(&emoji, widths.emoji_width);
        let repo = item.get_repo();
        let repo_display = format_repo_path_with_colors(&repo.path, &repo.slug, opts.use_colors);
        format!("{branch_display} {sha_display} {emoji_display} {repo_display}")
    }
}
