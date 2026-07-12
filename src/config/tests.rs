use super::*;
use crate::test_utils::env_lock;
use tempfile::TempDir;

#[test]
fn test_xdg_config_dir_honors_env_and_falls_back() {
    let guard = env_lock();
    let prior = std::env::var("XDG_CONFIG_HOME").ok();

    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
    assert_eq!(xdg_config_dir().as_deref(), Some(dir.path()));

    unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    assert!(xdg_config_dir().unwrap().ends_with(".config"));

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    drop(guard);
}

#[test]
fn test_xdg_data_dir_honors_env_and_falls_back() {
    let guard = env_lock();
    let prior = std::env::var("XDG_DATA_HOME").ok();

    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_DATA_HOME", dir.path()) };
    assert_eq!(xdg_data_dir().as_deref(), Some(dir.path()));

    unsafe { std::env::remove_var("XDG_DATA_HOME") };
    assert!(xdg_data_dir().unwrap().ends_with(".local/share"));

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
    drop(guard);
}

#[test]
fn test_config_defaults() {
    let config = Config::default();
    assert_eq!(config.confirm_threshold(), DEFAULT_CONFIRM_THRESHOLD);
    assert_eq!(config.pr_body_template(), DEFAULT_PR_BODY_TEMPLATE);
    // The discovery defaults must NOT include `.git` (would hide every repo).
    assert!(!config.ignore_patterns().contains(&".git".to_string()));
    assert!(config
        .ignore_patterns()
        .contains(&"node_modules".to_string()));
}

/// `deny_unknown_fields` must reject a typo'd top-level key loudly, naming it -
/// not silently ignore it and fall back to defaults ([house rule], design doc
/// docs/design/2026-07-12-llm-propose-apply-and-mcp-server.md).
#[test]
fn test_unknown_top_level_key_fails_loudly_naming_it() {
    let yaml = "jobs: \"2\"\nlogging-level: debug\n"; // typo: should be nested `logging.level`
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("logging-level"),
        "error should name the unknown key, got: {message}"
    );
}

/// The same rejection must hold for a typo'd key inside a nested config
/// struct, not just at the top level.
#[test]
fn test_unknown_nested_key_fails_loudly_naming_it() {
    let yaml = "repo-discovery:\n  max-depht: 5\n"; // typo: max-depth
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("max-depht"),
        "error should name the unknown key, got: {message}"
    );
}

/// Sibling test proving the negative test above actually bites: without
/// `deny_unknown_fields` this same typo'd YAML parses successfully (which was
/// exactly the silent-drop bug the design doc calls out). Regression guard: if
/// `deny_unknown_fields` is ever removed from `Config`, this test's assertion
/// direction below would need to flip, which is the point - it would force
/// eyes on the change.
#[test]
fn test_valid_config_with_known_keys_only_still_loads() {
    let yaml = "jobs: \"2\"\nrepo-discovery:\n  max-depth: 5\n";
    let config = serde_yaml::from_str::<Config>(yaml).unwrap();
    assert_eq!(config.jobs.as_deref(), Some("2"));
    assert_eq!(
        config.repo_discovery.as_ref().and_then(|rd| rd.max_depth),
        Some(5)
    );
}

/// `Config::load(None)` (the default `$XDG_CONFIG_HOME/gx/gx.yml` path) must
/// propagate a parse failure loudly, not swallow it into a `warn!` + silent
/// default - that swallow was the bug: a typo'd key at the real config
/// location used to run with defaults and no visible complaint.
#[test]
fn test_load_at_default_location_fails_loudly_on_typo() {
    let guard = env_lock();
    let prior = std::env::var("XDG_CONFIG_HOME").ok();

    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
    let project_dir = dir.path().join(env!("CARGO_PKG_NAME"));
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(
        project_dir.join(format!("{}.yml", env!("CARGO_PKG_NAME"))),
        "jobs: \"2\"\nlogging-level: debug\n",
    )
    .unwrap();

    let err = Config::load(None).unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("logging-level"),
        "error should name the unknown key, got: {message}"
    );

    match prior {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    drop(guard);
}
