use crate::diff;
use eyre::{Context, Result};

use log::debug;
use std::fs;
use std::path::{Path, PathBuf};

/// Find files matching a glob pattern within a repository
pub fn find_files_in_repo(repo_path: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let search_pattern = repo_path.join(pattern).to_string_lossy().to_string();
    let mut matches = Vec::new();

    debug!("Searching for files with pattern: {search_pattern}");

    for entry in glob::glob(&search_pattern).context("Failed to create glob pattern")? {
        match entry {
            Ok(path) => {
                if path.is_file() {
                    if let Ok(relative_path) = path.strip_prefix(repo_path) {
                        matches.push(relative_path.to_path_buf());
                        debug!("Found matching file: {}", relative_path.display());
                    }
                }
            }
            Err(e) => {
                debug!("Error processing glob entry: {e}");
            }
        }
    }

    Ok(matches)
}

/// Apply a string substitution to a file
pub fn apply_substitution_to_file(
    file_path: &Path,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> Result<crate::diff::SubstitutionResult> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

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
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    diff::apply_regex_substitution(&content, pattern, replacement, buffer)
}

/// Write content to a file, creating parent directories if needed
pub fn write_file_content(file_path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create parent directories for: {}",
                file_path.display()
            )
        })?;
    }

    fs::write(file_path, content)
        .with_context(|| format!("Failed to write file: {}", file_path.display()))?;

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

/// Filter files by multiple patterns
#[allow(dead_code)]
pub fn filter_files_by_patterns(files: &[PathBuf], patterns: &[String]) -> Vec<PathBuf> {
    if patterns.is_empty() {
        return files.to_vec();
    }

    files
        .iter()
        .filter(|file| {
            let file_str = file.to_string_lossy();
            patterns.iter().any(|pattern| {
                if pattern.contains('*') {
                    // Simple glob matching for *.extension
                    if let Some(ext) = pattern.strip_prefix("*.") {
                        file_str.ends_with(&format!(".{ext}"))
                    } else {
                        file_str.contains(&pattern.replace('*', ""))
                    }
                } else {
                    file_str.contains(pattern)
                }
            })
        })
        .cloned()
        .collect()
}

/// Check if a file exists and is readable
#[allow(dead_code)]
pub fn is_file_accessible(file_path: &Path) -> bool {
    file_path.is_file() && fs::metadata(file_path).is_ok()
}

