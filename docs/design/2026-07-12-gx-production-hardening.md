# Design Document: gx Production Hardening

**Author:** Scott Idler
**Date:** 2026-07-12
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

A four-dimension audit of gx v0.6.0 (secrets/auth, destructive-ops, concurrency, maturity) found the `create -> undo -> rollback` triad genuinely solid, but three classes of real gaps for a single SRE running N changes across N repos: (1) the finish-line operations (`review approve`, `review delete`, `cleanup`) are irreversible on GitHub with NO confirmation, NO merge/CI check, and NO tests; (2) partial-failure reporting is not airtight or scriptable (`gx create` exits 0 on failure; a hung git/gh op has no timeout and can wedge the whole run before it reports); (3) a worker panic can silently blank results. This doc closes those gaps, harvesting patterns gx already has in-tree.

## Problem Statement

### Background

- gx is at v0.6.0. The `create/undo/rollback` safety spine (write-ahead recovery journal, per-repo atomic transactions, `flock` locks, crash-injection tests) is well built and fail-closed.
- Audit evidence (all `path:line` verified against current code):
  - **Reporting:** every command fans out with rayon `par_iter`, each repo returns a result struct carrying `error: Option<String>`, results are collected, a summary prints at the end. Isolation is solid: one repo's failure never aborts the run. BUT `status`/`checkout`/`clone` exit non-zero on failures (`status.rs:138`, `checkout.rs:151`, `clone.rs`) while `create` ends `Ok(())` (`create.rs`) -> exit 0 even when repos failed.
  - **Subprocess:** ~55 `git`/`gh` calls use blocking `Command::output()` with NO wall-clock timeout or kill. A hung `git fetch`/`gh` blocks its rayon worker forever; `>= -j` hung ops wedge the whole run and it never reaches the summary. The correct pattern EXISTS in `src/create/core/propose.rs:439-502` (own process group, poll `try_wait`, `kill -KILL -<pgid>` on deadline) but only the LLM-agent path uses it.
  - **Panic:** `status`/`checkout`/`clone` collect into `Mutex<Vec<_>>` and finish `results.into_inner().unwrap_or_default()` (`status.rs:133`, `checkout.rs:144`, `clone.rs:83`), and no panic hook is installed. rayon PROPAGATES a worker panic out of `par_iter` (verified in rayon-1.11.0), so an uncaught panic aborts the process with 101 - the `unwrap_or_default()` blank-to-empty path is only reachable if a future `catch_unwind` ever contains the panic. So the real gap is the missing diagnostic (no panic hook -> a terse abort with no ERROR line), not a silent "0 repos" today.
  - **Finish-line ops:** `review approve` = batch squash-merge across all detected orgs, NO confirmation, NO CI/mergeable check (`PrInfo` at `github.rs:359` has no such field), `--admin` bypasses branch protection, a failed `--approve` still merges (`github.rs:602-662`). `review delete` = close + hard-delete branch of OPEN (unmerged) PRs, NO gate (`review.rs:450-453`, `github.rs:665,683`). `cleanup` force-deletes local branches (`git branch -D`, `git.rs:1037-1047`) gated only by a trusted `PrMerged` status flag (`cleanup.rs:255`), NO git-level ancestry check, NO confirm gate, `--force` removes even the flag. All three are untested.
  - **Partial-discovery fail-OPEN:** `review approve`/`delete` catch an `Err` from `list_prs_by_change_id`, `log::warn!`, and CONTINUE mutating whatever PRs the other orgs returned (`review.rs:305-315`, `:430-441`). A token/network blip on one org yields a partial merge/delete reported as success. This directly contradicts a "fail-closed" thesis and must be closed by a preflight-complete-or-abort gate.
  - The one gated finish-line op, `review purge`, already has the right shape: `confirm_purge` (`review.rs:785`) fails closed on non-interactive stdin without `--yes`, refuses branches with an open PR. This is the in-tree pattern to mirror.

### Problem

For a single SRE running N changes across N repos, the danger is at the finish line, not the start. Backing out a campaign is safe; merging/deleting/cleaning it up is ungated and irreversible, and the run's own partial-failure signal isn't reliable enough to script on.

### Goals

