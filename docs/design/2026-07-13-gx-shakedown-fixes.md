# Design Document: gx Shakedown Fixes (cleanup/squash + --pr parse)

**Author:** Scott Idler
**Date:** 2026-07-13
**Status:** Draft
**Review Passes Completed:** 5/5
**Review Panel:** Design Review complete (2026-07-13) -- Architect (Gemini) + Staff Engineer (Codex), both CONFIRMED Resolved Decisions 1 and 2; six findings folded in below.

## Summary

Live-fire shakedown of gx v0.6.1 (a real `create -> review approve -> cleanup` campaign across 6 throwaway `scottidler/*` repos) surfaced two bugs the test suite missed. Bug 1 (significant): `cleanup`'s Phase-4 merge-proof uses `git merge-base --is-ancestor`, which is a commit-identity test and returns false for every squash-merged branch -- and `gx review approve` merges with `--squash`. So the normal cleanup path skips every branch gx itself merged; the only way through is `--force`, which bypasses the guard entirely. Bug 2 (minor): `--pr` is an optional-value flag that greedily swallows the following `create` subcommand token (`--pr regex ...` fails to parse). This doc fixes both, closing the test gaps that let them ship.

## Problem Statement

### Background

- gx v0.6.1 shipped the production-hardening work (`docs/design/2026-07-12-gx-production-hardening.md`), including a Phase-4 `cleanup` guard that proves a branch is merged before `git branch -D`.
- Both bugs were found by exercising the tool live, not by tests. Both root causes are verified against the code and reproduced at runtime.

### Problem

**Bug 1 -- cleanup ancestry guard is wrong for squash merges.**
- `gx review approve` merges with `gh pr merge --squash` (`src/github.rs:706`). A squash merge writes ONE new commit onto the base; the branch's own commits never enter base history.
- The Phase-4 guard `branch_merged_into_base` proves merge with `git merge-base --is-ancestor <branch> origin/<base>` (`src/git.rs:1105-1116`), invoked from `src/cleanup.rs:361-387`. `--is-ancestor` is a commit-identity test: it asks "is the branch tip a literal ancestor commit of base?" For a squash merge the answer is always NO.
- Result: `cleanup` (without `--force`) skips every branch from gx's own merge flow. Runtime-proven on the live campaign: `cleanup --yes` -> `0 cleaned / 5 skipped`; `cleanup --yes --force` -> `5 cleaned`. It fails safe (never wrongly deletes) but the happy path is non-functional, and `--force` -- the only way through -- disables the very guard Phase 4 added.
- `PrMerged` is the eligibility gate (`cleanup.rs:308`); the ancestry check is the authoritative proof on top of it (`cleanup.rs:361-367` comment: *"PrMerged stays a fast-path signal, but the git-level ancestry check is the real guard against deleting unmerged work"*).

**Bug 2 -- `--pr` optional-value flag swallows the subcommand.**
- `--pr` is defined `Option<PR>` with `default_missing_value="normal"` + `num_args=0..=1` at `src/cli.rs:315-322` (Create) and `:357-364` (Apply). The space form `--pr regex ...` lets clap bind `regex` (a `CreateAction` subcommand, `cli.rs:619-642`) as the flag's value, so the next positional is misread as the subcommand. Reproduced: `--pr regex '<pat>' '<rep>'` -> `error: unrecognized subcommand '"@acme/shared": "[^"]+"'`. The documented `--help` example `--pr delete` (`cli.rs:283`) has the same latent flaw.

### Goals

- **Cleanup works for gx's own merges.** After a normal `create -> review approve` (squash) campaign, `gx cleanup <id>` WITHOUT `--force` cleans the merged branches.
- **The anti-divergence guarantee Phase 4 added is preserved.** A branch whose changes are NOT actually in the base is still refused by `cleanup` without `--force`. The fix must not reopen the hole Phase 4 closed.
- **`--pr <subcommand>` parses.** `gx create ... --pr regex 'a' 'b'` runs the regex subcommand; draft PRs still reachable.
- **The bugs cannot recur.** A positive squash-merge cleanup test and a `--pr` parse test, each break-the-guard.

### Non-Goals

