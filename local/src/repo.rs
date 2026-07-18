use eyre::Result;
use log::debug;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Structural layout of a discovered repo - known at discovery time from which
/// constructor ran, never re-derived downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// `.git` is a directory; the repo root is a work tree.
    Flat,
    /// The repo IS the default worktree of a bare container (`path` is that
    /// worktree, not the `.bare` root).
    Bare,
    /// Synthetic repo (`from_slug`); no filesystem to classify.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub slug: String, // Always determinable from git config or panic
    pub layout: Layout,
}

impl Repo {
    pub fn new(path: PathBuf) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // A flat repo probes origin at its own root and infers a fallback slug
        // from its own parent directory.
        let slug = resolve_slug(&name, &path, path.parent());
        Ok(Self {
            path,
            name,
            slug,
            layout: Layout::Flat,
        })
    }

    /// Construct a `Repo` for a bare container. The logical name is the
    /// *container* directory's name, but git operations run in the container's
    /// default `worktree` (which becomes `self.path`), because the container
    /// root is not a work tree. A bare container is ONE logical repo.
    pub fn from_container(container: &Path, worktree: PathBuf) -> Result<Self> {
        let name = container
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Probe origin inside the worktree (the container root has no work
        // tree); infer the fallback slug from the *container's* parent, so a
        // container behaves like a flat repo of the same name.
        let slug = resolve_slug(&name, &worktree, container.parent());
        Ok(Self {
            path: worktree,
            name,
            slug,
            layout: Layout::Bare,
        })
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
            layout: Layout::Unknown,
        }
    }
}

