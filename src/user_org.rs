use crate::config::Config;
use crate::repo::Repo;
use eyre::Result;
use std::collections::HashSet;
use std::path::PathBuf;

/// User/Org detection result
#[derive(Debug, Clone)]
pub struct UserOrgContext {
    pub user_or_org: String,
    pub detection_method: DetectionMethod,
}

#[derive(Debug, Clone)]
pub enum DetectionMethod {
    Explicit,      // From CLI parameter
    AutoDetected,  // From directory structure
    Configuration, // From config file default
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
        return Ok(detected_orgs
            .into_iter()
            .map(|org| UserOrgContext {
                user_or_org: org,
                detection_method: DetectionMethod::AutoDetected,
            })
            .collect());
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

/// Auto-detect user/org(s) from repository user_org field
fn auto_detect_from_repos(repos: &[Repo]) -> Result<Vec<String>> {
    let user_orgs: HashSet<String> = repos
        .iter()
        .map(|repo| repo.user_org.user.clone())
        .collect();

    match user_orgs.len() {
        0 => Err(eyre::eyre!("No user/org detected from repositories")),
        _ => Ok(user_orgs.into_iter().collect()),
    }
}

/// Build token path from template and user/org
pub fn build_token_path(template: &str, user_or_org: &str) -> PathBuf {
    let expanded = template.replace("{user_or_org}", user_or_org);

    // Handle tilde expansion
    if let Some(stripped) = expanded.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }

    PathBuf::from(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
            Repo::from_slug("tatari-tv/philo".to_string()),
            Repo::from_slug("tatari-tv/frontend".to_string()),
        ];

        let result = auto_detect_from_repos(&repos).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "tatari-tv");
    }

    #[test]
    fn test_auto_detect_multiple_orgs() {
        let repos = vec![
            Repo::from_slug("tatari-tv/philo".to_string()),
            Repo::from_slug("scottidler/gx".to_string()),
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
            assert!(!format!("{method:?}").is_empty());
        }
    }

    #[test]
    fn test_user_org_context_debug() {
        let context = UserOrgContext {
            user_or_org: "test-org".to_string(),
            detection_method: DetectionMethod::AutoDetected,
        };

        let debug_str = format!("{context:?}");
        assert!(debug_str.contains("test-org"));
        assert!(debug_str.contains("AutoDetected"));
    }
}