- **Airtight, scriptable reporting.** `gx create` exits non-zero on any repo failure (match `status`/`checkout`/`clone`). A worker panic emits an ERROR diagnostic via a panic hook. `gx create --report <path>` writes a machine-readable failure summary (JSON) to a FILE, leaving stdout's human output untouched (no stdout-contract refactor - see Alternatives).
- **No run can be wedged by one hung repo.** Every `git`/`gh` subprocess gets a wall-clock timeout + process-group kill; a timed-out repo becomes a reported per-repo error, and the run still reaches its end summary.
- **Finish-line ops fail closed.** `review approve`, `review delete`, `cleanup` get a confirm gate + count threshold, mirroring `confirm_purge`. AND a preflight-complete-or-abort gate: if ANY targeted org's PR discovery errors, abort the whole batch before a single GitHub write (no partial merge/delete reported as success).
- **Finish-line ops are correct.** `review approve` refuses/warns on a not-mergeable PR; `review delete` states the unmerged destruction truthfully in its consent prompt (it legitimately abandons open PRs); `cleanup` proves merge with `git merge-base --is-ancestor` against a freshly-fetched remote base ref before `-D`.
- **Tests bite** on every new gate (these paths are currently untested), and the flaky flock-test family is fixed.

### Non-Goals

- **`GH_PERSONA` mixed-fleet warning.** Parked (addendum). The owner will never run across `scottidler` (home) + `escote-tatari` (work) personas in one invocation, so the whole-run-override footgun cannot occur in practice.
- **SSH persona-awareness** (per-org key selection in gx's clone/push). Parked (addendum). Rationale: a wrong/mismatched SSH key fails closed at GitHub (a rejected clone/push, never a wrong-identity write), so the asymmetry with the now-per-org token side is a consistency nit, not a safety hole. (Note: the persona-auth doc's motivation cited mixed-persona single runs; the owner has since clarified that is not their usage, but the SSH parking does NOT rest on that - it rests on fail-closed-at-GitHub, which holds regardless.)
- No redesign of the `create/undo/rollback` spine. It is solid; this doc adds guards around the finish-line and reporting, nothing more.
- No new `--dry-run` on the opt-in destructive flags (per `cli.md`); the gate is the confirm prompt + explicit flag, not a preview.

## Proposed Solution

### Overview

Five focused phases, deterministic/cheap first. Each harvests an existing in-tree pattern rather than inventing:
- Reporting + panic + exit code (Phase 1) is mechanical.
- The subprocess timeout runner (Phase 2) is harvested verbatim from `propose.rs`.
- The confirm gate (Phase 3) is harvested from `confirm_purge`/`confirm_blast_radius`.
- The correctness guards (Phase 4) depend on a Phase 0 spike proving `gh` exposes mergeable/CI state.

### Architecture

- **Shared subprocess runner.** One helper `run_checked(cmd, timeout) -> Result<Output>` becomes the single chokepoint every `git`/`gh` call routes through: spawn in own process group, poll `try_wait` to the deadline, `kill -KILL -<pgid>` + reap on expiry, drain stdout/stderr. Modeled on `propose.rs:439-502`. Timeout is a config field (default const), per `general.md` (tunable via the standard delivery path, never hardcoded at the call site).
- **Shared confirm gate.** One `confirm_destructive(kind, count) -> Result<bool>` helper (generalize `confirm_purge`): prints the blast radius, prompts on a TTY, honors `--yes`, and FAILS CLOSED (loud error naming `--yes`) on non-interactive stdin. `review approve`, `review delete`, `cleanup` all call it. Threshold is a config field mirroring `create`'s `confirm-threshold`.
- **PR readiness fields.** `PrInfo` (`github.rs:359`) gains `mergeable` and `review_decision`/`status_check_rollup` (names TBD by Phase 0), populated from the existing GraphQL query. `review approve` consults them.
- **Panic surfacing.** A `std::panic::set_hook` installed in `main` logs the panic (thread, location, message) at ERROR before the process aborts. This is the fix; rayon already re-raises the panic (abort 101), so the hook just guarantees a diagnostic line. The `Mutex<Vec<_>>` poison-recovery is cheap belt-and-suspenders, not the mechanism.
- **Preflight gate for finish-line batches.** `review approve`/`delete` resolve every targeted org's PRs FIRST; if any discovery errors, abort the whole batch with a loud error naming the failed org before any `gh merge`/`close`/branch-delete. No warn-and-continue over a partial set.

