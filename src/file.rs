use crate::diff;
use crate::git;
use eyre::{Context, Result};

use log::{debug, trace, warn};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// The set of files gx is allowed to mutate in a repository.
///
/// Candidates come from git's index (`git ls-files --stage`), i.e. **tracked
/// files only** (see design Q6). This makes `.git/` contents, gitignored files,
/// untracked files, and submodule internals structurally unreachable. User
/// glob patterns are matched against the returned *relative* paths, so glob
/// metacharacters in the repo's absolute path can no longer corrupt matching
/// ([A1], [A26]).
pub struct FileSet;

impl FileSet {
    /// Tracked, regular-file candidates (relative paths) for a repository.
    ///
    /// Submodule gitlinks (mode `160000`) and symlinks (mode `120000`) are
    /// excluded: a symlink would let a substitution write through to a target
    /// outside the worktree, and delete/restore semantics differ.
    pub fn candidates(repo_path: &Path) -> Result<Vec<PathBuf>> {
        debug!("FileSet::candidates: repo_path={}", repo_path.display());
        let entries = git::list_index_files(repo_path)?;
        let mut candidates = Vec::with_capacity(entries.len());

        for (mode, path) in entries {
            if mode == "160000" || mode == "120000" {
                trace!(
                    "FileSet::candidates: skipping {} (mode {mode})",
                    path.display()
                );
                continue;
            }
            // Defense in depth: a tracked path must never contain a `.git`
            // component. git would never list one, but assert it anyway ([A1]).
            if path
                .components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new(".git"))
            {
                warn!(
                    "FileSet::candidates: refusing candidate with .git component: {}",
                    path.display()
                );
                continue;
            }
            candidates.push(path);
        }

        debug!("FileSet::candidates: {} candidates", candidates.len());
        Ok(candidates)
    }

    /// Candidates matching any of the supplied glob patterns, deduplicated and
    /// sorted. Patterns are matched against relative paths with
    /// `require_literal_separator` so `*` does not cross directory boundaries
    /// (`**` does), matching shell/gitignore expectations.
    pub fn matching_any(repo_path: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
        debug!(
            "FileSet::matching_any: repo_path={} patterns={:?}",
            repo_path.display(),
            patterns
        );
        let candidates = Self::candidates(repo_path)?;
        let compiled = patterns
            .iter()
            .map(|p| glob::Pattern::new(p).with_context(|| format!("Invalid glob pattern: {p}")))
            .collect::<Result<Vec<_>>>()?;

        let opts = glob::MatchOptions {
            require_literal_separator: true,
            ..Default::default()
        };

        let mut matched: Vec<PathBuf> = candidates
            .into_iter()
            .filter(|path| compiled.iter().any(|pat| pat.matches_path_with(path, opts)))
            .collect();
        matched.sort();
        matched.dedup();

        debug!("FileSet::matching_any: {} matched", matched.len());
        Ok(matched)
    }
}

