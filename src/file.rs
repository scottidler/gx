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

/// Mode a brand-new file gets from `atomic_write`, set explicitly rather than
/// inherited from the temp file's creation mode or the process umask (F3).
const NEW_FILE_MODE: u32 = 0o644;

/// Atomically write bytes to `path`: write to a uniquely named temp file in the
/// target's own directory, fsync, then rename over the target. A crash or torn
/// write can never leave a truncated file in place ([A21]). The temp file lives
/// in the same directory so the final rename stays on one filesystem.
///
/// Permissions are handled explicitly rather than inherited from the temp
/// file's restrictive creation mode (F3): an existing target's mode is stat'd
/// before the write and applied to the temp file before the rename, so
/// rewriting a tracked executable can never flip it to 0600. A brand-new
/// target gets [`NEW_FILE_MODE`] set directly - not derived from the umask.
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

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = mode_of(path).unwrap_or(NEW_FILE_MODE);
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("Failed to set mode on temp file for: {}", path.display()))?;
    }

    tmp.persist(path)
        .map_err(|e| eyre::eyre!("Failed to persist temp file to {}: {}", path.display(), e))?;

    debug!(
        "atomic_write: wrote {} bytes to {}",
        content.len(),
        path.display()
    );
    Ok(())
}

/// The permission bits of `path` (masked to the rwxrwxrwx + setuid/setgid/
/// sticky range), or an error if it does not exist / cannot be stat'd.
#[cfg(unix)]
fn mode_of(path: &Path) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path).with_context(|| format!("Failed to stat: {}", path.display()))?;
    Ok(meta.permissions().mode() & 0o7777)
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

/// Copy `original` to an out-of-tree `backup` path, creating parent dirs.
/// Returns `original`'s mode at backup time, so callers can carry it in the
/// `RestoreBackup` step and restore it even if `original` is later deleted
/// (delete-then-restore has nothing left to stat mode from otherwise, F3).
///
/// Backups live under `$XDG_DATA_HOME/gx/backups/<tx-id>/...`, never beside the
/// original ([A21]). This keeps `*.backup` files out of the worktree where they
/// could match later glob patterns or collide with user files.
pub fn create_backup(original: &Path, backup: &Path) -> Result<u32> {
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create backup dir: {}", parent.display()))?;
    }
    #[cfg(unix)]
    let mode = mode_of(original)?;
    #[cfg(not(unix))]
    let mode = NEW_FILE_MODE;
    fs::copy(original, backup).with_context(|| {
        format!(
            "Failed to back up {} to {}",
            original.display(),
            backup.display()
        )
    })?;
    debug!(
        "create_backup: {} -> {} (mode {mode:o})",
        original.display(),
        backup.display()
    );
    Ok(mode)
}

/// Restore `original` from an out-of-tree `backup` (atomic write), then apply
/// `mode` (captured at backup time) explicitly - `atomic_write` alone would
/// preserve `original`'s CURRENT mode, which is wrong when `original` no
/// longer exists (a delete step ran) or was already re-written by a later
/// mutation. The backup itself is left in place; the whole tx backup dir is
/// removed on finalize or completed rollback.
pub fn restore_backup(backup: &Path, original: &Path, mode: u32) -> Result<()> {
    let bytes =
        fs::read(backup).with_context(|| format!("Failed to read backup: {}", backup.display()))?;
    atomic_write(original, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(original, std::fs::Permissions::from_mode(mode)).with_context(
            || {
                format!(
                    "Failed to set mode on restored file: {}",
                    original.display()
                )
            },
        )?;
    }
    debug!(
        "restore_backup: {} -> {} (mode {mode:o})",
        backup.display(),
        original.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
