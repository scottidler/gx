# Design Document: Persona-Aware GitHub Auth for gx

**Author:** Scott Idler
**Date:** 2026-07-12
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

gx resolves its GitHub token from a per-org token FILE (`~/.config/github/tokens/<org>`). That file scheme is retired everywhere else in the toolchain: the persona system (`GH_PERSONA` + the `gh()` wrapper, dotfiles 2026-07-07) now keys off env vars decrypted at shell startup. This doc makes gx persona-aware: resolve the token per-org from `$GITHUB_PAT_WORK` / `$GITHUB_PAT_HOME`, using the exact classification rule from the `gh()` wrapper but keyed on the repo slug's ORG instead of `$PWD`. A single gx run can then cross the home/work boundary fluidly (mixed-org fleets), which one ambient `$GH_TOKEN` never could.

## Problem Statement

### Background

- gx's GitHub/token code predates the persona system by ~11 months: gx's `read_token` landed Aug 2025; `GH_PERSONA` + the `gh()` wrapper landed 2026-07-07 (dotfiles `f3ebc99`).
- The persona system fundamentally cannot help gx as built:
  - gx execs the `gh` BINARY via `std::process::Command`, never the `gh()` shell function, so `GH_PERSONA` / `gh-work` / `gh-home` are invisible to it.
  - gx's token comes from a file, injected as `.env("GH_TOKEN", token)` on the `gh` subprocess. Where it does hit `gh`, that `GH_TOKEN` overrides `gh`'s own persona switching anyway (per `secrets.md`).
- Net: gx has no persona-aware auth path. Provisioning is "drop a token file per org," divorced from the env-var flow every other tool now uses.

### Problem

- The file scheme is retired. We prefer env vars. gx is the last holdout.
- gx must cross the home/work persona boundary within a SINGLE run: a fleet can mix `scottidler/*` (home) and `tatari-tv/*` (work) repos. One ambient `$GH_TOKEN` cannot serve both, and the `$PWD`-keyed `gh()` wrapper is one-persona-per-shell. gx is the right place to solve this because it already knows each repo's org from its slug.

### Goals

- Retire the token-FILE scheme (`~/.config/github/tokens/<org>`, `token-path` config, `build_token_path`).
- Resolve the token per-org from env vars already present in every shell.
- Classification rule (owner, verbatim): any repo under `tatari-tv/` uses work (`$GITHUB_PAT_WORK`); everything else uses home (`$GITHUB_PAT_HOME`); the home token realistically only works for `scottidler/`.
- Honor `GH_PERSONA` (`work`|`home`) as an explicit whole-run override, mirroring the `gh()` wrapper.
- Fail loudly when the chosen env var is unset: a wrong-identity token silently 404s and reads as "no access," the exact trap `secrets.md` documents.
- Config carries env-var NAMES, never secret values.

### Non-Goals

- No direct GitHub REST client. gx shells out to `gh` for everything; there is zero `reqwest`/`Authorization` surface, and we are not adding one. This change is only "where does the string that becomes `GH_TOKEN` come from."
- No new decryption in gx. The `$GITHUB_PAT_*` vars are already decrypted at shell startup; gx just reads them.
- No `GH_PERSONA=service`. The `gh()` wrapper's `GH_PERSONA` is a plain work/home toggle; gx matches it. The service PAT is reachable only via an explicit config override (below), never automatic. (Parked; revisit if a service-identity gx run becomes a real need.)
- No configurable "default persona" knob beyond the org->env-var map. Deferred until it's an observed problem.

## Proposed Solution

### Overview

One choke point already exists: `read_token(user_or_org, config) -> Result<String>` (`src/github.rs:121`). Its first argument already IS the org. Rewrite its body to resolve an env-var NAME per org, then read that var. The signature is unchanged, so path-2 call sites (`clone.rs:65`, `github.rs:30`, `create/core.rs:1340`) are untouched. The only additional surface is making the swallow at `gh_command` fail loudly (below).