- No change to the merge STRATEGY. gx keeps `--squash` (it is the right default for campaign PRs). This doc fixes cleanup's DETECTION of a squash merge, not the merge itself.
- No redesign of the create/undo/rollback spine or the other Phase 1-5 guards. They are proven live (create/approve/cleanup all fail closed without `--yes`; `--admin` self-approve exemption works).
- No new `--dry-run` on the destructive flags (per `cli.md`).
- Not touching the cross-persona / mixed-org question -- out of scope, owner never mixes personas in one run (settled).

## Proposed Solution

### Overview

Three focused phases after a zero-code spike, cheap/deterministic first:
- Phase 0 (spike): prove the git-level squash-detection primitive on a real squash merge.
- Phase 1 (Bug 2): make `--pr` a boolean + add `--draft`, killing the optional-value ambiguity class.
- Phase 2 (Bug 1): replace the `--is-ancestor` proof with a patch-identity (content) check that handles squash while still vetoing genuinely-unmerged branches.
- Phase 3: bite tests for both + docs.

### Architecture

**Bug 1 -- patch-identity, not commit-identity (and NOT a fresh GitHub query).**
- The right git primitive is `git cherry origin/<base> <branch>`: it lists each branch commit with `-` (its patch is already in base, by patch-id) or `+` (its patch is NOT in base). A branch is safe to delete iff there are ZERO `+` lines (all its changes are in base).
- This handles a squash merge of gx's branches: gx create writes exactly ONE commit per repo (the change + `--commit` message), so the squashed commit's diff equals that single commit's diff -> matching patch-id -> `-` line -> detected as merged. It also still preserves a branch with a local commit whose changes are absent from base -> `+` line -> refused. That is the Phase-4 anti-divergence guarantee, kept.
- **Why NOT a fresh GitHub `mergedAt` query (rejected -- see Alternatives):** it would report "merged" for the PR while a locally-diverged branch tip (an extra commit never in the PR) gets deleted -- reopening the exact local-divergence risk Phase 4 closed. The guard must be git-level to see local reality. GitHub's merged flag is about the PR, not the local branch.
- Scope of change: `branch_merged_into_base` (`src/git.rs:1079-1132`) keeps its base-name resolution (`get_head_branch`, `:1084`) and the `fetch_origin` (`:1097`); ONLY the proof step (`:1105-1116`) changes from `--is-ancestor` to the `git cherry` check. `cleanup.rs:361-387` and the `PrMerged` eligibility gate (`:308`) and `--force` bypass are unchanged.

**Bug 2 -- boolean `--pr` + `--draft`.**
- Replace `pr: Option<PR>` (optional value) with `pr: bool` and add `draft: bool` at both `cli.rs:315-322` (Create) and `:357-364` (Apply). `gx create ... --pr` = normal PR; `gx create ... --pr --draft` = draft PR. No optional value -> no token to swallow -> `--pr regex ...` parses `regex` as the subcommand.
- Downstream consumers that read `Option<&PR>` / `matches!(pr, Some(PR::Draft))` (`create.rs:127,335,429`; `create/core.rs:142,239,257,310,834,1343-1348`) switch to the two booleans. `--help` examples updated (`cli.rs:283-284,351`).

### Data Model

- No persisted-state changes. `RepoChangeStatus` / `PrMerged` unchanged. The `PR` enum (`cli.rs:6-14`) is removed (folded into two booleans) -- a CLI-surface change only.

### API Design

```rust
// Phase 2: the merged-proof primitive (replaces --is-ancestor in branch_merged_into_base)
// Returns Ok(true) iff every commit on `branch` has its patch already present in `base_ref`
// (i.e. `git cherry <base_ref> <branch>` emits no `+` lines). Squash-merge of a
// single-commit branch -> true; a branch with a change absent from base -> false.
//
// FAIL-CLOSED exit-status contract (review finding #3): `run_checked` returns Ok on a
// non-zero exit (`subprocess.rs:82-88`), and `git cherry` exits 0 whether or not `+`
// lines exist -- so a fatal cherry (bad ref, empty stdout) would naively read as
// "no + lines -> merged -> delete". The primitive MUST require a SUCCESS exit before
// interpreting stdout; any non-zero/error status -> Err -> cleanup preserves the branch.
// This mirrors how the existing --is-ancestor guard maps exit codes (`git.rs:1118-1130`).
fn branch_changes_in_base(repo: &Path, base_ref: &str, branch: &str) -> Result<bool>;

// Phase 1: cli.rs
// before: pr: Option<PR>  (optional-value, swallows subcommand)
// after (review finding #1 -- --draft must fail LOUD, not silently no-op):
#[arg(long)] pr: bool,                       // create a PR
#[arg(long, requires = "pr")] draft: bool,   // when --pr, make it a draft; --draft alone is a clap error
```

