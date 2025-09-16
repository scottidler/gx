use colored::*;
use eyre::Result;
use regex::Regex;
use similar::{ChangeTag, TextDiff};

/// Result of a substitution operation
#[derive(Debug, Clone)]
pub enum SubstitutionResult {
    /// Content was changed (updated_content, diff)
    Changed(String, String),
    /// Pattern was valid but found no matches
    NoMatches,
    /// Pattern found matches but replacement resulted in no changes
    NoChange,
}

/// Generate a colored diff between original and updated content
pub fn generate_diff(original: &str, updated: &str, buffer: usize) -> String {
    if updated.is_empty() {
        let mut result = String::new();
        for (i, line) in original.lines().enumerate() {
            result.push_str(&format!(
                "{} | {}\n",
                format!("-{:4}", i + 1).red(),
                line.red()
            ));
        }
        return result;
    }

    let diff = TextDiff::from_lines(original, updated);
    let mut result = String::new();

    for group in diff.grouped_ops(buffer) {
        for op in group {
            for change in diff.iter_changes(&op) {
                match change.tag() {
                    ChangeTag::Delete => {
                        result.push_str(&format!(
                            "{} | {}\n",
                            format!("-{:4}", change.old_index().unwrap() + 1).red(),
                            change.to_string().trim_end().red()
                        ));
                    }
                    ChangeTag::Insert => {
                        result.push_str(&format!(
                            "{} | {}\n",
                            format!("+{:4}", change.new_index().unwrap() + 1).green(),
                            change.to_string().trim_end().green()
                        ));
                    }
                    ChangeTag::Equal => {
                        result.push_str(&format!(
                            "{} | {}\n",
                            format!(" {:4}", change.old_index().unwrap() + 1).dimmed(),
                            change.to_string().trim_end().dimmed()
                        ));
                    }
                }
            }
        }
    }
    result
}

/// Apply a string substitution to content and return result
pub fn apply_substitution(
    content: &str,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> SubstitutionResult {
    if !content.contains(pattern) {
        return SubstitutionResult::NoMatches;
    }
    let updated = content.replace(pattern, replacement);
    if updated == content {
        return SubstitutionResult::NoChange;
    }
    let diff = generate_diff(content, &updated, buffer);
    SubstitutionResult::Changed(updated, diff)
}

/// Apply a regex substitution to content and return result
pub fn apply_regex_substitution(
    content: &str,
    pattern: &str,
    replacement: &str,
    buffer: usize,
) -> Result<SubstitutionResult> {
    let regex = Regex::new(pattern)?;
    if !regex.is_match(content) {
        return Ok(SubstitutionResult::NoMatches);
    }
    let updated = regex.replace_all(content, replacement).to_string();
    if updated == content {
        return Ok(SubstitutionResult::NoChange);
    }
    let diff = generate_diff(content, &updated, buffer);
    Ok(SubstitutionResult::Changed(updated, diff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_empty_updated() {
        let original = "line1\nline2\nline3";
        let updated = "";
        let result = generate_diff(original, updated, 1);

        // Should show all original lines as deletions (ignoring color codes)
        assert!(result.contains("-   1"));
        assert!(result.contains("-   2"));
        assert!(result.contains("-   3"));
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
    }

    #[test]
    fn test_generate_diff_no_changes() {
        let original = "line1\nline2\nline3";
        let updated = "line1\nline2\nline3";
        let result = generate_diff(original, updated, 1);

        // When there are no changes, the diff should be empty
        assert!(result.is_empty());
    }

    #[test]
    fn test_generate_diff_with_changes() {
        let original = "line1\nline2\nline3";
        let updated = "line1\nmodified_line2\nline3";
        let result = generate_diff(original, updated, 1);

        // Should show deletion and insertion (ignoring color codes)
        assert!(result.contains("-   2"));
        assert!(result.contains("+   2"));
        assert!(result.contains("line2"));
        assert!(result.contains("modified_line2"));
    }

    #[test]
    fn test_generate_diff_empty_original() {
        let original = "";
        let updated = "new_line1\nnew_line2";
        let result = generate_diff(original, updated, 1);

        // Should show all lines as insertions (ignoring color codes)
        assert!(result.contains("+   1"));
        assert!(result.contains("+   2"));
        assert!(result.contains("new_line1"));
        assert!(result.contains("new_line2"));
    }

    #[test]
    fn test_apply_substitution() {
        let content = "Hello world\nThis is a test\nHello again";

        // Test successful substitution
        let result = apply_substitution(content, "Hello", "Hi", 1);
        assert!(matches!(result, SubstitutionResult::Changed(_, _)));
        if let SubstitutionResult::Changed(updated, diff) = result {
            assert_eq!(updated, "Hi world\nThis is a test\nHi again");
            assert!(!diff.is_empty());
        }

        // Test no match
        let result = apply_substitution(content, "nonexistent", "replacement", 1);
        assert!(matches!(result, SubstitutionResult::NoMatches));

        // Test no change (shouldn't happen with contains check, but for completeness)
        let result = apply_substitution("", "test", "replacement", 1);
        assert!(matches!(result, SubstitutionResult::NoMatches));
    }

    #[test]
    fn test_apply_regex_substitution() {
        let content = "version 1.2.3\nother line\nversion 4.5.6";

        // Test successful regex substitution
        let result =
            apply_regex_substitution(content, r"version \d+\.\d+\.\d+", "version X.X.X", 1);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(matches!(result, SubstitutionResult::Changed(_, _)));
        if let SubstitutionResult::Changed(updated, diff) = result {
            assert_eq!(updated, "version X.X.X\nother line\nversion X.X.X");
            assert!(!diff.is_empty());
        }

        // Test no match
        let result = apply_regex_substitution(content, r"nonexistent \d+", "replacement", 1);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), SubstitutionResult::NoMatches));

        // Test invalid regex
        let result = apply_regex_substitution(content, "[invalid", "replacement", 1);
        assert!(result.is_err());
    }
}