### Resolution model (precedence, highest wins)

The classification (`tatari-tv -> work`, `else -> home`) lives in CODE as a floor; config is a pure OVERRIDE layered on top. This ordering matters (see the serde footgun below):

1. **`GH_PERSONA` env** (`work`|`home`): forces persona for the whole run. `work` -> `GITHUB_PAT_WORK`, `home` -> `GITHUB_PAT_HOME`. The explicit escape hatch, keyed the same way the `gh()` wrapper overrides its `$PWD` guess. Trimmed-empty is treated as unset (falls through). **Intentional stricter divergence from the wrapper:** on any other non-`work`/`home` value the wrapper silently falls back to its `$PWD` classification (`.zshenv:37`, `*)` branch); gx instead errors loudly. A typo'd persona (`wrok`) must not silently pick an identity under the wrong-identity threat model.
2. **Config `github.token-env.by-org[<org>]`**: exact per-org override; use this env-var name. Escape hatch for any org, including naming `GITHUB_PAT_SERVICE`.
3. **Built-in `org == "tatari-tv"` -> `WORK_TOKEN_ENV`.** Always holds regardless of config.
4. **Config `github.token-env.default`**: override for the home floor (any org not `tatari-tv` and not in `by-org`).
5. **Built-in floor -> `HOME_TOKEN_ENV`.**

```rust
fn resolve_token_env(org: &str, config: &Config) -> Result<String> {
    if let Some(p) = gh_persona()? {            // 1: work|home | Err on bogus
        return Ok(p.env_name().to_string());
    }
    let te = config.token_env();                // effective config (never None; see accessor)
    if let Some(name) = te.by_org.get(org) {    // 2
        return Ok(name.clone());
    }
    if org == "tatari-tv" {                      // 3
        return Ok(WORK_TOKEN_ENV.to_string());
    }
    Ok(te.default_env.clone().unwrap_or_else(|| HOME_TOKEN_ENV.to_string())) // 4 -> 5
}
```

After resolving a NAME: `std::env::var(name)`. If unset or trimmed-empty -> loud `Err` naming the missing var AND the org that selected it. Never a silent empty string, never a silent ambient fallback for a mutating call.