### Data Model

- `PrInfo` gains: `mergeable: Mergeable` (enum `Mergeable { Mergeable, Conflicting, Unknown }`), and a CI/review readiness field shaped by Phase 0. Modeled as enums, not strings (`rust.md`).
- **New `RepoChangeStatus::Skipped { reason }` variant** (`src/state.rs`). Today a `ReviewResult` with `error == None` is recorded as merged (`review.rs:360`); a PR skipped for `mergeable: UNKNOWN` is neither merged nor an error, so it needs a distinct outcome or it would be mis-recorded as merged. `review approve` records skips as `Skipped`, and the end summary counts them separately.
- **Config additions - EXACT schema (pinned; kebab-case, `#[serde(default, deny_unknown_fields)]`, accessor + default const each, mirroring `create.confirm-threshold` / `pr_body_template()`):**
  - Top-level `Config.subprocess_timeout_secs: Option<u64>` -> YAML `subprocess-timeout-secs`; const `DEFAULT_SUBPROCESS_TIMEOUT_SECS: u64 = 300`; accessor `Config::subprocess_timeout() -> Duration`. One field (applies to all git AND gh calls), NOT a `timeouts:` block, NOT split network/local (see Alternatives).
  - New `ReviewConfig { confirm_threshold: Option<usize> }` -> YAML `review.confirm-threshold`; new `CleanupConfig { confirm_threshold: Option<usize> }` -> YAML `cleanup.confirm-threshold`; both default const `5` (reuse/mirror `DEFAULT_CONFIRM_THRESHOLD`). Add `review: Option<ReviewConfig>` and `cleanup: Option<CleanupConfig>` to `Config`, each with an accessor returning the effective threshold.
  - `gx.yml` gains commented example blocks for all three.
- A `RunReport` failure-summary type: `Vec<{ repo: String, phase: String, error: String }>`. Written to a FILE via `gx create --report <path>` as JSON (serde). stdout streaming is UNCHANGED (the full TTY-detect stdout json/yaml contract is deferred - see Alternatives).

### API Design

```rust
// Phase 2: the single subprocess chokepoint (harvested from propose.rs)
fn run_checked(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output>;
// on expiry: Err naming the command + timeout; child's process group killed + reaped.

// Phase 3: the single confirm gate (generalized from confirm_purge)
enum DestructiveOp { ReviewApprove, ReviewDelete, Cleanup }
fn confirm_destructive(op: DestructiveOp, count: usize, assume_yes: bool) -> Result<bool>;
// TTY -> prompt; --yes -> true; non-interactive && !yes -> Err naming --yes (fail closed).

// Phase 4: readiness consulted before a merge
fn is_mergeable(pr: &PrInfo) -> bool;   // Conflicting/Unknown -> false (fail closed)
```

### Implementation Plan

#### Phase 0: Prove `gh` exposes PR mergeability + CI state (zero code)
**Model:** sonnet
- Run the GraphQL/`gh pr view --json` query gx already uses, extended with `mergeable`, `mergeStateStatus`, `statusCheckRollup`, `reviewDecision`, against a real open PR. Confirm which fields are populated and how `mergeable: UNKNOWN` behaves (GitHub computes it lazily; a fresh PR returns UNKNOWN until the merge commit is enqueued).
- **Success criteria:** a documented sample response showing the field names + value domains gx will consume; a decided policy for `UNKNOWN` (fail-closed: treat as not-mergeable and warn). If a field is NOT queryable via gx's current `gh` path, that is recorded as a Phase 4 constraint before any code.

