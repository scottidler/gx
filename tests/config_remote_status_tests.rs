use gx::config::{Config, RemoteStatusConfig};
use serde_yaml;

/// Test RemoteStatusConfig default values
#[test]
fn test_remote_status_config_defaults() {
    let config = RemoteStatusConfig::default();

    assert_eq!(config.enabled, Some(true));
    assert_eq!(config.fetch_first, Some(false));
    assert_eq!(config.timeout_seconds, Some(10));
}

/// Test RemoteStatusConfig serialization/deserialization
#[test]
fn test_remote_status_config_serde() {
    let yaml_content = r#"
remote-status:
  enabled: true
  fetch-first: true
  timeout-seconds: 30
"#;

    let config: Config = serde_yaml::from_str(yaml_content).unwrap();

    assert!(config.remote_status.is_some());
    let remote_config = config.remote_status.unwrap();
    assert_eq!(remote_config.enabled, Some(true));
    assert_eq!(remote_config.fetch_first, Some(true));
    assert_eq!(remote_config.timeout_seconds, Some(30));
}

/// Test Config includes RemoteStatusConfig by default
#[test]
fn test_config_includes_remote_status_by_default() {
    let config = Config::default();

    assert!(config.remote_status.is_some());
    let remote_config = config.remote_status.unwrap();
    assert_eq!(remote_config.enabled, Some(true));
    assert_eq!(remote_config.fetch_first, Some(false));
    assert_eq!(remote_config.timeout_seconds, Some(10));
}

/// Test partial RemoteStatusConfig deserialization
#[test]
fn test_partial_remote_status_config() {
    let yaml_content = r#"
remote-status:
  enabled: false
"#;

    let config: Config = serde_yaml::from_str(yaml_content).unwrap();

    assert!(config.remote_status.is_some());
    let remote_config = config.remote_status.unwrap();
    assert_eq!(remote_config.enabled, Some(false));
    // Other fields get default values due to #[serde(default)]
    assert_eq!(remote_config.fetch_first, Some(false));
    assert_eq!(remote_config.timeout_seconds, Some(10));
}

/// Test RemoteStatusConfig serialization
#[test]
fn test_remote_status_config_serialization() {
    let mut config = Config::default();
    config.remote_status = Some(RemoteStatusConfig {
        enabled: Some(false),
        fetch_first: Some(true),
        timeout_seconds: Some(20),
    });

    let yaml = serde_yaml::to_string(&config).unwrap();

    assert!(yaml.contains("remote-status:"));
    assert!(yaml.contains("enabled: false"));
    assert!(yaml.contains("fetch-first: true"));
    assert!(yaml.contains("timeout-seconds: 20"));
}

/// Test empty RemoteStatusConfig handling
#[test]
fn test_empty_remote_status_config() {
    let yaml_content = r#"
remote-status: {}
"#;

    let config: Config = serde_yaml::from_str(yaml_content).unwrap();

    assert!(config.remote_status.is_some());
    let remote_config = config.remote_status.unwrap();
    // All fields get default values due to #[serde(default)]
    assert_eq!(remote_config.enabled, Some(true));
    assert_eq!(remote_config.fetch_first, Some(false));
    assert_eq!(remote_config.timeout_seconds, Some(10));
}
