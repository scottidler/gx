use eyre::Result;
use log::debug;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub slug: String, // Always determinable from git config or panic
}

impl Repo {
    pub fn new(path: PathBuf) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Extract git config information to determine slug
        // If we can't determine the user/org, fall back to a reasonable default
        let slug = match extract_origin_url(&path).and_then(|url| extract_user_from_remote(&url)) {
            Ok(user) => format!("{user}/{name}"),
            Err(_) => {
                // Fallback: try to infer from parent directory structure
                // If repo is at /path/to/user/repo, use user/repo
                // Otherwise use unknown/repo
                if let Some(parent) = path.parent() {
                    if let Some(parent_name) = parent.file_name().and_then(|n| n.to_str()) {
                        // Skip common directory names that aren't user/org names
                        if !["repos", "src", "code", "projects", "workspace", "git"]
                            .contains(&parent_name)
                        {
                            format!("{parent_name}/{name}")
                        } else {
                            format!("unknown/{name}")
                        }
                    } else {
                        format!("unknown/{name}")
                    }
                } else {
                    format!("unknown/{name}")
                }
            }
        };

        Ok(Self { path, name, slug })
    }

    /// Create a fake repo from slug for filtering purposes (used in clone command)
    pub fn from_slug(slug: String) -> Self {
        let parts: Vec<&str> = slug.split('/').collect();
        let name = if parts.len() == 2 {
            parts[1].to_string()
        } else {
            slug.clone()
        };

        Self {
            path: PathBuf::from(&name),
            name,
            slug,
        }
    }
}