/// Discover git repositories starting from the given directory with workspace awareness
pub fn discover_repos(
    start_dir: &Path,
    max_depth: usize,
    ignore_patterns: &[String],
) -> Result<Vec<Repo>> {
    debug!(
        "discover_repos: start_dir={} max_depth={} ignore_patterns={:?}",
        start_dir.display(),
        max_depth,
        ignore_patterns
    );

    let search_root = find_workspace_root(start_dir, max_depth, ignore_patterns)?;
    debug!("Using search root: {}", search_root.display());

    let mut repos = Vec::new();

    for entry in WalkDir::new(&search_root)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| {
            !is_ignored_directory(e.path(), ignore_patterns) && !is_inside_bare_container(e.path())
        })
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

        // A bare container is ONE logical repo: emit its default worktree and
        // do NOT descend into its internals (its worktrees/.bare are pruned by
        // `is_inside_bare_container` above, so they are never separate repos).
        if entry.file_type().is_dir() && crate::bare::is_bare_container(path) {
            if is_ignored_directory(path, ignore_patterns) {
                debug!("Skipping ignored bare container: {}", path.display());
                continue;
            }
            match crate::bare::default_worktree(path)
                .and_then(|worktree| Repo::from_container(path, worktree))
            {
                Ok(repo) => {
                    debug!(
                        "Found bare container: {} (default worktree {}, layout={:?})",
                        repo.slug,
                        repo.path.display(),
                        repo.layout
                    );
                    repos.push(repo);
                }
                Err(e) => {
                    debug!("Skipping bare container at {}: {}", path.display(), e);
                }
            }
            continue;
        }

        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            if let Some(repo_root) = path.parent() {
                // Skip if this is an ignored directory
                if is_ignored_directory(repo_root, ignore_patterns) {
                    debug!("Skipping ignored directory: {}", repo_root.display());
                    continue;
                }

                // Try to create repo, skip if it fails (e.g., invalid git config)
                match Repo::new(repo_root.to_path_buf()) {
                    Ok(repo) => {
                        debug!(
                            "Found repo: {} at {} (layout={:?})",
                            repo.name,
                            repo.path.display(),
                            repo.layout
                        );
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

/// Find the appropriate search root based on simple rules:
/// 1. If we're inside a repo (`.git` exists here): search from the parent so the
///    current repo and its siblings are included.
/// 2. Otherwise, if repos are found by searching downward: search from here.
///
/// We deliberately do NOT walk *up* the directory tree from a repo-less CWD
/// (the old case 3, which could widen scope all the way to `$HOME`). A repo-less
/// CWD now finds zero repos and the caller reports "no repositories found"
/// rather than silently expanding the blast radius ([A9]).
fn find_workspace_root(
    start_dir: &Path,
    max_depth: usize,
    ignore_patterns: &[String],
) -> Result<PathBuf> {
    let current = start_dir.to_path_buf();

    // Case 1: If we're inside a git repository, search from its parent.
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

    // Case 2: Search downward from the current directory.
    let repos_found_down = count_repos_in_subtree(&current, max_depth, ignore_patterns)?;
    if repos_found_down > 0 {
        debug!(
            "Found {} repos searching down from {}, using as search root",
            repos_found_down,
            current.display()
        );
        return Ok(current);
    }

    // No upward walk: keep scope anchored at the starting directory.
    debug!(
        "No repos found under {}, not widening scope",
        start_dir.display()
    );
    Ok(start_dir.to_path_buf())
}

/// Count git repositories in subtree with given max depth
fn count_repos_in_subtree(
    dir: &Path,
    max_depth: usize,
    ignore_patterns: &[String],
) -> Result<usize> {
    let mut count = 0;

    for entry in WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| {
            !is_ignored_directory(e.path(), ignore_patterns) && !is_inside_bare_container(e.path())
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        // A bare container counts as exactly one repo, same as a flat repo.
        if entry.file_type().is_dir() && crate::bare::is_bare_container(path) {
            if !is_ignored_directory(path, ignore_patterns) {
                count += 1;
            }
            continue;
        }
        if path.file_name() == Some(std::ffi::OsStr::new(".git")) && path.is_dir() {
            if let Some(repo_root) = path.parent() {
                if !is_ignored_directory(repo_root, ignore_patterns) {
                    count += 1;
                }
            }
        }
    }

    Ok(count)
}

/// True if `path`'s parent is a bare container, i.e. `path` is one of a
/// container's internal entries (`.git`, `.bare/`, or a worktree dir). Used to
/// prune the walk so a container's worktrees are never discovered as separate
/// repos - the container is emitted once as its default worktree instead.
fn is_inside_bare_container(path: &Path) -> bool {
    path.parent()
        .map(crate::bare::is_bare_container)
        .unwrap_or(false)
}

/// Derive a repo slug (`user/name`) from origin, falling back to parent-dir
/// inference. `origin_probe` is the path git runs in to read origin;
/// `fallback_parent` is the directory whose name seeds the fallback slug.
fn resolve_slug(name: &str, origin_probe: &Path, fallback_parent: Option<&Path>) -> String {
    match extract_origin_url(origin_probe).and_then(|url| extract_user_from_remote(&url)) {
        Ok(user) => format!("{user}/{name}"),
        Err(_) => {
            // Fallback: infer from the parent directory structure. If the repo
            // is at /path/to/user/repo, use user/repo; otherwise unknown/repo.
            if let Some(parent_name) = fallback_parent
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
            {
                // Skip common directory names that aren't user/org names.
                if !["repos", "src", "code", "projects", "workspace", "git"].contains(&parent_name)
                {
                    format!("{parent_name}/{name}")
                } else {
                    format!("unknown/{name}")
                }
            } else {
                format!("unknown/{name}")
            }
        }
    }
}

/// Extract origin URL for a repo path, layout-aware.
///
/// A flat repo has a `.git` *directory* with a `config` file we read directly
/// (fast, no subprocess). A linked worktree or bare container has a `.git`
/// *pointer file*, so `.git/config` does not exist there; fall back to asking
/// git (`git remote get-url origin`), which resolves the shared config.
fn extract_origin_url(repo_path: &Path) -> Result<String> {
    let dot_git = repo_path.join(".git");
    if dot_git.is_dir() {
        let config_path = dot_git.join("config");
        if let Ok(config_content) = std::fs::read_to_string(&config_path) {
            if let Some(url) = extract_remote_url_from_config(&config_content, "origin") {
                return Ok(url);
            }
        }
    }

    // Linked worktree, bare container, or a flat repo whose config parse missed
    // origin: ask git directly.
    crate::bare::origin_url(repo_path)
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

/// Check if a directory should be ignored during discovery.
///
/// Cache directories are excluded by path substring (this already catches
/// pre-commit caches under `~/.cache/`). Directory *names* are matched against
/// the configured `ignore_patterns` ([A27]); the previous `name.starts_with("repo")`
/// heuristic is gone - it silently hid real repos like `reporting` ([A6]).
fn is_ignored_directory(path: &Path, ignore_patterns: &[String]) -> bool {
    if let Some(path_str) = path.to_str() {
        // Ignore cache directories by path - this catches the pre-commit cache.
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
        if ignore_patterns.iter().any(|pattern| pattern == name) {
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
    use crate::test_utils::create_minimal_test_repo;
    use tempfile::TempDir;

    #[test]
    fn test_reporting_named_repo_is_discovered() {
        // The old `name.starts_with("repo")` heuristic silently hid repos like
        // `reporting` and `repository` ([A6]); they must now be discovered.
        let temp = TempDir::new().unwrap();
        create_minimal_test_repo(temp.path(), "reporting");
        create_minimal_test_repo(temp.path(), "repository");
        create_minimal_test_repo(temp.path(), "normal");

        let repos = discover_repos(temp.path(), 3, &[]).unwrap();
        let names: Vec<String> = repos.iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"reporting".to_string()));
        assert!(names.contains(&"repository".to_string()));
        assert!(names.contains(&"normal".to_string()));
    }

    #[test]
    fn test_ignore_patterns_respected() {
        // A repo directory whose name matches a configured ignore pattern is
        // skipped ([A27]).
        let temp = TempDir::new().unwrap();
        create_minimal_test_repo(temp.path(), "keepme");
        create_minimal_test_repo(temp.path(), "vendor");

        let repos = discover_repos(temp.path(), 3, &["vendor".to_string()]).unwrap();
        let names: Vec<String> = repos.iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"keepme".to_string()));
        assert!(!names.contains(&"vendor".to_string()));
    }

    #[test]
    fn test_is_ignored_directory_uses_patterns() {
        let patterns = vec!["node_modules".to_string()];
        assert!(is_ignored_directory(
            Path::new("/x/node_modules"),
            &patterns
        ));
        // No name heuristic any more: `reporting` is not ignored.
        assert!(!is_ignored_directory(Path::new("/x/reporting"), &patterns));
    }

    #[test]
    fn test_workspace_root_not_widened_above_repoless_dir() {
        // A repo-less starting directory must not walk up to find repos ([A9]).
        let temp = TempDir::new().unwrap();
        create_minimal_test_repo(temp.path(), "sibling");
        let empty = temp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let repos = discover_repos(&empty, 3, &[]).unwrap();
        assert_eq!(repos.len(), 0);
    }

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
    fn test_new_sets_flat_layout() {
        let temp = TempDir::new().unwrap();
        create_minimal_test_repo(temp.path(), "flat-repo");
        let repo = Repo::new(temp.path().join("flat-repo")).unwrap();
        assert_eq!(repo.layout, Layout::Flat);
    }

    #[test]
    fn test_from_slug_sets_unknown_layout() {
        let repo = Repo::from_slug("tatari-tv/frontend".to_string());
        assert_eq!(repo.layout, Layout::Unknown);
    }

    #[test]
    fn test_from_container_sets_bare_layout() {
        let temp = TempDir::new().unwrap();
        let container =
            crate::test_utils::create_bare_container(temp.path(), "gx", "scottidler/gx");
        let worktree = crate::bare::default_worktree(&container).unwrap();
        let repo = Repo::from_container(&container, worktree).unwrap();
        assert_eq!(repo.layout, Layout::Bare);
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