#### Phase 1: Airtight, scriptable reporting
**Model:** sonnet
- `gx create` returns/exits non-zero when any repo result carries an error (mirror `status.rs:138`); wire through `main.rs` exit mapping.
- Install `std::panic::set_hook` in `main` logging thread + location + message at ERROR. This is the PRIMARY panic-surfacing fix: rayon already propagates a worker panic out of `par_iter().for_each`/`collect`, so a panic aborts loudly rather than being swallowed - the hook guarantees a diagnostic line before it does.
- Belt-and-suspenders: replace `Mutex<Vec<_>>::into_inner().unwrap_or_default()` (`status.rs:133`, `checkout.rs:144`, `clone.rs:83`) with poison-recovery (`unwrap_or_else(|e| e.into_inner())`) and the `if let Ok(mut v) = results.lock()` push sites likewise, so that IF a panic is ever contained (a future `catch_unwind`), partial results are recovered instead of blanked to an empty vec. Not the main path (the hook is), but it removes the fail-open shape.
- Add `gx create --report <path>`: writes the `RunReport` (per-repo `{repo, phase, error}` list) to that FILE as JSON. The on-screen human summary is unchanged; scriptability comes from the exit code + this file, NOT from reshaping stdout (that larger stdout-contract change is deferred, see Alternatives).
- **Success criteria:** `gx create` with one repo forced to fail exits non-zero AND prints the failing repo in the on-screen summary; `--report <path>` produces a file that parses as JSON and lists that failure; a deliberately panicking worker produces an ERROR log line (panic hook) rather than a bare abort.

#### Phase 2: Subprocess timeout + process-group kill
**Model:** opus
- Add `run_checked(cmd, timeout)` harvested from `propose.rs:439-502` (own process group, poll `try_wait`, `kill -KILL -<pgid>` + reap on expiry, drain both pipes). Route ALL `git`/`gh` `Command::output()` calls in `git.rs`/`github.rs` through it.
- **Null the child's stdin** (`Stdio::null()`): a `gh` waiting on an auth/interactive prompt or a `git` credential prompt is itself a wedge; a closed stdin makes it fail fast instead of blocking forever.
- Timeout is a config field (default const). A timed-out op returns a per-repo error (isolated, reported), never a wedge.
- **Success criteria:** a git/gh invocation rigged to hang past the timeout is killed and surfaces a per-repo timeout error while sibling repos complete and the run reaches its summary; a git/gh op that would prompt on stdin fails fast rather than hanging; `rg 'Command::new\("(git|gh)"\)' src/` shows every call flows through `run_checked` (no raw `.output()` on git/gh remains).

#### Phase 3: Confirm gate + threshold on finish-line ops
**Model:** opus
- Generalize `confirm_purge` into `confirm_destructive(op, count, assume_yes)`. Apply to `review approve`, `review delete`, `cleanup`: TTY prompt showing blast radius (op + count + org breakdown), `--yes` bypass, FAIL CLOSED on non-interactive stdin without `--yes`.
- **Preflight-complete-or-abort** (closes the fail-OPEN hole at `review.rs:305-315`/`:430-441`): `review approve`/`delete` must resolve PR discovery for EVERY targeted org before any mutation; if any org's `list_prs_by_change_id` errors, abort the whole batch with a loud error naming the failed org - do NOT warn-and-continue over the partial set. The confirm prompt shows the count only after discovery is proven complete.
- Add `--yes` to `cleanup` (it currently has none) and `review approve`/`delete`. Wire the pinned `review.confirm-threshold` / `cleanup.confirm-threshold` config.
- **Success criteria:** each of the three ops on non-interactive stdin without `--yes` exits with a loud error naming `--yes` and performs ZERO GitHub/git mutations (assert via a spy/fake); with `--yes` it proceeds; a `review approve`/`delete` run where one org's discovery is forced to error performs ZERO mutations on the other orgs (aborts before any write).