/// Get file size in bytes
#[allow(dead_code)]
pub fn get_file_size(file_path: &Path) -> Result<u64> {
    let metadata = fs::metadata(file_path)
        .with_context(|| format!("Failed to get metadata for: {}", file_path.display()))?;
    Ok(metadata.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_find_files_in_repo() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create test files
        fs::write(repo_path.join("file1.txt"), "content1").unwrap();
        fs::write(repo_path.join("file2.txt"), "content2").unwrap();
        fs::write(repo_path.join("file3.md"), "markdown").unwrap();
        fs::create_dir_all(repo_path.join("subdir")).unwrap();
        fs::write(repo_path.join("subdir").join("file4.txt"), "content4").unwrap();

        let result = find_files_in_repo(repo_path, "*.txt");
        assert!(result.is_ok());

        let files = result.unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.to_string_lossy() == "file1.txt"));
        assert!(files.iter().any(|f| f.to_string_lossy() == "file2.txt"));
    }

    #[test]
    fn test_find_files_in_repo_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create nested structure
        fs::create_dir_all(repo_path.join("src").join("utils")).unwrap();
        fs::write(repo_path.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(
            repo_path.join("src").join("utils").join("helper.rs"),
            "// helper",
        )
        .unwrap();

        let result = find_files_in_repo(repo_path, "**/*.rs");
        assert!(result.is_ok());

        let files = result.unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .any(|f| f.to_string_lossy().contains("main.rs")));
        assert!(files
            .iter()
            .any(|f| f.to_string_lossy().contains("helper.rs")));
    }

    #[test]
    fn test_filter_files_by_patterns() {
        let files = vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("tests/test.rs"),
            PathBuf::from("README.md"),
            PathBuf::from("Cargo.toml"),
        ];

        // Test with patterns
        let patterns = vec!["*.rs".to_string()];
        let filtered = filter_files_by_patterns(&files, &patterns);
        assert_eq!(filtered.len(), 3);

        // Test with multiple patterns
        let patterns = vec!["*.rs".to_string(), "*.md".to_string()];
        let filtered = filter_files_by_patterns(&files, &patterns);
        assert_eq!(filtered.len(), 4);

        // Test with empty patterns
        let filtered = filter_files_by_patterns(&files, &[]);
        assert_eq!(filtered.len(), files.len());
    }

    #[test]
    fn test_apply_substitution_to_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world\nThis is a test\nHello again").unwrap();

        let result = apply_substitution_to_file(&file_path, "Hello", "Hi", 1);
        assert!(result.is_ok());

        let result = result.unwrap();
        assert!(matches!(result, crate::diff::SubstitutionResult::Changed(_, _)));

        if let crate::diff::SubstitutionResult::Changed(updated, diff) = result {
            assert_eq!(updated, "Hi world\nThis is a test\nHi again");
            assert!(!diff.is_empty());
        }
    }

    #[test]
    fn test_apply_regex_to_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "version 1.2.3\nother line\nversion 4.5.6").unwrap();

        let result = apply_regex_to_file(&file_path, r"version \d+\.\d+\.\d+", "version X.X.X", 1);
        assert!(result.is_ok());

        let result = result.unwrap();
        assert!(matches!(result, crate::diff::SubstitutionResult::Changed(_, _)));

        if let crate::diff::SubstitutionResult::Changed(updated, diff) = result {
            assert_eq!(updated, "version X.X.X\nother line\nversion X.X.X");
            assert!(!diff.is_empty());
        }
    }

    #[test]
    fn test_create_file_with_content() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("new_file.txt");

        let result = create_file_with_content(&file_path, "Hello world", 1);
        assert!(result.is_ok());

        let (content, diff) = result.unwrap();
        assert_eq!(content, "Hello world\n");
        assert!(!diff.is_empty());
        assert!(file_path.exists());

        let file_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(file_content, "Hello world\n");
    }

    #[test]
    fn test_delete_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("to_delete.txt");
        fs::write(&file_path, "content").unwrap();

        assert!(file_path.exists());

        let result = delete_file(&file_path);
        assert!(result.is_ok());
        assert!(!file_path.exists());
    }

    #[test]
    fn test_write_file_content_with_nested_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("nested").join("dir").join("file.txt");

        let result = write_file_content(&file_path, "nested content");
        assert!(result.is_ok());
        assert!(file_path.exists());

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "nested content");
    }

    #[test]
    fn test_is_file_accessible() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("accessible.txt");
        let nonexistent_path = temp_dir.path().join("nonexistent.txt");

        fs::write(&file_path, "content").unwrap();

        assert!(is_file_accessible(&file_path));
        assert!(!is_file_accessible(&nonexistent_path));
    }

    #[test]
    fn test_get_file_size() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("sized.txt");
        let content = "Hello, world!";
        fs::write(&file_path, content).unwrap();

        let result = get_file_size(&file_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), content.len() as u64);
    }

    #[test]
    fn test_backup_and_restore() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("original.txt");
        let original_content = "original content";
        fs::write(&file_path, original_content).unwrap();

        // Create backup
        let result = backup_file(&file_path);
        assert!(result.is_ok());
        let backup_path = result.unwrap();
        assert!(backup_path.exists());

        // Modify original
        fs::write(&file_path, "modified content").unwrap();
        let modified_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(modified_content, "modified content");

        // Restore from backup
        let result = restore_from_backup(&backup_path, &file_path);
        assert!(result.is_ok());
        assert!(!backup_path.exists()); // Backup should be cleaned up

        let restored_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(restored_content, original_content);
    }
}
