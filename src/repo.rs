use eyre::{Context, Result};
use log::debug;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub slug: Option<String>,
}

impl Repo {
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let slug = extract_repo_slug(&path);

        Self { path, name, slug }
    }
}

/// Discover git repositories starting from the given directory
pub fn discover_repos(start_dir: &Path, max_depth: usize) -> Result<Vec<Repo>> {
    debug!(
        "Discovering repos from {} with max depth {}",
        start_dir.display(),
        max_depth
    );

    let mut repos = Vec::new();

    for entry in WalkDir::new(start_dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| !is_ignored_directory(e.path()))
    {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            if let Some(repo_root) = path.parent() {
                let repo = Repo::new(repo_root.to_path_buf());
                debug!("Found repo: {} at {}", repo.name, repo.path.display());
                repos.push(repo);
            }
        }
    }

    // Sort by path for consistent ordering
    repos.sort_by(|a, b| a.path.cmp(&b.path));

    debug!("Discovered {} repositories", repos.len());
    Ok(repos)
}

/// Extract repository slug (org/repo) from git remote origin if available
fn extract_repo_slug(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8(output.stdout).ok()?.trim().to_string();
    parse_repo_slug_from_url(&url)
}

/// Parse repository slug from git URL
fn parse_repo_slug_from_url(url: &str) -> Option<String> {
    // Handle GitHub SSH URLs: git@github.com:org/repo.git
    if let Some(ssh_part) = url.strip_prefix("git@github.com:") {
        return Some(ssh_part.trim_end_matches(".git").to_string());
    }

    // Handle GitHub SSH URLs with ssh:// prefix: ssh://git@github.com/org/repo.git
    if let Some(ssh_part) = url.strip_prefix("ssh://git@github.com/") {
        return Some(ssh_part.trim_end_matches(".git").to_string());
    }

    // Handle GitHub HTTPS URLs: https://github.com/org/repo.git
    if let Some(https_part) = url.strip_prefix("https://github.com/") {
        return Some(https_part.trim_end_matches(".git").to_string());
    }

    None
}

/// Check if directory should be ignored during discovery
fn is_ignored_directory(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        matches!(
            name,
            "node_modules" | "target" | "build" | ".next" | "dist" | "vendor"
        )
    } else {
        false
    }
}

/// Filter repositories using slam's 4-level filtering logic
pub fn filter_repos(repos: Vec<Repo>, patterns: &[String]) -> Vec<Repo> {
    if patterns.is_empty() {
        return repos;
    }

    debug!(
        "Filtering {} repos with patterns: {:?}",
        repos.len(),
        patterns
    );

    // Level 1: Exact match on repository name
    let level1: Vec<Repo> = repos
        .iter()
        .filter(|r| patterns.contains(&r.name))
        .cloned()
        .collect();

    if !level1.is_empty() {
        debug!("Level 1 (exact name match): {} repos", level1.len());
        return level1;
    }

    // Level 2: Starts-with match on repository name
    let level2: Vec<Repo> = repos
        .iter()
        .filter(|r| patterns.iter().any(|pattern| r.name.starts_with(pattern)))
        .cloned()
        .collect();

    if !level2.is_empty() {
        debug!("Level 2 (name starts-with): {} repos", level2.len());
        return level2;
    }

    // Level 3: Exact match on full repo slug
    let level3: Vec<Repo> = repos
        .iter()
        .filter(|r| {
            if let Some(slug) = &r.slug {
                patterns.iter().any(|pattern| slug == pattern)
            } else {
                false
            }
        })
        .cloned()
        .collect();

    if !level3.is_empty() {
        debug!("Level 3 (exact slug match): {} repos", level3.len());
        return level3;
    }

    // Level 4: Starts-with match on full repo slug
    let level4: Vec<Repo> = repos
        .iter()
        .filter(|r| {
            if let Some(slug) = &r.slug {
                patterns.iter().any(|pattern| slug.starts_with(pattern))
            } else {
                false
            }
        })
        .cloned()
        .collect();

    debug!("Level 4 (slug starts-with): {} repos", level4.len());
    level4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_repo_slug_from_url() {
        assert_eq!(
            parse_repo_slug_from_url("git@github.com:tatari-tv/frontend.git"),
            Some("tatari-tv/frontend".to_string())
        );

        assert_eq!(
            parse_repo_slug_from_url("https://github.com/tatari-tv/api.git"),
            Some("tatari-tv/api".to_string())
        );

        assert_eq!(
            parse_repo_slug_from_url("https://github.com/scottidler/gx"),
            Some("scottidler/gx".to_string())
        );

        assert_eq!(
            parse_repo_slug_from_url("https://gitlab.com/org/repo.git"),
            None
        );
    }

    #[test]
    fn test_filter_repos() {
        let repos = vec![
            Repo {
                path: PathBuf::from("/path/frontend"),
                name: "frontend".to_string(),
                slug: Some("tatari-tv/frontend".to_string()),
            },
            Repo {
                path: PathBuf::from("/path/api"),
                name: "api".to_string(),
                slug: Some("tatari-tv/api".to_string()),
            },
            Repo {
                path: PathBuf::from("/path/frontend-utils"),
                name: "frontend-utils".to_string(),
                slug: Some("tatari-tv/frontend-utils".to_string()),
            },
        ];

        // Level 1: Exact name match
        let filtered = filter_repos(repos.clone(), &["frontend".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "frontend");

        // Level 2: Starts-with name match
        let filtered = filter_repos(repos.clone(), &["front".to_string()]);
        assert_eq!(filtered.len(), 2);

        // Level 3: Exact slug match
        let filtered = filter_repos(repos.clone(), &["tatari-tv/api".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "api");

        // No patterns - return all
        let filtered = filter_repos(repos.clone(), &[]);
        assert_eq!(filtered.len(), 3);
    }
}
