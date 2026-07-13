//! Persona-aware GitHub token env-var resolution.
//!
//! gx crosses the home/work GitHub identity boundary within a single run
//! (mixed-org fleets). Instead of a per-org token FILE, gx resolves an
//! env-var NAME per org and reads that var's value elsewhere. This module owns
//! the NAME resolution only -- it never reads a token value (design doc
//! `2026-07-12-persona-aware-github-auth.md`, Phase 2).

use crate::config::Config;
use eyre::{bail, Result};
use log::debug;

#[cfg(test)]
mod tests;

/// Env var supplying the WORK-persona GitHub token (`tatari-tv/*`).
const WORK_TOKEN_ENV: &str = "GITHUB_PAT_WORK";
/// Env var supplying the HOME-persona GitHub token (`scottidler/*` and the
/// classification floor for every other org).
const HOME_TOKEN_ENV: &str = "GITHUB_PAT_HOME";
/// The whole-run persona override, mirroring the `gh()` shell wrapper.
const GH_PERSONA_ENV: &str = "GH_PERSONA";

/// An explicit whole-run persona from `$GH_PERSONA`.
enum Persona {
    Work,
    Home,
}

impl Persona {
    /// The env var NAME this persona reads its token from. Both literal names
    /// live once each as consts so the `GH_PERSONA` mapping and the classifier
    /// floor can never drift.
    fn env_name(&self) -> &'static str {
        match self {
            Persona::Work => WORK_TOKEN_ENV,
            Persona::Home => HOME_TOKEN_ENV,
        }
    }
}

/// Parse `$GH_PERSONA` into an explicit whole-run persona override.
///
/// - Unset, or trimmed-empty -> `Ok(None)` (fall through to classification).
/// - `work`/`home` -> `Ok(Some(..))`.
/// - Any other value -> loud `Err`. This is an INTENTIONAL stricter divergence
///   from the `gh()` wrapper, which silently falls back to its `$PWD`
///   classification on a bogus value (`.zshenv` `*)` branch). Under gx's
///   wrong-identity threat model a typo (`wrok`) must never silently pick an
///   identity (design doc Resolution model #1).
fn gh_persona() -> Result<Option<Persona>> {
    match std::env::var(GH_PERSONA_ENV) {
        Ok(raw) => {
            let value = raw.trim();
            if value.is_empty() {
                Ok(None)
            } else if value == "work" {
                Ok(Some(Persona::Work))
            } else if value == "home" {
                Ok(Some(Persona::Home))
            } else {
                bail!(
                    "invalid {GH_PERSONA_ENV}={raw:?}: expected `work` or `home` \
                     (gx refuses to guess a GitHub identity from a bogus persona)"
                );
            }
        }
        Err(_) => Ok(None),
    }
}

/// Resolve the env-var NAME whose value is the GitHub token for `org`.
///
/// Precedence (highest wins):
/// 1. `$GH_PERSONA` (`work`|`home`; any other non-empty value is a loud `Err`).
/// 2. config `github.token-env.by-org[org]` (exact per-org override).
/// 3. built-in `org == "tatari-tv"` -> [`WORK_TOKEN_ENV`] (undroppable floor,
///    kept in code so a partial config `by-org` map cannot silently clear it).
/// 4. config `github.token-env.default` (override for the home floor).
/// 5. built-in home floor -> [`HOME_TOKEN_ENV`].
///
/// Reads only `$GH_PERSONA` + `config`; it does NOT read the token value
/// itself. Returns the NAME to read (never a secret).
pub fn resolve_token_env(org: &str, config: &Config) -> Result<String> {
    debug!("resolve_token_env: org={org}");

    // 1. explicit whole-run override.
    if let Some(persona) = gh_persona()? {
        let name = persona.env_name().to_string();
        debug!("resolve_token_env: org={org} -> {name} (via {GH_PERSONA_ENV})");
        return Ok(name);
    }

    let token_env = config.token_env();

    // 2. exact per-org config override.
    if let Some(name) = token_env.by_org.get(org) {
        debug!("resolve_token_env: org={org} -> {name} (via config by-org)");
        return Ok(name.clone());
    }

    // 3. built-in classification floor: tatari-tv -> work. Always holds
    // regardless of a config `default`, so config can only override the HOME
    // floor, never the work classification.
    if org == "tatari-tv" {
        debug!("resolve_token_env: org={org} -> {WORK_TOKEN_ENV} (built-in tatari-tv)");
        return Ok(WORK_TOKEN_ENV.to_string());
    }

    // 4. config `default` override, else 5. built-in home floor.
    let name = token_env
        .default_env
        .clone()
        .unwrap_or_else(|| HOME_TOKEN_ENV.to_string());
    debug!("resolve_token_env: org={org} -> {name} (config default / built-in home floor)");
    Ok(name)
}