/// Discover git repositories starting from the given directory with workspace awareness
pub fn discover_repos(start_dir: &Path, max_depth: usize) -> Result<Vec<Repo>> {
    debug!(
        "Discovering repos from {} with max depth {}",
        start_dir.display(),
        max_depth
    );

    // Find the actual search root - this is the key to fixing Stephen's issue
    let search_root = find_workspace_root(start_dir, max_depth)?;
    debug!("Using search root: {}", search_root.display());

    let mut repos = Vec::new();

    for entry in WalkDir::new(&search_root)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| !is_ignored_directory(e.path()))
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                // Log permission errors and other IO errors, but continue
                debug!("Skipping directory due to error: {err}");
                None
            }
        })
    {
        let path = entry.path();

        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            if let Some(repo_root) = path.parent() {
                // Skip if this is an ignored directory
                if is_ignored_directory(repo_root) {
                    debug!("Skipping ignored directory: {}", repo_root.display());
                    continue;
                }

                // Try to create repo, skip if it fails (e.g., invalid git config)
                match Repo::new(repo_root.to_path_buf()) {
                    Ok(repo) => {
                        debug!("Found repo: {} at {}", repo.name, repo.path.display());
                        repos.push(repo);
                    }
                    Err(e) => {
                        debug!(
                            "Skipping invalid repository at {}: {}",
                            repo_root.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    // Sort by path for consistent ordering
    repos.sort_by(|a, b| a.path.cmp(&b.path));

    debug!("Discovered {} repositories", repos.len());
    Ok(repos)
}

/// Find the appropriate search root based on simple rules
/// 1. If we're inside a repo (.git exists here): search from parent to include this repo
/// 2. If we're above repos: search down from here  
/// 3. If we're in random dir: move up until we find a repo, then search from its parent
fn find_workspace_root(start_dir: &Path, max_depth: usize) -> Result<PathBuf> {
    let mut current = start_dir.to_path_buf();

    // Case 1: If we're inside a git repository, search from its parent
    if current.join(".git").exists() {
        if let Some(parent) = current.parent() {
            debug!(
                "Inside git repo at {}, searching from parent: {}",
                current.display(),
                parent.display()
            );
            return Ok(parent.to_path_buf());
        }
    }

    // Case 2: Check if we can find repos by searching down from current directory
    let repos_found_down = count_repos_in_subtree(&current, max_depth)?;
    if repos_found_down > 0 {
        debug!(
            "Found {} repos searching down from {}, using as search root",
            repos_found_down,
            current.display()
        );
        return Ok(current);
    }

    // Case 3: No repos found downward, so walk up until we find something useful
    while let Some(parent) = current.parent() {
        current = parent.to_path_buf();
        
        // Check if this parent directory itself is a git repo
        if current.join(".git").exists() {
            // Found a repo, search from its parent
            if let Some(grandparent) = current.parent() {
                debug!(
                    "Found repo while walking up at {}, searching from parent: {}",
                    current.display(),
                    grandparent.display()
                );
                return Ok(grandparent.to_path_buf());
            } else {
                // Repo is at root level, search from repo itself
                debug!(
                    "Found repo at root level {}, searching from there",
                    current.display()
                );
                return Ok(current);
            }
        }
        
        // Check if this parent directory has repos underneath it
        let repos_found_down = count_repos_in_subtree(&current, max_depth)?;
        if repos_found_down > 0 {
            debug!(
                "Found {} repos while walking up, searching down from: {}",
                repos_found_down,
                current.display()
            );
            return Ok(current);
        }
    }

    // Fallback: use the original start_dir
    debug!("No repos found, using original start dir: {}", start_dir.display());
    Ok(start_dir.to_path_buf())
}

/// Count git repositories in subtree with given max depth
fn count_repos_in_subtree(dir: &Path, max_depth: usize) -> Result<usize> {
    let mut count = 0;

    for entry in WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| !is_ignored_directory(e.path()))
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            if let Some(repo_root) = path.parent() {
                if !is_ignored_directory(repo_root) {
                    count += 1;
                }
            }
        }
    }

    Ok(count)
}

/// Extract origin URL from git config
fn extract_origin_url(repo_path: &Path) -> Result<String> {
    let config_path = repo_path.join(".git").join("config");

    // Try to read git config
    let config_content = std::fs::read_to_string(&config_path).map_err(|_| {
        eyre::eyre!(
            "Repository at {} has no .git/config file",
            repo_path.display()
        )
    })?;

    // Extract origin URL (required)
    extract_remote_url_from_config(&config_content, "origin").ok_or_else(|| {
        eyre::eyre!(
            "Repository at {} has no remote origin configured",
            repo_path.display()
        )
    })
}

/// Extract user from remote URL
fn extract_user_from_remote(remote_url: &str) -> Result<String> {
    parse_user_from_url(remote_url)
        .map_err(|_| eyre::eyre!("Cannot parse user from remote URL: {remote_url}"))
}

/// Parse git config to extract remote URL
fn extract_remote_url_from_config(config_content: &str, remote_name: &str) -> Option<String> {
    let section_header = format!("[remote \"{remote_name}\"]");
    let mut in_remote_section = false;

    for line in config_content.lines() {
        let line = line.trim();

        if line == section_header {
            in_remote_section = true;
            continue;
        }

        if in_remote_section {
            if line.starts_with('[') {
                // Entered a new section, stop looking
                break;
            }

            if let Some(stripped) = line.strip_prefix("url = ") {
                return Some(stripped.trim().to_string());
            }
        }
    }

    None
}

/// Parse user from various git remote URL formats
fn parse_user_from_url(url: &str) -> Result<String> {
    // Handle SSH format: git@github.com:user/repo.git
    if let Some(ssh_match) = url.strip_prefix("git@") {
        if let Some(colon_pos) = ssh_match.find(':') {
            let path_part = &ssh_match[colon_pos + 1..];
            if let Some(slash_pos) = path_part.find('/') {
                let user = path_part[..slash_pos].to_string();
                return Ok(user);
            }
        }
    }

    // Handle SSH URL format: ssh://git@github.com/user/repo.git
    if url.starts_with("ssh://git@github.com/") {
        if let Some(path_part) = url.strip_prefix("ssh://git@github.com/") {
            if let Some(slash_pos) = path_part.find('/') {
                let user = path_part[..slash_pos].to_string();
                return Ok(user);
            }
        }
    }

    // Handle HTTPS format: https://github.com/user/repo.git
    if url.starts_with("https://github.com/") {
        if let Some(path_part) = url.strip_prefix("https://github.com/") {
            if let Some(slash_pos) = path_part.find('/') {
                let user = path_part[..slash_pos].to_string();
                return Ok(user);
            }
        }
    }

    Err(eyre::eyre!("Unsupported remote URL format: {url}"))
}

/// Check if directory should be ignored during discovery
fn is_ignored_directory(path: &Path) -> bool {
    if let Some(path_str) = path.to_str() {
        // Ignore cache directories by path - this should catch pre-commit cache
        if path_str.contains("/.cache/")
            || path_str.contains("/.local/")
            || path_str.contains("/.nvm/")
        {
            return true;
        }

        // Ignore Go module cache
        if path_str.contains("/go/pkg/mod/") {
            return true;
        }
    }

    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        // Ignore common build/cache directories
        if matches!(
            name,
            "node_modules" | "target" | "build" | ".next" | "dist" | "vendor"
        ) {
            return true;
        }

        // Ignore pre-commit cache directories that start with "repo" and have random suffixes
        if name.starts_with("repo")
            && name.len() >= 8
            && name.chars().skip(4).all(|c| c.is_alphanumeric())
        {
            return true;
        }
    }

    false
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
            let slug = &r.slug;
            patterns.iter().any(|pattern| slug == pattern)
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
            let slug = &r.slug;
            patterns.iter().any(|pattern| slug.starts_with(pattern))
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
    fn test_parse_user_from_url() {
        // Test SSH format
        let result = parse_user_from_url("git@github.com:tatari-tv/frontend.git").unwrap();
        assert_eq!(result, "tatari-tv");

        // Test SSH URL format
        let result = parse_user_from_url("ssh://git@github.com/scottidler/nvim").unwrap();
        assert_eq!(result, "scottidler");

        // Test HTTPS format
        let result = parse_user_from_url("https://github.com/scottidler/gx.git").unwrap();
        assert_eq!(result, "scottidler");

        // Test unsupported format should return error
        let result = parse_user_from_url("https://gitlab.com/org/repo.git");
        assert!(result.is_err());
    }

    #[test]
    fn test_filter_repos() {
        let repos = vec![
            Repo::from_slug("tatari-tv/frontend".to_string()),
            Repo::from_slug("tatari-tv/api".to_string()),
            Repo::from_slug("tatari-tv/frontend-utils".to_string()),
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