**Why classification-in-code, not config `impl Default` data:** serde does not deep-merge. Container-level `#[serde(default)]` fills only fields ABSENT from the input (from the container's `Default`); a field PRESENT in the input is taken verbatim. So if the built-in `tatari-tv -> work` rule lived in the config's `impl Default` `by_org` map, a user writing `token-env: { by-org: { some-org: X } }` replaces the whole map and silently drops `tatari-tv -> work`. (A block that omits `by-org` entirely, e.g. `{ default: FOO }`, DOES preserve the built-in map via container default - but "preserved unless you touch the field" is a landmine, not a guarantee.) Keeping the classification as a code floor makes it a hard, undroppable policy: config `by-org`/`default` can only override or extend, never clear it. The two literal names live once each as consts (`WORK_TOKEN_ENV`, `HOME_TOKEN_ENV`), referenced by both the `GH_PERSONA` mapping and the resolver, so they can't drift.

The `gx.yml` example ships the block COMMENTED, documenting the built-in behavior and how to override:

```yaml
github:
  # Token env-var resolution. Built-in: tatari-tv -> $GITHUB_PAT_WORK, else -> $GITHUB_PAT_HOME.
  # GH_PERSONA=work|home overrides per-run. Uncomment to add overrides (names, never secrets):
  # token-env:
  #   default: GITHUB_PAT_HOME      # override the home floor for unlisted orgs
  #   by-org:
  #     some-org: GITHUB_PAT_SERVICE
```

### Fail-loud vs the current swallow

- **Path 2** (pre-resolved token string): `clone.rs:65` and `github.rs:30` already `?`-propagate `read_token`'s `Err`. They fail loudly for free.
- **`resolve_base_branch`** (`create/core.rs:1340`): `if let Ok(token) = read_token(...)` is INTENTIONALLY soft per audit finding `[A4]` ("a lookup failure must never drop the PR"). A missing var here falls through to warn+`main`, which is the designed behavior. It stays soft for free: the new `Err` is caught by the existing `if let Ok` and the fallback fires. NO change.
- **Path 1** (`gh_command`, `github.rs:153`): TODAY it swallows a token-read `Err` into `debug!(... "using ambient gh auth")` and returns a `Command` with no `GH_TOKEN`. For the mutating calls (create PR, revert PR, approve, merge, close, delete-branch, list-branches, list-open-pr-branches) that ambient fallback is exactly where a wrong or absent identity silently acts as the wrong account. This is the one behavior change: `gh_command` becomes fallible and propagates the loud error.

### API Design

```rust
// src/github.rs (or a new src/persona.rs seam)

const WORK_TOKEN_ENV: &str = "GITHUB_PAT_WORK";
const HOME_TOKEN_ENV: &str = "GITHUB_PAT_HOME";

/// Resolve the env-var NAME for `org`: GH_PERSONA > config by-org >
/// built-in tatari-tv > config default > built-in home floor (see
/// Resolution model). Reads only GH_PERSONA + config; no other I/O.
fn resolve_token_env(org: &str, config: &Config) -> Result<String>;

/// Read the token for `user_or_org`. Signature UNCHANGED from today.
/// New body: resolve_token_env -> std::env::var -> loud Err on unset/empty.
pub fn read_token(user_or_org: &str, config: &Config) -> Result<String>;

/// Was: `-> Command` (swallowed token errors -> ambient auth).
/// Now: `-> Result<Command>`, propagating read_token's loud Err.
fn gh_command(org: &str, config: &Config) -> Result<Command>;
```

Config additions (`src/config.rs`, mirroring `GithubConfig` / `McpConfig` patterns):

```rust
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(rename = "pr-body-template")]
    pub pr_body_template: Option<String>,
    #[serde(rename = "token-env")]
    pub token_env: Option<TokenEnvConfig>,
}

// Overrides ONLY (empty by default). The classification floor lives in
// resolve_token_env, so a partial block can never drop tatari-tv -> work.
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct TokenEnvConfig {
    /// Override for the home floor (orgs not tatari-tv and not in by-org).
    #[serde(rename = "default")]      // `default` is a Rust keyword
    pub default_env: Option<String>,
    /// org -> env-var NAME overrides. BTreeMap: deterministic, keyed-map form.
    pub by_org: BTreeMap<String, String>,
}

/// Effective token-env config: `GithubConfig`'s `token_env` field is
/// `Option` and `#[serde(default)]` leaves it `None` when the block is
/// absent, so resolve through an accessor that yields the empty default
/// rather than reading the field directly (mirrors `pr_body_template()`
/// at config.rs:330).
impl Config {
    pub fn token_env(&self) -> TokenEnvConfig { /* self.github?.token_env.unwrap_or_default() */ }
}
```

Retire from config: the top-level `token-path` field (`config.rs:41-42,240`) and `build_token_path` (`user_org.rs:79-90`). Under `deny_unknown_fields`, a stale `token-path:` key then fails to parse loudly with "unknown field `token-path`" - the correct fail-closed migration signal. The live config (`~/.config/gx/gx.yml`) does NOT set `token-path`, so nothing of Scott's breaks; the shipped example's `token-path:` line is removed in the same phase.

### Implementation Plan

#### Phase 0: Prove the env assumption (zero code)
**Model:** sonnet
- From a real `gx` invocation context, confirm the process inherits `$GITHUB_PAT_WORK`, `$GITHUB_PAT_HOME`, `$GITHUB_PAT_SERVICE` (a throwaway `std::env::var` probe or `gx` run under a debug print).
- Re-confirm `gh` honors `GH_TOKEN` passed via `.env` over ambient `gh auth` (already the current mechanism).
- **Success criteria:** a probe from a real `gx` run prints presence (not value) of all three vars; the missing-var behavior is confirmed decided as loud error.

#### Phase 1: Config surface
**Model:** sonnet
- Add `token_env: Option<TokenEnvConfig>` to `GithubConfig`; add `TokenEnvConfig` (derive `Default`, empty overrides); add the `Config::token_env()` accessor; add `WORK_TOKEN_ENV`/`HOME_TOKEN_ENV` consts.
- Add the COMMENTED `github.token-env` doc block to `gx.yml`; REMOVE the `token-path:` line.
- Tests in `src/config/tests.rs`: a specified `token-env` block round-trips; a bogus sub-key fails loudly; `token_env()` yields the empty default when the block is absent.
- **Success criteria:** `cargo test` config round-trip passes; a `token-env` with an unknown sub-key errors; `Config::default().token_env().by_org` is empty and `.default_env` is `None`.

#### Phase 2: Persona resolver
**Model:** opus
- Implement `resolve_token_env(org, config) -> Result<String>` with the full 5-level precedence: GH_PERSONA (work|home, else loud Err) > config `by-org` > built-in `tatari-tv` > config `default` > built-in home floor.
- Tests via `crate::test_utils::env_lock()` (save/restore `$GH_PERSONA` and the `$GITHUB_PAT_*` vars).
- **Success criteria (assert-style) — cover every precedence edge, not just the built-ins:**
  - built-in: `resolve_token_env("tatari-tv", &default)` == `"GITHUB_PAT_WORK"`; `resolve_token_env("scottidler", &default)` == `"GITHUB_PAT_HOME"`.
  - `by-org` beats built-in: with `by-org: {tatari-tv: GITHUB_PAT_HOME}`, `resolve_token_env("tatari-tv", ..)` == `"GITHUB_PAT_HOME"`.
  - `GH_PERSONA` beats `by-org`: `GH_PERSONA=work` with `by-org: {scottidler: GITHUB_PAT_HOME}` on org `scottidler` yields `"GITHUB_PAT_WORK"`.
  - config `default` applies only to non-`tatari-tv`: with `default: GITHUB_PAT_SERVICE`, `resolve_token_env("someorg", ..)` == `"GITHUB_PAT_SERVICE"` while `resolve_token_env("tatari-tv", ..)` still == `"GITHUB_PAT_WORK"`.
  - `by-org` can select the service PAT: with `by-org: {ops: GITHUB_PAT_SERVICE}`, `resolve_token_env("ops", ..)` == `"GITHUB_PAT_SERVICE"`.
  - `GH_PERSONA=home` on org `tatari-tv` yields `"GITHUB_PAT_HOME"`; `GH_PERSONA=bogus` is `Err`.

#### Phase 3: Rewire `read_token`
**Model:** opus
- Replace the file-read body with `resolve_token_env` -> `std::env::var` -> loud `Err` on unset/empty (naming the var and org). Keep the signature.
- Log the chosen var NAME and the token LENGTH at debug, never the value (`logging.md` sensitive-payload clause).
- Retire `build_token_path` and the `token_path` field.
- **Success criteria:** all existing `read_token` call sites compile unchanged; `grep fs::read_to_string src/github.rs` finds no token read; `grep -r 'tokens/' src/` (non-test) is clean; `read_token("scottidler", cfg)` with `$GITHUB_PAT_HOME` set returns it; with it unset returns `Err` naming `GITHUB_PAT_HOME`.

#### Phase 4: Fail-loud `gh_command` + test/doc cleanup
**Model:** sonnet
- Change `gh_command(org, config) -> Result<Command>`; remove the ambient-auth swallow; add `?` at the 8 call sites (`github.rs:295,505,610,647,665,683,715,745`).
- Leave `resolve_base_branch` (`create/core.rs:1340`) soft: its `if let Ok` already yields the `[A4]` warn+`main` fallback on the new `Err`.
- Migrate `test_utils.rs:369-383` (`should_run_github_tests` / `get_test_github_token`) from the `~/.config/github/tokens/scottidler` file to `$GITHUB_PAT_HOME`; drop `user_org.rs` file-path tests.
- Move `github.rs`'s inline `#[cfg(test)] mod tests` to `src/github/tests.rs` if new persona tests land there (per `rust.md` test placement).
- **Success criteria:** `otto ci` green; no `~/.config/github/tokens` path remains in `src/`; a mutating gh call with the persona var unset fails loudly rather than silently using ambient auth.

#### Phase 5: Operator docs
**Model:** sonnet
- Update `docs/clone-feature.md:105,346-361` and `docs/testing-infrastructure.md:55-56,112-124` to the env scheme; announce retirement of the file scheme. Called out as its own phase (not a buried bullet) because cross-cutting doc steps otherwise don't get executed by phase agents.
- **Success criteria:** `rg '\.config/github/tokens' docs/clone-feature.md docs/testing-infrastructure.md` is clean; both docs describe the env-var scheme and name `GH_PERSONA`.

## Acceptance Criteria

- [ ] gx resolves a `tatari-tv/*` repo's token to `$GITHUB_PAT_WORK` and a `scottidler/*` repo's to `$GITHUB_PAT_HOME` within one run, with no token file present.
- [ ] `GH_PERSONA=work` and `GH_PERSONA=home` override the org classification for the whole run; an invalid `GH_PERSONA` is a loud error.
- [ ] A mutating `gh` call whose selected `$GITHUB_PAT_*` var is unset fails loudly naming the var, never silently falling back to ambient auth.
- [ ] No `~/.config/github/tokens` path and no `build_token_path`/`token-path` remain in `src/` (non-test); a stale `token-path:` in a config fails to parse loudly.
- [ ] `otto ci` green; new persona-resolution tests bite (break the classifier -> a named test fails).

## Resolved Decisions

- **2026-07-12 | Missing var: fail loud (not ambient fallback).** `taste.md` fail-closed; a silent ambient fallback reintroduces the wrong-identity 404 this whole change exists to kill. Requires `gh_command -> Result<Command>` (8 mechanical call-site `?`s). `resolve_base_branch` stays soft by design (`[A4]`).
- **2026-07-12 | Service PAT: config-only, not automatic.** Owner's rule is binary work/home. `GITHUB_PAT_SERVICE` is reachable only by naming it in `github.token-env.by-org`. `GH_PERSONA`'s accepted VALUES stay work|home to match the `gh()` wrapper; its invalid-value HANDLING is a deliberate stricter divergence (gx errors, the wrapper falls back to `$PWD`) - see Resolution model #1.
- **2026-07-12 | Contradicting `GH_PERSONA` override: no warning.** Declined the panel's optional "warn when `GH_PERSONA=work` on a `scottidler` repo" nicety. `GH_PERSONA` is a WHOLE-RUN override, so on a mixed-org fleet (the exact cross-boundary use case) it would fire a warning on every repo of the other persona - noise precisely when the feature is used as designed. Explicit override stays the user's responsibility, matching the wrapper.
- **2026-07-12 | token-path field: delete it.** Live config doesn't set it; the shipped example's line is removed in Phase 1. `deny_unknown_fields` turns any stale key into a loud parse error, the correct migration signal.
- **2026-07-12 | Third-party orgs get home + write-fail is acceptable.** A non-scottidler, non-tatari-tv org classifies as home; the home token fails any authenticated write there, but gx write ops only make sense on repos you can push to anyway. The `by-org` map is the escape hatch if a working third-party token ever exists.
- **2026-07-12 | No org threading needed.** Every token-resolution site already knows the org (as the `user_or_org`/`org` param, or `slug.split('/').next()`); `Repo` needs no new field.

## Alternatives Considered

### Alternative 1: Minimal file-first, env-fallback in `read_token`
- **Description:** keep the file scheme; only add `GH_TOKEN`/`GITHUB_TOKEN` env fallback when no file exists.
- **Why not chosen:** files are retired; a one-line fallback doesn't cross the home/work boundary within a run (ambient `$GH_TOKEN` defaults to WORK per `secrets.md`, silently 404ing on `scottidler` repos). Rejected as a non-solution to the stated problem.

### Alternative 2: Shell out through the `gh()` wrapper / `gh auth switch`
- **Description:** invoke the persona wrapper or switch `gh` accounts per repo.
- **Why not chosen:** the wrapper is a zsh function, invisible to gx's `std::process::Command`; `gh auth switch` is shared mutable state unsafe under the many parallel gx/agent/cron sessions Scott runs (`secrets.md`). Per-invocation env injection (what gx already does) is the concurrency-safe form.

### Alternative 3: Persona as a two-table model in config (org->persona, persona->env)
- **Description:** config maps org->persona and persona->env-var separately.
- **Why not chosen:** more surface for the same behavior. Owner sketched `token-env: {default, by-org}` (org->env directly); consts keep the two literal names DRY. Fewer moving parts.

## Technical Considerations

### Dependencies

- No new crates. `std::env::var` + existing serde/config machinery.

### Security

- Config holds env-var NAMES, never values (committable, `taste.md`).
- Never print/echo/log a decrypted token; log the chosen var NAME and token LENGTH only (`secrets.md`, `logging.md`).
- Fail-closed: a missing var is a loud error, never a silent empty or a wrong-identity ambient fallback.

### Testing Strategy

- Unit tests for `resolve_token_env` covering the full precedence table, via `env_lock()` with save/restore.
- Config round-trip + `deny_unknown_fields` bite test.
- Break-the-classifier test: flip the `tatari-tv` default and assert the persona test fails (tests must bite).

### Rollout Plan

- Single repo (gx). `gx-mcp` has no token code and inherits the change via the gx lib for free.
- Ship order: this doc -> phases -> `otto ci` -> implementation audit -> `/cli-shakedown` -> bump -> live validation against the `gx-testing` faux repos (the current blocked validation unblocks here: no file, `$GITHUB_PAT_HOME` set, PR->undo lifecycle, zero disk writes).
- **Rollback is a binary downgrade, not a config toggle.** Phase 3 deletes the file scheme, so there is no config flag to revert to files - reverting means reinstalling the prior gx binary. Keep the `token-env` block in `gx.yml` COMMENTED until the new binary is deployed everywhere: an OLD binary parsing a config with an uncommented nested `token-env` key rejects it under `deny_unknown_fields`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Spawned/agent/cron gx run lacks the decrypted `$GITHUB_PAT_*` var | Med | Med | Fail loud naming the var; operator decrypts explicitly (`manifest age`) per `secrets.md` |
| `gh_command -> Result` churns 8 call sites | Low | Low | All 8 are in Result-returning fns; mechanical `?` |
| Stale `token-path:` in someone's config breaks parse | Low | Low | Intended loud migration signal; Scott's live config is already clean |
| `GH_PERSONA` forces wrong persona (home on a tatari-tv repo) | Low | Low | Explicit human override, same as the `gh()` wrapper; user's responsibility |
| Uncommented `token-env` block breaks an OLD gx binary (`deny_unknown_fields`) during a staged rollout or rollback | Low | Med | Keep the block commented until the new binary is deployed everywhere; rollback is a binary downgrade (see Rollout) |

## Open Questions

- (none)

## References

- `~/repos/.claude/rules/secrets.md` - the `gh()` wrapper, `GH_PERSONA`, `$GITHUB_PAT_*` names, the wrong-identity 404 trap.
- dotfiles `f3ebc99` (2026-07-07) - `GH_PERSONA` + `gh-work`/`gh-home` wrappers.
- `src/github.rs:121-164` - `read_token` + `gh_command` (the choke point).
- `src/config.rs:102-116` - `GithubConfig` / `impl Default` pattern to mirror.
- Audit finding `[A4]` - base-branch lookup must never drop the PR (why `resolve_base_branch` stays soft).