### Implementation Plan

#### Phase 0: Prove the squash-detection primitive (zero code)
**Model:** sonnet
- In a temp git repo: create a base, branch with ONE commit, `git merge --squash` + commit onto base (new SHA), then run `git cherry <base> <branch>` and confirm it emits NO `+` lines (all `-`). Then on a second branch with a commit whose change is NOT on base, confirm `git cherry` emits a `+` line. Compare against `git merge-base --is-ancestor` on the same two branches to document that `--is-ancestor` is false for the squash-merged one (the bug) while `git cherry` is correct.
- **Success criteria:** documented transcript showing `git cherry` returns empty-of-`+` for the squash-merged single-commit branch AND a `+` line for the unmerged branch; `--is-ancestor` shown false for the squash case. If `git cherry` does NOT cleanly distinguish them, record it as a Phase-2 constraint before any code.

#### Phase 1: Fix `--pr` parsing (Bug 2)
**Model:** sonnet
- Replace `pr: Option<PR>` with `pr: bool` + add `draft: bool` with `requires = "pr"` at `cli.rs:315-322` (Create) and `:357-364` (Apply); remove the `PR` enum (`cli.rs:6-14`). The `requires = "pr"` makes a bare `--draft` a clap error rather than a silent no-op (review finding #1 -- fail loud).
- **Complete downstream `PR`/`Option<PR>` consumer inventory (review finding #2 -- the doc's original list was incomplete; the compiler enforces exhaustiveness but every site is named here so none is missed):**
  - `create.rs:127,335,429`
  - `create/core.rs:142,239,257,310,834,1343-1348`
  - **`create/core/apply.rs:52`** (`pr: Option<&crate::cli::PR>`) -- the apply path
  - **`github.rs:177`** (`pr: &crate::cli::PR`) and **`:202-203`** (`matches!(pr, crate::cli::PR::Draft)`) -- this is the actual gh `--draft` wiring; both reviewers flagged it as the most important miss
  - **`main.rs:157,188`** (Create `pr.clone()`) and **`:196-197`** (Apply `pr.as_ref()`) -- the CLI destructure / pass-through
  - **`gx-mcp/src/logic.rs`** (cross-crate consumer of the apply API; confirm exact lines during Phase 1)
- Update `--help` examples (`cli.rs:283-284,351`).
- **Success criteria (now covering BOTH Create and Apply -- review finding #5):**
  - `gx create --files x --commit m --pr regex 'a' 'b'` parses `regex` as the subcommand (no "unrecognized subcommand" error)
  - `gx create ... --pr --draft sub 'a' 'b'` creates a DRAFT PR; `gx create ... --pr add f c` creates a normal PR
  - `gx apply <id> --pr --draft` produces a DRAFT PR
  - `gx create ... --draft` (no `--pr`) and `gx apply <id> --draft` (no `--pr`) each FAIL with a clap error (not a silent no-op)

#### Phase 2: Fix cleanup squash detection (Bug 1)
**Model:** opus
- Add `branch_changes_in_base` (the `git cherry` patch-identity check) in `src/git.rs` beside `branch_merged_into_base`; route it through Phase-2-of-hardening `run_checked`. Replace the `--is-ancestor` proof (`git.rs:1105-1116`) with it. Keep base-name resolution (`get_head_branch`), the `fetch_origin` of the base, the `PrMerged` eligibility gate (`cleanup.rs:308`), and the `--force` bypass unchanged.
- **FAIL-CLOSED exit-status handling (review finding #3, correctness-critical):** because `run_checked` returns `Ok` on a non-zero exit and `git cherry` exits 0 regardless of `+`-line presence, `branch_changes_in_base` MUST require a success exit before counting `+` lines. On any non-zero/error status it returns `Err`, which propagates so cleanup PRESERVES the branch (never deletes on an ambiguous cherry). This matches the exit-code mapping the current `--is-ancestor` guard already does at `git.rs:1118-1130`.
- **Success criteria:** a squash-merged single-commit branch is cleaned by `cleanup_change(force=false)` (`repos_cleaned == N`, branch gone); a branch with a commit whose change is absent from the freshly-fetched base is still preserved without `--force` (the existing negative test still passes); a `git cherry` that exits non-zero causes the branch to be preserved (fail closed), not deleted; `--force` still bypasses the check.

#### Phase 3: Tests that bite + docs
**Model:** sonnet
- Add the POSITIVE squash-merge cleanup bite test (`src/cleanup.rs` tests, matching the existing inline `mod tests` location per Phase 3/4 precedent): init upstream base, branch + one commit, `git merge --squash` + commit on base (new SHA), push, assert `cleanup_change(force=false)` returns `repos_cleaned == 1` and the branch is gone. Against pre-fix code this test FAILS (proves the bug).
- Add a `--pr` clap parse test: `--pr <subcommand>` resolves the subcommand; `--pr --draft` yields draft; a normal `--pr` is not draft; and (review finding #1/#5) a bare `--draft` with no `--pr` returns a clap error, on BOTH the Create and Apply parse surfaces.
- Add a Phase-2 fail-closed test (review finding #3): `branch_changes_in_base` against a bad/non-existent base ref returns `Err` (so cleanup preserves), proving a fatal `git cherry` is never read as "merged".
- Keep the existing negative test `test_cleanup_preserves_branch_with_commits_absent_from_base`.
- Update `docs/subcommands.md` for the `--pr`/`--draft` change.
- **Success criteria:** `otto ci` green; each new test fails if its fix is reverted (break-the-guard); the existing negative cleanup test still passes.

## Acceptance Criteria

- [ ] `gx create --files x --commit m --pr regex 'a' 'b'` parses without error and runs the regex subcommand; `--pr --draft` produces a draft PR.
- [ ] `gx apply <id> --pr --draft` produces a draft PR; a bare `--draft` (no `--pr`) on BOTH `create` and `apply` fails with a clap error, not a silent no-op.
- [ ] After a squash-merged campaign, `gx cleanup <id>` WITHOUT `--force` cleans the merged branches (`repos_cleaned == N`, branches gone).
- [ ] `gx cleanup <id>` WITHOUT `--force` still preserves a branch whose change is absent from the freshly-fetched base (existing negative test passes), AND preserves a branch when `git cherry` exits non-zero (fail closed).
- [ ] `otto ci` passes; the positive squash-merge cleanup test, the `--pr`/`--draft` parse tests, and the fail-closed exit-status test each fail if their fix is reverted.

## Resolved Decisions

- **2026-07-13 | Bug 1 fix is git-level patch-identity (`git cherry`), NOT a fresh GitHub `mergedAt` query.** A fresh-GitHub proof reopens the local-divergence risk Phase 4 closed (PR merged on GitHub, but a locally-diverged branch tip gets deleted). `git cherry` sees local reality and handles squash for gx's single-commit branches. **CONFIRMED by the review panel (Architect + Staff Engineer, 2026-07-13), conditional on the fail-closed exit-status contract (finding #3) and the patch-equivalence edge documentation (finding #4), both now folded in.**
- **2026-07-13 | Bug 2 fix is boolean `--pr` + `--draft`, not `require_equals`.** Kills the optional-value footgun class entirely (also fixes the `--help` `--pr delete` example) and matches `cli.md` "flags not optional-value". Accepts a small interface change (`--pr=draft` -> `--pr --draft`); acceptable pre-1.0 for a single-operator tool. **CONFIRMED by the review panel (2026-07-13), conditional on `--draft` failing loud via clap `requires = "pr"` rather than a silent no-op (finding #1), now folded in.**

## Alternatives Considered

### Alternative 1: cleanup re-queries GitHub for merged state (fix 1a)
- **Description:** reuse `sync_change_state` / `list_prs_by_change_id` to treat `PrState::Merged`/`merged_at` as the merge proof.
- **Why not chosen:** GitHub's merged flag is about the PR, not the local branch. A branch with a local commit never in the merged PR would be reported merged and deleted -- reopening the exact local-divergence hole Phase 4's git-level check was added to close. Also adds a `gh`-token network dependency to cleanup. Recorded because it is the obvious first instinct and must not be re-litigated.

### Alternative 2: `require_equals = true` on the existing `--pr` (fix 2b)
- **Description:** keep `Option<PR>` but require `--pr=<normal|draft>`; the space form can no longer bind the next token.
- **Why not chosen:** leaves the optional-value shape (still a footgun class, e.g. a bare `--pr` at end-of-args), and `cli.md` prefers eliminating optional-value flags. Kept as the low-risk fallback if interface stability of `--pr=draft` is later deemed more important than taste.

### Alternative 3: change the merge strategy away from squash
- **Description:** merge with `--merge` so `--is-ancestor` works.
- **Why not chosen:** squash is the right default for campaign PRs; changing merge semantics to satisfy a detection bug is backwards. Fix the detection.

## Technical Considerations

### Dependencies
- No new crates. `git cherry` via `std::process` through `run_checked` (existing). No new network dependency (cleanup already `fetch`es the base).

### Security
- No token/auth changes. `--force` still bypasses the guard (documented escape hatch); the fix makes `--force` rarely necessary rather than mandatory.

### Testing Strategy
- Break-the-guard for both fixes. The positive squash-merge cleanup test is the one that would have caught Bug 1 (Phase 4 shipped only the negative case). Real temp-repo squash merge in the test, no mocks. `--pr` parse tested at the clap layer.
- `git cherry` is patch-*equivalence* (patch-id), not literal tree containment. "Safe to delete iff no `+` lines" means "every branch commit's patch-id is present in base," which is the correct primitive for gx's single-commit flow.
- Known edges (review finding #4 -- own every direction explicitly, not just the safe ones):
  - **N>1 commits squashed into one base commit** -> combined patch-id != any single patch-id -> `+` lines -> preserved (skip -> `--force`). gx branches are single-commit so this does not arise in gx's own flow. Fail-SAFE (preserve).
  - **GitHub Web-UI commit-suggestion / conflict resolution** modifies the squashed commit -> different patch-id -> `+` line -> preserved. Fail-SAFE (preserve).
  - **Whitespace-only divergence** -> `git cherry` / patch-id IGNORE whitespace, so a local whitespace-only diff from base reads as merged -> DELETE. This is the one **fail-OPEN** direction and the only edge that is not safe-by-default. Acceptable for gx's single-commit flow (a whitespace-only local drift from a merged change is not unmerged work), but owned here explicitly rather than implied safe.
  - Theoretical patch-id collision (distinct diffs, same patch-id): astronomically unlikely; noted for completeness.
- Intentional friction (review finding #6, product-utility note, no design change): an operator who habitually resolves review feedback via GitHub's Web UI will diverge their branch by patch-id, so `cleanup` will keep skipping it and they will reach for `--force`. The fail-safe (preserve) direction is deliberate -- the guard refusing to delete a locally-diverged branch is the Phase-4 guarantee working as designed, not a regression.

### Rollout Plan
- Single repo (gx). gx-mcp inherits via the lib but exposes no mutating cleanup/PR tool by default (read-only MCP surface verified in the shakedown), so no MCP behavior change. Ship order: this doc -> phases -> `otto ci` -> implementation audit -> live re-run of the squash `create -> approve -> cleanup` cycle against a throwaway repo set (the Bug-1 reproduction, now expected to clean without `--force`) -> bump.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `git cherry` misclassifies a squash of a multi-commit branch as unmerged | Low (gx branches are single-commit) | Low (fails safe: preserve + `--force`) | Phase 0 spike documents the single-commit guarantee; note the edge in Testing Strategy |
| Boolean `--pr` change breaks a script using `--pr=draft` | Low (single operator, pre-1.0) | Low | Update `docs/subcommands.md`; call out in the phase; `--pr --draft` is the replacement |
| Downstream `Option<PR>` consumers missed in the refactor | Med | Med (compile error, caught by CI) | Phase 1 success criteria exercise normal + draft paths; compiler enforces exhaustiveness |

## Open Questions
- (none)

## References
- `docs/shakedown-v0.6.1.md` -- the live-fire run that found both bugs.
- `docs/design/2026-07-12-gx-production-hardening.md` -- Phase 4 shipped the ancestry guard (Bug 1 origin).
- `src/git.rs:1079-1132` `branch_merged_into_base` (the `--is-ancestor` proof, `:1105-1116`).
- `src/cleanup.rs:308,361-387` cleanup eligibility + proof; `:582-639` the existing negative test.
- `src/github.rs:706` `--squash` merge; `:528-648` `list_prs_by_change_id`; `src/review.rs:744-790` `sync_change_state`.
- `src/cli.rs:6-14,315-322,357-364,619-642` the `--pr` flag + `PR` enum + `CreateAction`.