/// Atomically write bytes to `path`: write to a uniquely named temp file in the
/// target's own directory, fsync, then rename over the target. A crash or torn
/// write can never leave a truncated file in place ([A21]). The temp file lives
/// in the same directory so the final rename stays on one filesystem.
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };

    fs::create_dir_all(parent).with_context(|| {
        format!(
            "Failed to create parent directories for: {}",
            path.display()
        )
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temp file in: {}", parent.display()))?;
    tmp.write_all(content)
        .with_context(|| format!("Failed to write temp file for: {}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("Failed to fsync temp file for: {}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| eyre::eyre!("Failed to persist temp file to {}: {}", path.display(), e))?;

    debug!(
        "atomic_write: wrote {} bytes to {}",
        content.len(),
        path.display()
    );
    Ok(())
}

/// Validate a relative path for `gx add` and resolve it to an absolute path
/// inside `repo_path`. This is the one write path that does not flow through
/// [`FileSet`], so it enforces the same policy directly ([A32]): reject absolute
/// paths, `..` components, and any `.git` component, and reject paths that would
/// escape the worktree through a symlinked parent.
pub fn validate_new_file_path(repo_path: &Path, file_path: &str) -> Result<PathBuf> {
    debug!(
        "validate_new_file_path: repo_path={} file_path={file_path}",
        repo_path.display()
    );

    let rel = Path::new(file_path);
    if rel.is_absolute() {
        return Err(eyre::eyre!(
            "File path must be relative, got absolute: {file_path}"
        ));
    }
    for comp in rel.components() {
        match comp {
            Component::ParentDir => {
                return Err(eyre::eyre!("File path must not contain '..': {file_path}"));
            }
            Component::Normal(s) if s == std::ffi::OsStr::new(".git") => {
                return Err(eyre::eyre!("File path must not target .git: {file_path}"));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(eyre::eyre!("File path must be relative: {file_path}"));
            }
            _ => {}
        }
    }

    let full = repo_path.join(rel);
    let repo_canon = repo_path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize repo path: {}", repo_path.display()))?;

    // Walk up to the deepest *existing* ancestor and canonicalize it; if it
    // resolves outside the repo, a symlinked parent is escaping the worktree.
    let mut ancestor = full.as_path();
    let existing = loop {
        if ancestor.exists() {
            break Some(ancestor);
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => break None,
        }
    };
    if let Some(existing) = existing {
        let existing_canon = existing
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize: {}", existing.display()))?;
        if !existing_canon.starts_with(&repo_canon) {
            return Err(eyre::eyre!(
                "File path escapes repository via symlink: {file_path}"
            ));
        }
    }

    Ok(full)
}

/// Apply a string substitution to a file
pub fn apply_substitution_to_file(
    file_path: &Path,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> Result<crate::diff::SubstitutionResult> {
    let Some(content) = read_utf8_or_skip(file_path)? else {
        return Ok(diff::SubstitutionResult::SkippedBinary);
    };

    Ok(diff::apply_substitution(
        &content,
        pattern,
        replacement,
        buffer,
    ))
}

/// Apply a regex substitution to a file
pub fn apply_regex_to_file(
    file_path: &Path,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> Result<crate::diff::SubstitutionResult> {
    let Some(content) = read_utf8_or_skip(file_path)? else {
        return Ok(diff::SubstitutionResult::SkippedBinary);
    };

    diff::apply_regex_substitution(&content, pattern, replacement, buffer)
}

/// Read a file as UTF-8, returning `Ok(None)` (with a `warn!`) when the file is
/// not valid UTF-8 so callers can skip binary files instead of corrupting them
/// or aborting the whole repository ([A21]).
pub fn read_utf8_or_skip(file_path: &Path) -> Result<Option<String>> {
    let bytes = fs::read(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    match String::from_utf8(bytes) {
        Ok(content) => Ok(Some(content)),
        Err(_) => {
            warn!("Skipping non-UTF-8 (binary) file: {}", file_path.display());
            Ok(None)
        }
    }
}

/// Write content to a file atomically, creating parent directories if needed.
pub fn write_file_content(file_path: &Path, content: &str) -> Result<()> {
    atomic_write(file_path, content.as_bytes())?;
    debug!("Wrote content to file: {}", file_path.display());
    Ok(())
}

/// Delete a file
pub fn delete_file(file_path: &Path) -> Result<()> {
    fs::remove_file(file_path)
        .with_context(|| format!("Failed to delete file: {}", file_path.display()))?;

    debug!("Deleted file: {}", file_path.display());
    Ok(())
}

/// Create a new file with content
pub fn create_file_with_content(
    file_path: &Path,
    content: &str,
    buffer: usize,
) -> Result<(String, String)> {
    // Ensure content has exactly one trailing newline
    let mut file_content = content.to_string();
    if !file_content.ends_with('\n') {
        file_content.push('\n');
    }

    // Generate diff from empty to new content
    let diff_output = diff::generate_diff("", &file_content, buffer);

    // Write the file
    write_file_content(file_path, &file_content)?;

    Ok((file_content, diff_output))
}

/// Backup a file by creating a .backup copy
pub fn backup_file(file_path: &Path) -> Result<PathBuf> {
    let backup_path = file_path.with_extension(format!(
        "{}.backup",
        file_path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));

    fs::copy(file_path, &backup_path).with_context(|| {
        format!(
            "Failed to backup file {} to {}",
            file_path.display(),
            backup_path.display()
        )
    })?;

    debug!(
        "Created backup: {} -> {}",
        file_path.display(),
        backup_path.display()
    );
    Ok(backup_path)
}

/// Restore a file from its backup
pub fn restore_from_backup(backup_path: &Path, original_path: &Path) -> Result<()> {
    fs::copy(backup_path, original_path).with_context(|| {
        format!(
            "Failed to restore from backup {} to {}",
            backup_path.display(),
            original_path.display()
        )
    })?;

    // Remove backup file
    fs::remove_file(backup_path)
        .with_context(|| format!("Failed to remove backup file: {}", backup_path.display()))?;

    debug!(
        "Restored from backup: {} -> {}",
        backup_path.display(),
        original_path.display()
    );
    Ok(())
}

/// Clean up a backup file without restoring (for successful operations)
pub fn cleanup_backup_file(backup_path: &Path) -> Result<()> {
    if backup_path.exists() {
        fs::remove_file(backup_path)
            .with_context(|| format!("Failed to remove backup file: {}", backup_path.display()))?;
        debug!("Cleaned up backup file: {}", backup_path.display());
    }
    Ok(())
}

/// Find all .backup files in a repository (recursive)
pub fn find_backup_files_recursive(repo_path: &Path) -> Result<Vec<PathBuf>> {
    use walkdir::WalkDir;

    let mut backup_files = Vec::new();

    for entry in WalkDir::new(repo_path).into_iter().filter_entry(|e| {
        // Skip .git directory and other hidden directories (but allow the root)
        let file_name = e.file_name().to_str().unwrap_or("");
        e.depth() == 0 || !file_name.starts_with('.')
    }) {
        let entry = entry.with_context(|| "Failed to read directory entry during backup scan")?;
        let path = entry.path();

        if path.is_file() {
            if let Some(extension) = path.extension() {
                if extension == "backup" {
                    backup_files.push(path.to_path_buf());
                }
            }
        }
    }

    backup_files.sort();
    Ok(backup_files)
}

#[cfg(test)]
mod tests;