#### Phase 4: Correctness guards
**Model:** opus
- Wire the Phase 0 fields into `PrInfo` + the GraphQL query. `review approve` skips any PR that is not `Mergeable` (Conflicting/Unknown -> skip). **Approve-vs-`--admin` (amended 2026-07-12, see Resolved Decisions):** on the NON-admin path, a failed `gh pr review --approve` aborts THAT merge instead of proceeding (fail closed). On the `--admin` path, gx SKIPS `gh pr review --approve` entirely and merges via `gh pr merge --admin` - GitHub categorically rejects self-approval ("Can not approve your own pull request"), and gx's primary workflow is a single SRE landing their OWN campaign, so requiring a self-approval that always fails would make the documented merge-regardless override unreachable. The confirm gate and mergeability skip still apply on both paths.
  - **`mergeable: UNKNOWN` batch reality + state encoding:** GitHub computes mergeability lazily, so freshly-opened PRs return UNKNOWN until the merge commit is enqueued. Failing closed means a batch approve right after `create` may skip most PRs. A skipped PR is recorded as the new `RepoChangeStatus::Skipped { reason }` (NOT merged, NOT error - see Data Model), and the end summary states "N PRs skipped (mergeability not yet computed) - re-run `review approve`." No silent merge on uncertainty; no implicit poll/wait (decided: re-run over wait).
