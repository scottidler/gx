use super::*;
use local::config::Config;
use local::test_utils::env_lock;

/// Build a `Config` carrying a `github.token-env` block from YAML, matching the
/// parse-based construction pattern in `src/config/tests.rs`.
fn config_from(yaml: &str) -> Config {
    serde_yaml::from_str::<Config>(yaml).unwrap()
}

/// Save `$GH_PERSONA`, set it to `value` (or remove it when `None`), run `body`,
/// then restore the prior value. All under the process-wide env lock, since
/// `set_var`/`remove_var` are global (rust.md env-test contract).
fn with_gh_persona<F: FnOnce()>(value: Option<&str>, body: F) {
    let guard = env_lock();
    let prior = std::env::var("GH_PERSONA").ok();

    match value {
        Some(v) => unsafe { std::env::set_var("GH_PERSONA", v) },
        None => unsafe { std::env::remove_var("GH_PERSONA") },
    }

    body();

    match prior {
        Some(v) => unsafe { std::env::set_var("GH_PERSONA", v) },
        None => unsafe { std::env::remove_var("GH_PERSONA") },
    }
    drop(guard);
}

// ---- Built-in classification floor (no config, no GH_PERSONA) ----

#[test]
fn test_builtin_tatari_tv_is_work() {
    with_gh_persona(None, || {
        let config = Config::default();
        assert_eq!(
            resolve_token_env("tatari-tv", &config).unwrap(),
            "GITHUB_PAT_WORK"
        );
    });
}

#[test]
fn test_builtin_other_org_is_home() {
    with_gh_persona(None, || {
        let config = Config::default();
        assert_eq!(
            resolve_token_env("scottidler", &config).unwrap(),
            "GITHUB_PAT_HOME"
        );
    });
}

// ---- config by-org overrides the built-in tatari-tv classification ----

#[test]
fn test_by_org_beats_builtin_tatari_tv() {
    with_gh_persona(None, || {
        let config =
            config_from("github:\n  token-env:\n    by-org:\n      tatari-tv: GITHUB_PAT_HOME\n");
        assert_eq!(
            resolve_token_env("tatari-tv", &config).unwrap(),
            "GITHUB_PAT_HOME"
        );
    });
}

#[test]
fn test_by_org_can_select_service_pat() {
    with_gh_persona(None, || {
        let config =
            config_from("github:\n  token-env:\n    by-org:\n      ops: GITHUB_PAT_SERVICE\n");
        assert_eq!(
            resolve_token_env("ops", &config).unwrap(),
            "GITHUB_PAT_SERVICE"
        );
    });
}

// ---- GH_PERSONA beats config by-org (BITE test: fails if by-org checked first) ----

#[test]
fn test_gh_persona_work_beats_by_org() {
    with_gh_persona(Some("work"), || {
        // by-org would map scottidler -> HOME; GH_PERSONA=work must win.
        let config =
            config_from("github:\n  token-env:\n    by-org:\n      scottidler: GITHUB_PAT_HOME\n");
        assert_eq!(
            resolve_token_env("scottidler", &config).unwrap(),
            "GITHUB_PAT_WORK",
            "GH_PERSONA=work must override config by-org; if this reads GITHUB_PAT_HOME \
             the precedence order is inverted (by-org checked before GH_PERSONA)"
        );
    });
}

#[test]
fn test_gh_persona_home_beats_builtin_tatari_tv() {
    with_gh_persona(Some("home"), || {
        let config = Config::default();
        assert_eq!(
            resolve_token_env("tatari-tv", &config).unwrap(),
            "GITHUB_PAT_HOME"
        );
    });
}

// ---- config default applies only to the home floor, never tatari-tv ----

#[test]
fn test_default_applies_only_to_non_tatari_tv() {
    with_gh_persona(None, || {
        let config = config_from("github:\n  token-env:\n    default: GITHUB_PAT_SERVICE\n");
        assert_eq!(
            resolve_token_env("someorg", &config).unwrap(),
            "GITHUB_PAT_SERVICE",
            "config default overrides the home floor for unlisted orgs"
        );
        assert_eq!(
            resolve_token_env("tatari-tv", &config).unwrap(),
            "GITHUB_PAT_WORK",
            "the built-in tatari-tv -> work floor sits ABOVE config default and \
             must be unaffected by it"
        );
    });
}

// ---- GH_PERSONA edge cases ----

#[test]
fn test_gh_persona_bogus_is_loud_err() {
    with_gh_persona(Some("wrok"), || {
        let err = resolve_token_env("scottidler", &Config::default()).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("GH_PERSONA"),
            "error should name GH_PERSONA, got: {message}"
        );
    });
}

#[test]
fn test_gh_persona_empty_treated_as_unset() {
    with_gh_persona(Some("   "), || {
        // Trimmed-empty falls through to classification, not an error.
        let config = Config::default();
        assert_eq!(
            resolve_token_env("tatari-tv", &config).unwrap(),
            "GITHUB_PAT_WORK"
        );
        assert_eq!(
            resolve_token_env("scottidler", &config).unwrap(),
            "GITHUB_PAT_HOME"
        );
    });
}
