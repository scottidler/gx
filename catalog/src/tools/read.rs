//! `read(slug, path)` -> file contents, path CLAMPED inside the repo dir. Reuses
//! `local::file::read_utf8_or_skip` (fail loud on non-utf8, never empty-as-
//! success) and `local::file::validate_new_file_path` (the canonicalize-compare
//! clamp that rejects `..` and symlink escapes). An oversized file is rejected
//! loudly UNLESS a bounded line range is requested.

use eyre::{bail, Context, Result};
use log::debug;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::{Path, PathBuf};

use local::file::{read_utf8_or_skip, validate_new_file_path};

use super::{scope_sql, truncate_bytes, Bounds};

/// The contents `read` returns. `line_start`/`line_end` are present (1-based,
/// inclusive) when a bounded range was requested. `truncated` is set when the
/// byte cap cut the returned content.
#[derive(Debug, Clone, Serialize)]
pub struct ReadResult {
    pub slug: String,
    pub path: String,
    pub content: String,
    pub truncated: bool,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
}

/// Read `rel_path` inside the repo identified by `slug`. `line_range`, when
/// `Some((start, end))`, restricts the result to 1-based inclusive lines
/// `start..=end` (and permits reading a file that would otherwise be rejected
/// for exceeding `bounds.max_bytes`).
pub fn read(
    conn: &Connection,
    catalog_root: &Path,
    slug: &str,
    rel_path: &str,
    line_range: Option<(usize, usize)>,
    bounds: &Bounds,
) -> Result<ReadResult> {
    debug!(
        "read: catalog_root={} slug={slug} rel_path={rel_path} line_range={line_range:?}",
        catalog_root.display()
    );

    // The repo must be in the catalog AND under the catalog root (scope clamp).
    let ceiling = catalog_root.canonicalize().with_context(|| {
        format!(
            "catalog.root {} does not exist or cannot be resolved",
            catalog_root.display()
        )
    })?;
    let (root_str, prefix) = scope_sql(&ceiling);
    let repo_path: Option<String> = conn
        .query_row(
            "SELECT path FROM repos WHERE slug = ?1 AND (path = ?2 OR path LIKE ?3)",
            params![slug, root_str, prefix],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .context("failed to look up repo path for read")?;

    let Some(repo_path) = repo_path else {
        bail!("read: no repo `{slug}` in the catalog under the catalog root (walk it first, or check the slug)");
    };
    let repo_dir = PathBuf::from(repo_path);

    // Clamp the file path inside the repo dir: rejects absolute paths, `..`,
    // `.git`, and symlink escapes (the same canonicalize-compare pattern used
    // for `gx add`).
    let full = validate_new_file_path(&repo_dir, rel_path)
        .with_context(|| format!("read: path `{rel_path}` is not inside repo `{slug}`"))?;

    // Oversized guard: a whole-file read over the byte cap is refused loudly
    // unless the caller asked for a bounded line range.
    if line_range.is_none() {
        let meta = std::fs::metadata(&full)
            .with_context(|| format!("read: cannot stat {}", full.display()))?;
        if meta.len() as usize > bounds.max_bytes {
            bail!(
                "read: {} is {} bytes, over the {}-byte cap; request a bounded line range",
                full.display(),
                meta.len(),
                bounds.max_bytes
            );
        }
    }

    // Fail loud on non-utf8 (`read_utf8_or_skip` yields None): never empty-as-
    // success.
    let Some(content) = read_utf8_or_skip(&full)? else {
        bail!(
            "read: {} is not valid UTF-8 (binary file); cannot read as text",
            full.display()
        );
    };

    let (content, line_start, line_end) = match line_range {
        Some((start, end)) => slice_lines(&content, start, end),
        None => (content, None, None),
    };

    let (content, truncated) = truncate_bytes(&content, bounds.max_bytes);
    debug!(
        "read: slug={slug} rel_path={rel_path} bytes={} truncated={truncated}",
        content.len()
    );
    Ok(ReadResult {
        slug: slug.to_string(),
        path: rel_path.to_string(),
        content,
        truncated,
        line_start,
        line_end,
    })
}

/// Extract 1-based inclusive lines `start..=end` from `text`. `start` is floored
/// to 1; `end` is clamped to the last line. Returns the joined slice plus the
/// effective (start, end) actually returned.
fn slice_lines(text: &str, start: usize, end: usize) -> (String, Option<usize>, Option<usize>) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return (String::new(), Some(0), Some(0));
    }
    let start = start.max(1);
    let end = end.min(lines.len());
    if start > end {
        return (String::new(), Some(start), Some(end));
    }
    let slice = lines[start - 1..end].join("\n");
    (slice, Some(start), Some(end))
}