- `review delete` (its purpose IS abandoning a campaign's OPEN PRs - close + delete their branches): keep that behavior. The guard is the Phase 3 confirm gate, whose prompt MUST state the unmerged destruction explicitly - e.g. "will CLOSE N open (unmerged) PRs and DELETE their branches" with the count. Do NOT flip the default to merged-only (that defeats the command). Correctness here = the prompt tells the truth about what is destroyed; consent is the gate.
- `cleanup`: before `git branch -D`, resolve the repo's default base branch and **fetch `origin/<base>` first** (the ancestry test against a stale local `main` is worse than useless), then run `git merge-base --is-ancestor <branch> origin/<base>`. Base-branch NAME source: reuse `resolve_base_branch`'s logic (`create/core.rs:1335`) - state carries `base_sha` but not the name (`state.rs:67`), so resolve the name, don't assume `main`. If the branch is NOT an ancestor, skip + warn unless `--force`. Keep `PrMerged` as a fast-path signal but the fetched-ancestry check is the real guard.
- **Success criteria:** `review approve` does not merge a PR whose `mergeable != Mergeable` (test with a fake) and records it as `Skipped`, not merged; the summary reports UNKNOWN skips with a re-run hint; `review delete`'s confirm prompt names the unmerged-PR count it will close; `cleanup` without `--force` preserves a branch with a commit absent from the freshly-fetched base ref (assert the branch still exists).

#### Phase 5: Tests that bite + flock-test fix + docs
**Model:** sonnet
- Add tests for every gate in Phases 1, 3, 4 (break-the-guard: assert the mutation is blocked). Cover the `review` mutating paths (currently only token/GraphQL parsing is tested).
- Fix the flaky flock-test family (`lock`/`state`/`review`/`create`): widen/gate the load-sensitive timing assertions and serialize env-touching tests behind the shared lock. Keep the logical assertions (exactly-one-winner, holder-pid-named, reacquire-after-SIGKILL); only the wall-clock margins move (two audits found the flakiness is timing/harness, not a production race).
- Update `docs/subcommands.md` and any command docs for the new gates/flags (`--yes`, `--report`, the thresholds).
- **Success criteria:** `otto cov`/`otto ci` passes **5 consecutive runs at default test parallelism** (`otto ci; echo $?` -> 0, x5); each Phase 1/3/4 guard has a test that fails if the guard is removed (break-the-guard).

## Acceptance Criteria

- [ ] `gx create` with a forced per-repo failure exits non-zero and lists the failing repo; `gx create --report <path>` writes a file that parses as JSON and names that failure.
- [ ] A `git`/`gh` op rigged to hang past the timeout is killed; the owning repo reports a timeout error and the run still completes its summary.
- [ ] `review approve`, `review delete`, and `cleanup` each fail closed (loud error naming `--yes`, zero mutations) on non-interactive stdin without `--yes`; and `review approve`/`delete` abort with ZERO mutations if any targeted org's PR discovery errors (preflight-complete-or-abort).
- [ ] `review approve` does not merge a non-mergeable PR and records an UNKNOWN skip as `Skipped` (not merged); `cleanup` without `--force` does not `-D` a branch with commits absent from the freshly-fetched base ref.
- [ ] `otto ci` passes 5 consecutive runs at default test parallelism; every new gate has a bite test.

## Resolved Decisions

- **2026-07-12 | `--admin` exempts the self-approve step (Phase 4 amendment).** Surfaced during Phase 4 execution: gx's `review approve` always ran `gh pr review --approve` unconditionally, and Phase 4's new abort-on-failed-approve guard fired BEFORE the `--admin` merge was reached, so a single SRE landing their OWN campaign PRs (self-approval is categorically rejected by GitHub) could no longer merge even with `--admin`. Decision (owner-approved after a review-panel consensus - Architect + Staff Engineer both unanimously chose this): on the `--admin` path, SKIP `gh pr review --approve` and merge via `gh pr merge --admin`; the abort-on-failed-approve guard applies only to the non-admin path. Rejected: string-matching GitHub's "Can not approve your own pull request" as non-fatal (banned by `rust.md`'s "match a typed error variant, never `msg.contains(...)`" and `taste.md`'s "magic that can't be made predictable and tested gets ripped out"); and keeping doc-as-written (makes `review approve` unusable for own PRs). Acceptable failure mode: an `--admin` merge lands with no approval record - structurally unavoidable for a self-authored PR, and the real batch-damage guards (discovery preflight, mergeability skip, confirm threshold) are upstream and untouched.
- **2026-07-12 | Non-goals parked, not built.** `GH_PERSONA` mixed-fleet warning and SSH persona-awareness are out of scope because the owner never crosses the home/work persona boundary in a single invocation. Recorded in the Addendum so they are not re-litigated.
- **2026-07-12 | No `--dry-run` on the destructive flags.** Per `cli.md`; the confirm gate + explicit flag is the opt-in. Recovery for `create`/`undo` already rides the recovery journal.
- **2026-07-12 | `mergeable: UNKNOWN` fails closed.** Treat unknown mergeability as not-mergeable (record `Skipped`, warn), never merge on uncertainty. No implicit poll/wait; the operator re-runs. (Panel wanted a 2s x 15s poll; declined - re-run is simpler and the skip is recorded distinctly.)
- **2026-07-12 | Panel findings folded (Design Review).** MUST-FIX (all folded): output contract resolved to `--report <path>` file, not a stdout refactor (#1); config schema pinned exactly (#2); partial-discovery fail-OPEN closed by preflight-complete-or-abort (#4); `Skipped` state variant added (#5); cleanup ancestry check fetches `origin/<base>` and resolves the base name (#6). CHEAP-WIN (folded): panic/mutex Problem-Statement wording corrected (#3); Goals `review delete` wording fixed + SSH parking re-rationaled to fail-closed-at-GitHub (#7); Phase 5 success made falsifiable (5 consecutive runs) (#8). DEFERRED (recorded, not built): split network/local timeouts (#9), `Stdio::null` interactive-prompt concern (#10), UNKNOWN poll loop (#11) - see Alternatives/Addendum.
- **2026-07-12 | Mutex belt-and-suspenders kept.** Reviewers split (Architect: drop as dead code; Staff: keep). Kept: cheap, removes a fail-open shape if a future `catch_unwind` contains a panic. The panic hook is the primary fix.

## Alternatives Considered

### Alternative 1: A single global `--yes`/`--force` and no per-op threshold
- **Why not chosen:** the finish-line ops differ in blast radius (merge vs delete-unmerged vs local `-D`); one flag can't express "prompt above N" per op. Mirror `create`'s existing per-op threshold instead.

### Alternative 2: Add timeouts only to the network ops (`git fetch`/`pull`, `gh`)
- **Why not chosen:** picking "which git calls are network" is a guess that rots; routing ALL git/gh through one `run_checked` chokepoint is simpler and can't miss a call. A local git op with a generous timeout is harmless.

### Alternative 3: Ship Phase 1 only as a targeted fix, skip the doc
- **Why not chosen:** the owner asked to formalize the whole batch through the funnel. Phase 1 alone leaves the irreversible finish-line ops ungated, which is the higher risk.

### Alternative 4: Split network vs local subprocess timeouts (panel #9, deferred)
- **Why not chosen (now):** two timeout classes add config surface for little gain. `Stdio::null()` already makes auth/credential-prompt hangs fail fast, so a single GENEROUS timeout only lengthens the rare genuinely-wedged network op - acceptable. Revisit only if a legitimate long local op (huge repo) trips the single timeout in practice.

### Alternative 5: Full TTY-detect stdout contract (json when piped / yaml for humans) (panel #1, deferred)
- **Why not chosen (now):** `create.rs` streams ~20+ human `println!`s to stdout; a clean piped-JSON contract means routing all human progress to stderr plus a `--format` override - a real refactor beyond the owner's ask ("report failures at the end, scriptably"). `--report <path>` delivers the scriptable artifact without it. Deferred as the honest next step IF full machine-readable stdout is later wanted.

### Alternative 6: Poll/wait on `mergeable: UNKNOWN` (panel #11, deferred)
- **Why not chosen:** a 2s x 15s retry adds latency to every batch approve for a state the operator can resolve by re-running once GitHub finishes computing. The recorded `Skipped` outcome + re-run hint is simpler and honest.

## Technical Considerations

### Dependencies
- No new crates expected. `run_checked` uses `std::process` + a poll loop (as `propose.rs` does). Confirm whether `propose.rs` uses a helper crate for `try_wait`/process-group; reuse it if so.

### Security
- `--admin` on `review approve` (bypasses branch protection) stays available but now sits behind the confirm gate. No token handling changes (v0.6.0 auth is unchanged).

### Testing Strategy
- Fakes/spies for the GitHub-mutating paths (no live calls in unit tests); assert zero mutations on the fail-closed paths. Break-the-guard tests per `taste.md`. The flock-test fix is verified by repeated `otto ci` under load.

### Rollout Plan
- Single repo (gx). `gx-mcp` inherits behavior via the lib. Ship order: this doc -> phases -> `otto ci` -> implementation audit -> `/cli-shakedown` -> bump -> live validation of a gated `review`/`cleanup` run against `gx-testing`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `gh` does not expose a needed CI/mergeable field via gx's path | Med | Med | Phase 0 spike proves it BEFORE Phase 4 code; fall back to `mergeable` only if CI rollup is unavailable |
| `run_checked` process-group kill breaks a git op that spawns helpers (credential/ssh) | Low | Med | Test git+ssh clone/push through `run_checked`; generous default timeout |
| Adding a confirm gate breaks existing non-interactive automation the owner runs | Med | Low | `--yes` is the documented bypass; call it out in Phase 5 docs |
| Flock-test fix masks a real race instead of fixing timing | Low | Med | Two audits independently found it is timing/harness, not a production race; keep the logical assertions, only widen the wall-clock margins |

## Open Questions
- (none)

## Addendum: Parked (do NOT re-litigate)

- **`GH_PERSONA` mixed-fleet warning.** A whole-run `GH_PERSONA=work` mis-auths every home repo in a mixed campaign. Parked: the owner never mixes personas in one run. Revisit only if that usage changes.
- **SSH persona-awareness.** gx's clone/push uses the ambient `~/.ssh/config` (global `core.sshCommand`, single `git@github.com` host), not a per-org key like the token side. Parked because a wrong/mismatched key FAILS CLOSED at GitHub (rejected clone/push, never a wrong-identity write) - a consistency nit with the token side, not a safety hole. Revisit only if the asymmetry becomes an ergonomic problem.
- **`Stdio::null()` breaking a genuinely interactive git/gh prompt** (panel #10). Parked: gx is a batch tool for a single SRE on ssh-agent + `GIT_SSH_COMMAND` per-org keys (`secrets.md`); an interactive credential prompt is not the owner's path, and nulling stdin is what converts a prompt-hang into a fast failure. The risk table carries the clone/push-through-`run_checked` test.

## References
- Four-dimension audit (this session): secrets/auth, destructive-ops, concurrency, maturity.
- `src/create/core/propose.rs:439-502` - the process-group-kill subprocess pattern to harvest.
- `src/review.rs:785` `confirm_purge` - the fail-closed confirm gate to generalize.
- `src/github.rs:359` `PrInfo`; `:602-662` approve/merge; `:665,683` close/delete-branch.
- `src/cleanup.rs:255` `PrMerged` gate; `src/git.rs:1037-1047` `delete_local_branch` (`git branch -D`).
- `docs/design/2026-07-12-persona-aware-github-auth.md` - the v0.6.0 auth model (unchanged here).
