use eyre::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use crate::repo::Repo;
use crate::config::Config;

/// User/Org detection result
#[derive(Debug, Clone)]
pub struct UserOrgContext {
    pub user_or_org: String,
    pub detection_method: DetectionMethod,
}

#[derive(Debug, Clone)]
pub enum DetectionMethod {
    Explicit,           // From CLI parameter
    AutoDetected,       // From directory structure
    Configuration,      // From config file default
}

/// Determine user/org(s) from various sources with precedence
pub fn determine_user_orgs(
    cli_override: Option<&str>,
    global_override: Option<&str>,
    discovered_repos: &[Repo],
    config: &Config,
) -> Result<Vec<UserOrgContext>> {
    // 1. Explicit CLI parameter (highest precedence) - single org
    if let Some(user_org) = cli_override.or(global_override) {
        return Ok(vec![UserOrgContext {
            user_or_org: user_org.to_string(),
            detection_method: DetectionMethod::Explicit,
        }]);
    }

    // 2. Auto-detect from repository paths - potentially multiple orgs
    if let Ok(detected_orgs) = auto_detect_from_repos(discovered_repos) {
        return Ok(detected_orgs.into_iter().map(|org| UserOrgContext {
            user_or_org: org,
            detection_method: DetectionMethod::AutoDetected,
        }).collect());
    }

    // 3. Configuration file default - single org
    if let Some(default) = &config.default_user_org {
        return Ok(vec![UserOrgContext {
            user_or_org: default.clone(),
            detection_method: DetectionMethod::Configuration,
        }]);
    }

    Err(eyre::eyre!("Unable to determine user/org: not specified explicitly, cannot auto-detect from directory structure, and no default configured"))
}

/// Auto-detect user/org(s) from repository directory structure
fn auto_detect_from_repos(repos: &[Repo]) -> Result<Vec<String>> {
    let user_orgs: HashSet<String> = repos
        .iter()
        .filter_map(|repo| extract_user_org_from_path(&repo.path))
        .collect();

    match user_orgs.len() {
        0 => Err(eyre::eyre!("No user/org detected from repository paths")),
        _ => Ok(user_orgs.into_iter().collect()),
    }
}

/// Extract user/org from repository path
/// Examples (working from parent directory):
///   ./tatari-tv/philo -> Some("tatari-tv")
///   ./scottidler/gx -> Some("scottidler")
///   ./standalone-repo -> None (only 2 components)
fn extract_user_org_from_path(repo_path: &Path) -> Option<String> {
    let path_components: Vec<_> = repo_path.components().collect();

    // Look for pattern: ./user_or_org/repo_name
    // When running from parent directory, repo paths look like:
    // - ./tatari-tv/philo (3 components: ".", "tatari-tv", "philo")
    // - ./scottidler/gx (3 components: ".", "scottidler", "gx")
    // - ./standalone-repo (2 components: ".", "standalone-repo") - NOT a user/org pattern
    if path_components.len() == 3 {
        // Get the first directory component after "./" (user/org)
        if let Some(user_org_component) = path_components.get(1) {
            if let Some(user_org) = user_org_component.as_os_str().to_str() {
                // Skip common non-user-org directory names
                if !["src", "projects", "workspace", "repos", "git"].contains(&user_org) {
                    return Some(user_org.to_string());
                }
            }
        }
    }

    None
}

/// Build token path from template and user/org
pub fn build_token_path(template: &str, user_or_org: &str) -> PathBuf {
    let expanded = template.replace("{user_or_org}", user_or_org);

    // Handle tilde expansion
    if expanded.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&expanded[2..]);
        }
    }

    PathBuf::from(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_extract_user_org_from_path() {
        // Valid cases - 3 components: ./user_or_org/repo_name
        assert_eq!(
            extract_user_org_from_path(&PathBuf::from("./tatari-tv/philo")),
            Some("tatari-tv".to_string())
        );
        assert_eq!(
            extract_user_org_from_path(&PathBuf::from("./scottidler/gx")),
            Some("scottidler".to_string())
        );

        // Invalid cases - not 3 components or excluded names
        assert_eq!(
            extract_user_org_from_path(&PathBuf::from("./standalone-repo")),
            None
        );
        assert_eq!(
            extract_user_org_from_path(&PathBuf::from("./src/main")),
            None
        );
        assert_eq!(
            extract_user_org_from_path(&PathBuf::from(".")),
            None
        );
        assert_eq!(
            extract_user_org_from_path(&PathBuf::from("./projects/test")),
            None
        );
    }

    #[test]
    fn test_build_token_path() {
        let template = "~/.config/github/tokens/{user_or_org}";
        let result = build_token_path(template, "tatari-tv");

        if let Some(home) = dirs::home_dir() {
            let expected = home.join(".config/github/tokens/tatari-tv");
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_build_token_path_no_tilde() {
        let template = "/etc/tokens/{user_or_org}";
        let result = build_token_path(template, "scottidler");
        let expected = PathBuf::from("/etc/tokens/scottidler");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_auto_detect_from_repos() {
        let repos = vec![
            Repo {
                path: PathBuf::from("./tatari-tv/philo"),
                name: "philo".to_string(),
                slug: Some("tatari-tv/philo".to_string()),
            },
            Repo {
                path: PathBuf::from("./tatari-tv/frontend"),
                name: "frontend".to_string(),
                slug: Some("tatari-tv/frontend".to_string()),
            },
        ];

        let result = auto_detect_from_repos(&repos).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "tatari-tv");
    }

    #[test]
    fn test_auto_detect_multiple_orgs() {
        let repos = vec![
            Repo {
                path: PathBuf::from("./tatari-tv/philo"),
                name: "philo".to_string(),
                slug: Some("tatari-tv/philo".to_string()),
            },
            Repo {
                path: PathBuf::from("./scottidler/gx"),
                name: "gx".to_string(),
                slug: Some("scottidler/gx".to_string()),
            },
        ];

        let result = auto_detect_from_repos(&repos).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"tatari-tv".to_string()));
        assert!(result.contains(&"scottidler".to_string()));
    }

    #[test]
    fn test_detection_method_debug() {
        let methods = vec![
            DetectionMethod::Explicit,
            DetectionMethod::AutoDetected,
            DetectionMethod::Configuration,
        ];

        for method in methods {
            assert!(!format!("{:?}", method).is_empty());
        }
    }

    #[test]
    fn test_user_org_context_debug() {
        let context = UserOrgContext {
            user_or_org: "test-org".to_string(),
            detection_method: DetectionMethod::AutoDetected,
        };

        let debug_str = format!("{:?}", context);
        assert!(debug_str.contains("test-org"));
        assert!(debug_str.contains("AutoDetected"));
    }
}
