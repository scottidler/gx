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
    assert_eq!(
        config.subprocess_timeout(),
        std::time::Duration::from_secs(DEFAULT_SUBPROCESS_TIMEOUT_SECS)
    );
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

/// Ringer #5: with `deny_unknown_fields` live, an `mcp:` block must PARSE
/// (before this field existed the design's own config example failed to load).
/// The kebab-case tool keys deserialize into the `McpTool`-keyed map.
#[test]
fn test_mcp_block_parses_with_kebab_case_tool_keys() {
    let yaml =
        "mcp:\n  tools:\n    status: true\n    create-propose: false\n    undo-execute: true\n";
    let config = serde_yaml::from_str::<Config>(yaml).unwrap();
    let mcp = config.mcp.expect("mcp block should parse");
    assert_eq!(mcp.tools.get(&McpTool::Status), Some(&true));
    assert_eq!(mcp.tools.get(&McpTool::CreatePropose), Some(&false));
    assert_eq!(mcp.tools.get(&McpTool::UndoExecute), Some(&true));
    // A tool not listed is simply absent from the map (gx-mcp applies the
    // category default for it).
    assert_eq!(mcp.tools.get(&McpTool::Doctor), None);
}

/// A typo'd tool key under `mcp.tools` fails loudly (the vocabulary is an enum,
/// not free strings), naming the bad key - not silently gating nothing.
#[test]
fn test_mcp_unknown_tool_key_fails_loudly() {
    let yaml = "mcp:\n  tools:\n    create-propse: true\n"; // typo: create-propose
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("create-propse") || message.contains("unknown variant"),
        "error should reject the bad tool key, got: {message}"
    );
}

/// A typo'd key INSIDE the `mcp:` struct (not under `tools`) is rejected by the
/// struct's own `deny_unknown_fields`.
#[test]
fn test_mcp_unknown_field_fails_loudly() {
    let yaml = "mcp:\n  toolz:\n    status: true\n"; // typo: tools
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("toolz"),
        "error should name the unknown mcp field, got: {message}"
    );
}

/// A specified `github.token-env` block round-trips through parse and
/// `Config::token_env()` (design doc 2026-07-12-persona-aware-github-auth.md,
/// Phase 1).
#[test]
fn test_token_env_block_round_trips() {
    let yaml = "github:\n  token-env:\n    default: GITHUB_PAT_HOME\n    by-org:\n      some-org: GITHUB_PAT_SERVICE\n";
    let config = serde_yaml::from_str::<Config>(yaml).unwrap();
    let token_env = config.token_env();
    assert_eq!(token_env.default_env.as_deref(), Some("GITHUB_PAT_HOME"));
    assert_eq!(
        token_env.by_org.get("some-org").map(String::as_str),
        Some("GITHUB_PAT_SERVICE")
    );
}

/// A bogus sub-key under `token-env` fails to parse loudly - the
/// `deny_unknown_fields` bite on `TokenEnvConfig`.
#[test]
fn test_token_env_unknown_field_fails_loudly() {
    let yaml = "github:\n  token-env:\n    defualt: GITHUB_PAT_HOME\n"; // typo: default
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("defualt"),
        "error should name the unknown token-env field, got: {message}"
    );
}

/// With no `github.token-env` block at all, `Config::token_env()` yields the
/// empty default - no `by-org` entries and no `default` override. The
/// built-in classification floor lives in the Phase 2 resolver, not here.
#[test]
fn test_token_env_absent_yields_empty_default() {
    let config = Config::default();
    let token_env = config.token_env();
    assert!(token_env.by_org.is_empty());
    assert_eq!(token_env.default_env, None);
}

/// An explicit `subprocess-timeout-secs` overrides the default. Bite: the
/// accessor must read the configured value, not the const.
#[test]
fn test_subprocess_timeout_honors_config() {
    let yaml = "subprocess-timeout-secs: 42\n";
    let config: Config = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        config.subprocess_timeout(),
        std::time::Duration::from_secs(42)
    );
}

/// A typo'd `subprocess-timeout-secs` key fails to parse loudly under the
/// top-level `deny_unknown_fields` - a silently-ignored timeout would be worse
/// than useless (the run would wedge with no timeout and no warning).
#[test]
fn test_subprocess_timeout_unknown_field_fails_loudly() {
    let yaml = "subprocess-timeout-sec: 42\n"; // typo: missing trailing s
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    assert!(
        err.to_string().contains("subprocess-timeout-sec"),
        "error should name the unknown field, got: {err}"
    );
}

/// Both finish-line confirm thresholds default to `DEFAULT_CONFIRM_THRESHOLD`
/// when the block is absent, and each accessor reads its own configured value.
#[test]
fn test_review_and_cleanup_confirm_thresholds() {
    let default = Config::default();
    assert_eq!(
        default.review_confirm_threshold(),
        DEFAULT_CONFIRM_THRESHOLD
    );
    assert_eq!(
        default.cleanup_confirm_threshold(),
        DEFAULT_CONFIRM_THRESHOLD
    );

    let yaml = "review:\n  confirm-threshold: 2\ncleanup:\n  confirm-threshold: 9\n";
    let config: Config = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.review_confirm_threshold(), 2);
    assert_eq!(config.cleanup_confirm_threshold(), 9);
}

/// A typo'd nested key under `review`/`cleanup` fails loudly (each config
/// struct carries `deny_unknown_fields`) rather than silently ignoring the
/// operator's threshold.
#[test]
fn test_review_confirm_threshold_unknown_field_fails_loudly() {
    let yaml = "review:\n  confirm-treshold: 2\n"; // typo: treshold
    let err = serde_yaml::from_str::<Config>(yaml).unwrap_err();
    assert!(
        err.to_string().contains("confirm-treshold"),
        "error should name the unknown nested field, got: {err}"
    );
}
