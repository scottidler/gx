# Implementation Notes: gx Production Hardening

Running record of decisions, deviations, tradeoffs, and open questions during
execution of `docs/design/2026-07-12-gx-production-hardening.md`. Append-only.

## Phase 0: Prove `gh` exposes PR mergeability + CI state (zero code)

### Design decisions
- **Verified query path** — ran gx's exact `gh api graphql` search path (the
  `PR_SEARCH_QUERY` shape in `src/github.rs:452`) extended with the four
  candidate fields against real open PRs on `cli/cli` (no open PR exists on
  `scottidler/gx`, and `gx-testing` does not exist; the field domains are
  GitHub-universal, and the goal was to prove gx's `gh api graphql` path exposes
  them, which it does).
- **Field names + value domains confirmed** (documented sample response):
  - `mergeable` (top-level PR enum): `MERGEABLE` | `CONFLICTING` | `UNKNOWN`.
    This is the primary field `is_mergeable(pr)` consumes.
  - `mergeStateStatus`: `BLOCKED` | `CLEAN` | `DIRTY` | `BEHIND` | `UNKNOWN` | etc.
  - `reviewDecision`: `REVIEW_REQUIRED` | `APPROVED` | `CHANGES_REQUESTED` | `null`.
  - CI rollup via `commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }`,
    `state`: `SUCCESS` | `PENDING` | `FAILURE` | `ERROR` | `EXPECTED` | `null`
    (null when a commit has no checks). `statusCheckRollup` is NOT a direct field
    on `PullRequest`; it must be reached through `commits(last: 1)`.
  - Sample (PR cli/cli#13844): `mergeable=MERGEABLE`, `mergeStateStatus=BLOCKED`,
    `reviewDecision=REVIEW_REQUIRED`, rollup `state=SUCCESS`.
- **`UNKNOWN` policy (pinned by the doc, confirmed applicable)** — GitHub computes
  `mergeable` lazily; a freshly-opened PR returns `UNKNOWN` until the merge commit
  is enqueued. Policy: treat `UNKNOWN` (and `CONFLICTING`) as not-mergeable, record
  `Skipped`, warn with a re-run hint. No implicit poll/wait.

### Deviations
- None.

### Tradeoffs
- **`mergeable` as the primary gate vs. CI rollup** — Phase 4 keys the mergeable
  guard on the top-level `mergeable` enum (simplest, always populated once
  computed). `statusCheckRollup.state` and `reviewDecision` are available and MAY
  be surfaced/consulted, but the risk-table fallback ("mergeable only if CI rollup
  is unavailable") is moot: both are queryable via the one path. Phase 4 decides
  final field usage; no field is a blocker.

### Open questions
- None. All four candidate fields are queryable via gx's current `gh` path; no
  Phase 4 constraint recorded.

## Phase 1: Airtight, scriptable reporting

### Design decisions
- **Exit non-zero via direct `std::process::exit`, mirroring `status`/
  `checkout`/`clone`** — `create.rs::process_create_command` — the doc's
  Phase 1 bullet says "wire through `main.rs` exit mapping", but no such
  mapping exists anywhere in this codebase today: `status.rs:138`,
  `checkout.rs:151`, and `clone.rs` all call `std::process::exit(error_count
  .min(255) as i32)` directly from within the command module, before
  returning `Ok(())`. `main.rs`'s only error path is `run()` returning `Err`,
  which prints `Error: {err:#}` and exits 1 - that would collapse every
  distinct error count to a flat 1 and print a duplicate "Error:" line on top
  of the already-displayed unified summary. Implemented at the actual seam
  (direct exit in `create.rs`, same shape as its three siblings) rather than
  inventing new main.rs plumbing this phase didn't ask for - `taste.md`
  "siblings behave identically".
- **`RunReport` lives in `create.rs`, not a shared module** — `create.rs` —
  the Data Model section describes `RunReport` generically, but Phase 1 only
  wires it into `gx create`; no other command consumes it yet. Kept local
  (`RunReportEntry`/`RunReport`/`build_run_report`/`write_run_report`) rather
  than promoting to a shared type prematurely; a future phase can hoist it if
  a second command needs the same shape.
- **`phase` field is the existing `CreateAction`, kebab-cased** —
  `create.rs::phase_label` — `CreateResult.action` (`DryRun`/`Committed`/
  `PrCreated`) already carries exactly "the pipeline stage reached before
  failing" (verified: `dry_run_error`/`Committed`-path constructors set
  `action` even when `error: Some(..)`), so `phase` maps it 1:1 to a stable
  kebab-case label instead of introducing a parallel enum.
- **`--report` writes via the existing `file::atomic_write` helper** —
  `create.rs::write_run_report` — reused the repo's one atomic-write seam
  (temp file in the target's own dir, fsync, rename) rather than a bare
  `fs::write`, per `rust.md`'s filesystem-mutation-safety rule; a torn report
  write would otherwise hand a script a truncated/invalid JSON file.
- **Panic hook chains through the previous (default) hook** — `main.rs::
  install_panic_hook` — captured via `std::panic::take_hook()` and invoked
  first, so the default `thread '<name>' panicked at ...` stderr message
  Scott is used to seeing is preserved; the ERROR log line is added on top,
  not a replacement. Installed in `run()` right after `setup_logging`, so it
  is live before any parallel command (`create`/`status`/`checkout`/`clone`)
  spins up its rayon pool.
- **Test-only fault-injection hooks, mirroring `GX_TEST_FAIL_STATE_SAVE`** —
  `create/core.rs::process_single_repo` — added `GX_TEST_FORCE_REPO_ERROR`
  (forces one named repo's result to carry an error, via the existing
  `dry_run_error` path) and `GX_TEST_PANIC_WORKER` (panics the worker
  processing a named repo) so the non-zero-exit, `--report`, and panic-hook
  success criteria could each be proven end-to-end without fabricating a real
  git failure or crashing a real subprocess. Same "compiled in, inert unless
  the env var names this repo" shape as the existing state-save fault hook.

### Deviations
- **Exit-code plumbing implemented as a direct `process::exit` in `create.rs`,
  not a `main.rs` exit-mapping layer** — the doc's Phase 1 bullet names a
  mechanism ("wire through main.rs exit mapping") that doesn't exist anywhere
  in the codebase; the actual established pattern is a direct exit call in
  the command module itself. Implemented at that correct, consistent seam
  instead (same effect: non-zero exit on any repo error) - see Design
  decisions above for the full reasoning.
- **`--report` and the new exit-code behavior apply only to the direct
  create pipeline (`add`/`delete`/`sub`/`regex`), not the `llm`/`apply`
  sub-flows** — `create.rs::process_create_command` dispatches `Change::Llm`
  to `run_llm` (a separate propose/present/confirm/apply flow with its own
  `ProposeSummary`/`ApplyReport` types from a different design doc) before
  the report/exit-code logic runs. Extending machine-readable reporting and
  exit-code semantics to that flow is more surface than this doc's Phase 1
  names (it doesn't mention `llm`/`apply` at all) and risks scope creep into
  a separately-designed feature. Flagged as an open question below rather
  than silently built or silently ignored.
- **Inverted a test that pinned the pre-Phase-1 bug** —
  `tests/e2e_f12_failclosed.rs::
  test_pushed_save_failure_retains_recovery_and_reports_committed` previously
  asserted `out.status.success()` for a run whose one repo result carries an
  error (the retained-recovery message) - i.e. it pinned exactly the bug this
  phase closes (`create` ending `Ok(())` on a partial failure). Inverted to
  assert non-zero exit (and the specific exit code), per the phase-implementer
  contract: invert the old test by name, don't leave it green by accident or
  delete it silently.

### Tradeoffs
- **`RunReport` includes only failing repos, not every repo** — the Data
  Model section is worded generically ("`Vec<{ repo, phase, error }>`"), but
  the doc's own success criterion is "a file that parses as JSON and lists
  that failure" (singular focus on failures), and every existing summary
  mechanism (`status`/`checkout`/`clone`'s categorize helpers) already
  distinguishes error from non-error. A report entry per repo regardless of
  outcome would force every consumer to filter on `error != null` themselves;
  a failures-only list matches the stated purpose (a "failure summary") and
  keeps `[]` as the trivially-greppable "everything passed" signal.
- **Kept the belt-and-suspenders `Mutex` poison-recovery narrowly scoped to
  the three files the doc names** (`status.rs:133`, `checkout.rs:144`,
  `clone.rs:83`, plus their push sites) — `create/core.rs` has its own
  `Mutex<ChangeState>` usage (a different, RMW-shaped lock, not a
  results-accumulator), which the doc does not name and Phase 1 does not
  touch, to avoid scope creep into code the doc didn't flag.

### Open questions
- Should `--report` (and the non-zero-exit contract) extend to `gx create
  ... llm` / `gx apply`, or is that intentionally out of scope until a later
  phase/doc covers the LLM propose/apply reporting surface on its own terms?
  Today `--report` is silently a no-op when `action` is `Llm` since that path
  returns before the report/exit-code logic runs.
- The pre-existing flaky flock-test family (`lock`/`transaction`/`review`
  tests intermittently racing under `otto ci`'s parallel test runner,
  documented as a known issue and explicitly assigned to Phase 5) reproduced
  5 times during this phase's validation, always in files this phase does not
  touch (`lock.rs`, `transaction.rs`, `review.rs`), never in the Phase 1
  files. The full lib suite is 100% green single-threaded (303/303). Confirms
  Phase 5's assignment is correct and this phase's changes are not the cause;
  flagged here so the parent doesn't mistake it for a Phase 1 regression.

## Phase 2: Subprocess timeout + process-group kill

### Design decisions
- **`run_checked` lives in a new shared `src/subprocess.rs` module** — it is the
  single chokepoint every `git`/`gh` call routes through (Architecture "Shared
  subprocess runner"), and it must be reachable from `git.rs`, `github.rs`,
  `output.rs`, `ssh.rs`, and `bare.rs`, so a shared module is the only seam that
  fits. Harvested the process-group mechanism verbatim-in-spirit from
  `create/core/propose.rs::run_agent` (own process group via
  `CommandExt::process_group(0)`, poll `try_wait` to an `Instant` deadline,
  `kill -KILL -<pgid>` via `/bin/kill` + reap on expiry). `const
  POLL_INTERVAL_MS = 50` mirrors propose.rs.
- **Concurrent thread-drain instead of propose's file-redirect** —
  `subprocess.rs::run_checked` — `run_agent` redirects the child's stdio to a
  log file (no pipes), but `run_checked` must RETURN the captured `Output`, so
  it pipes stdout+stderr and drains BOTH on their own `std::thread`s while the
  parent polls. Reading one pipe to completion before the other deadlocks a
  child that fills the ~64 KB pipe buffer (`rust.md` subprocess hygiene) — that
  deadlock would masquerade as a hang and trip the kill. A bite test
  (`test_run_checked_drains_large_output_without_deadlock`, 200 KB) proves it.
- **No new crate** — `subprocess.rs` — std only (`process_group` via
  `std::os::unix::process::CommandExt`, `child.try_wait()`, `/bin/kill` for the
  group kill), exactly as `propose.rs` does. The doc's Dependencies note said
  "confirm whether propose.rs uses a helper crate; reuse it if so" — it does
  not, so none was added.
- **stdin is nulled** (`Stdio::null()`) — `run_checked` — a `gh`/`git` waiting on
  an interactive/credential prompt is itself a wedge; a closed stdin turns that
  prompt-hang into an immediate EOF failure. Bite test
  (`test_run_checked_nulls_stdin` with `cat`) proves it returns promptly instead
  of blocking to the timeout.
- **Timeout is a config field, threaded to deep call sites via a write-once
  `OnceLock`** — `config.rs` (`subprocess_timeout_secs` -> `subprocess-timeout-secs`,
  `DEFAULT_SUBPROCESS_TIMEOUT_SECS = 300`, `Config::subprocess_timeout() ->
  Duration`) + `subprocess.rs` (`init_subprocess_timeout` / `subprocess_timeout`).
  `main::run` calls `init_subprocess_timeout(config.subprocess_timeout())` right
  after the config loads, before any rayon pool spins up. The ~65 low-level
  git/gh call sites read the value through the free `subprocess_timeout()` (which
  falls back to the default const when uninitialized, e.g. under `cargo test`).
  Config field mirrors the pinned schema and the existing `create.confirm-threshold`
  / `github.pr-body-template` conventions (`#[serde(default, deny_unknown_fields)]`,
  accessor + default const).
- **Non-zero EXIT stays `Ok`; only a timeout is `Err`** — `run_checked` returns
  `Ok(Output)` for any completed child regardless of exit code (callers inspect
  `output.status` exactly as with `Command::output`); only a wall-clock timeout
  (or a spawn/poll failure) is `Err`. A bite test pins this so a future refactor
  can't silently turn every failed git into a timeout-shaped error.

### Deviations
- **Scope widened past `git.rs`/`github.rs` to `output.rs`, `ssh.rs`, `bare.rs`**
  — the phase bullet names `git.rs`/`github.rs`, but the doc's own success
  criterion is repo-wide (`rg 'Command::new("(git|gh)")' src/` shows every call
  through `run_checked`, "no raw `.output()` on git/gh remains"). `output.rs`
  (1 git call), `ssh.rs` (its `git config` call), and `bare.rs` (3 git calls)
  each had raw git `.output()`, so they were routed too. Zero production git/gh
  raw `.output()` remain.
- **Left un-routed, deliberately:** the `Command::new("ssh")` connectivity probe
  in `ssh.rs` (not a git/gh call; out of the criterion's scope and already
  carries its own `-o ConnectTimeout`), and the git commands in test code
  (`test_utils.rs`, `undo/core/tests.rs` — test fixtures, exempt). The
  `gh_command` factory line (`Command::new("gh")` in `github.rs`) legitimately
  keeps its bare form: it BUILDS and returns a `Command`; the actual execution
  at every `gh_command(...)?` call site is wrapped in `run_checked`.
- **Same-effect, correct-seam: file-redirect -> concurrent thread-drain** (see
  Design decisions) — the harvested mechanism is the process-group kill; the
  stdio handling differs because `run_checked` captures Output.

### Tradeoffs
- **Write-once `OnceLock<Duration>` global vs threading `Config`/`Duration`
  through ~65 git functions** — chose the global. The git/gh helpers take `Repo`,
  not `Config`, and threading a `Duration` through every signature (and its
  callers) is enormous churn for a value that is uniform process-wide. A
  write-once `OnceLock` set at startup has no shared-mutable-state hazard under
  rayon's parallel workers (all reads see the same installed value). The pinned
  `run_checked(cmd, timeout)` signature keeps the explicit timeout param so bite
  tests inject short timeouts directly; production call sites pass
  `subprocess_timeout()`.
- **Two drain threads per call vs an async/`select` single-loop** — chose plain
  `std::thread` draining: std-only, matches the `rust.md` concurrent-drain
  reference, and these call sites are blocking `std::process::Command` under
  rayon (NOT async), so introducing tokio here would be the wrong seam.

### Open questions
- The per-repo isolation of a timeout ("sibling repos complete, the run reaches
  its summary") rides Phase 1's existing collected-results machinery unchanged:
  `run_checked`'s `Err` propagates via `?` into the git helper's `Result`, which
  the per-repo rayon worker records as that repo's `error: Option<String>` — the
  exact path any git failure (e.g. a failed fetch) already travels. The
  `run_checked` unit bite tests prove the kill + `Err`; no new full-fan-out
  integration test was added, since the isolation seam is pre-existing and
  untouched. Flag if the parent wants an end-to-end rigged-hang integration test
  anyway.

## Phase 3: Confirm gate + threshold on finish-line ops

### Design decisions
- **`DestructiveOp` + `confirm_destructive` live in the existing `confirm.rs`
  module** — `confirm.rs` — that module is already "the confirmation seam";
  putting the generalized fail-closed TTY gate beside the `Confirmation`/`Token`
  core seam keeps every confirmation concept in one file, and it is the only
  seam reachable from BOTH `review.rs` and `cleanup.rs`. Harvested the exact
  fail-closed shape of `confirm_purge` (`is_terminal()` check -> loud `Err`
  naming `--yes`; y/yes parsing) and generalized the wording per op via
  `DestructiveOp::action_phrase`.
- **`confirm_destructive` always engages when called; the caller gates on the
  threshold** — `review.rs`/`cleanup.rs` — the pinned signature
  `confirm_destructive(op, count, assume_yes)` has no threshold param, so each
  caller computes `count >= threshold` and only then calls the gate (mirroring
  how `create` computes `needs_prompt` and short-circuits below it). The
  per-PR/per-branch blast-radius listing is printed by the caller BEFORE the
  call (as `review approve`/`delete` already list every PR + repo slug), so the
  gate owns only the final consent line — exactly `confirm_purge`'s division.
- **Preflight-complete-or-abort is a single `discover_all_prs` helper** —
  `review.rs::discover_all_prs` — used by BOTH `review approve` and `delete`,
  replacing the two warn-and-continue loops (`review.rs:305-315`/`:430-441`).
  It resolves discovery for EVERY org and fails the whole batch with a loud
  `Err` naming the failed org on any error. The command binds it with `?`
  BEFORE the parallel mutation section, so an aborted discovery is a structural
  guarantee of zero GitHub writes.
- **Cleanup blast radius = eligible-to-`-D` branches, not raw repo count** —
  `cleanup.rs::eligible_cleanup_count` — the count shown/gated is the branches a
  pass would actually delete (merged-only without `--force`, merged+closed with
  it), not `get_repos_needing_cleanup().len()`, so the prompt does not overstate
  the destruction. `cleanup --all` sums this across all cleanable changes; the
  gate runs BEFORE any `ChangeLock` acquisition (so it never touches the flaky
  flock family).
- **Config: `ReviewConfig`/`CleanupConfig`, each `{ confirm_threshold }`** —
  `config.rs` — mirrors `CreateConfig` exactly (`#[serde(default,
  deny_unknown_fields)]`, kebab `confirm-threshold`, `Default` = the reused
  `DEFAULT_CONFIRM_THRESHOLD = 5`), with accessors `review_confirm_threshold()`
  / `cleanup_confirm_threshold()`. `review`/`cleanup` added to `Config` +
  `Config::default()`. `gx.yml` gains commented `review:`/`cleanup:` blocks.
- **`--yes` (`-y`) added to `review approve`, `review delete`, `cleanup`** —
  `cli.rs` — same clap shape (`short='y', long="yes"`) as the existing
  `review purge`/`rollback execute` flags; threaded through `main.rs` into each
  command.

### Deviations
- **Threshold comparison is `count >= threshold`, where `create` uses
  `count > threshold`** — `review.rs`/`cleanup.rs` — the design doc and this
  phase's brief both pin "prompts only when count >= threshold" (stated twice),
  so I used `>=`. `create`'s `confirm_blast_radius` uses strict `>`; the 1-off
  difference means these irreversible finish-line ops prompt one item earlier
  than `create`, which is the fail-safe direction for a merge/delete. Same
  effect (a count-vs-threshold gate), the doc's exact boundary.
- **New tests added to the EXISTING inline `#[cfg(test)] mod tests` blocks in
  `review.rs` and `cleanup.rs`, not extracted `foo/tests.rs` files** — those two
  files already carry inline test modules; adding a second external `mod tests;`
  would collide, and `rust.md` explicitly says the inline->external migration is
  a tree-wide mechanical pass, "never mixed into a feature." So I matched each
  file's current structure. The genuinely new module (`confirm.rs`) already uses
  the external `confirm/tests.rs` form, and the new `confirm_destructive` tests
  went there per the rule. Config tests went in the existing external
  `config/tests.rs`.
- **`confirm_destructive`'s pinned signature is honored; the "org breakdown" in
  the prompt is the caller's already-printed PR list** — the doc's Phase 3
  bullet says the prompt shows "op + count + org breakdown", but the pinned API
  (`confirm_destructive(op, count, assume_yes)`) carries no org data. Same
  division as `confirm_purge`/`confirm_blast_radius`: the caller prints the
  breakdown, the gate prints the final consent line. Correct seam, same effect.

### Tradeoffs
- **Bite tests at the function/command seam with a PATH-shimmed `gh`, not live
  GitHub** — followed the repo's 2026-06-11 gh-shim precedent
  (`test_review_sync_marks_merged_pr_via_gh_shim`). The confirm gate is
  bite-tested directly for all three ops in `confirm/tests.rs` (fail-closed
  naming `--yes`, `--yes` proceeds); the preflight is bite-tested via
  `discover_all_prs` with a shim that errors one org; and each of `review
  approve` + `cleanup --all` has a COMMAND-level test with a spy shim asserting
  ZERO mutations (the merge never runs / the branch survives). `review delete`
  shares the identical gate+preflight code path as `approve`, so its command
  seam is covered transitively rather than with a third near-duplicate shim
  test.
- **`confirm_destructive` fail-closed relies on `cargo test` stdin being
  non-interactive** — no env manipulation needed for the pure gate tests
  (deterministic, lock-free); the command-level tests hold `env_lock()` because
  they touch `PATH`/`XDG_DATA_HOME`/token env.

### Open questions
- None. All three ops fail closed naming `--yes` on non-interactive stdin (bite
  tests green), `--yes` proceeds, and the preflight aborts the whole batch on a
  single org's discovery error with zero mutations proven via spy shims.

## Phase 4: Correctness guards

### Design decisions
- **`Mergeability` enum (`MERGEABLE`/`CONFLICTING`/`UNKNOWN`) + `mergeable`
  field on `PrInfo`** — `github.rs` — modeled as an enum, not a string
  (`rust.md`); parsed from a new `mergeable` node in `PR_SEARCH_QUERY` via
  `Mergeability::parse`. The raw GraphQL struct field is `#[serde(default)]
  Option<String>` so a hand-written test shim (or an older cached response)
  that omits it deserializes to `None` -> `Mergeability::Unknown` (fail
  closed), never a parse error. Only `mergeable` was wired (the required gate
  field per Phase 0); `statusCheckRollup`/`reviewDecision` were NOT added (see
  Deviations).
- **`is_mergeable(pr) -> bool`** — `github.rs` — `matches!(pr.mergeable,
  Mergeability::Mergeable)`; `Conflicting` and `Unknown` both return false. The
  single seam `review approve` consults before merging.
- **A failed `--approve` now ABORTS that PR's merge** — `github::
  approve_and_merge_pr` — previously the failure was only `warn!`'d and the
  merge proceeded (Problem Statement's "a failed `--approve` still merges"
  gap). Now it returns `Err` and the merge never runs. (See Open questions for
  the `--admin`/self-approval interaction this surfaces.)
- **`RepoChangeStatus::Skipped { reason }` + `mark_skipped`; schema v3 -> v4**
  — `state.rs` — a PR skipped for non-mergeability is neither merged nor an
  error, so the `error == None => mark_merged` outcome loop would mis-record it
  as merged. `review approve` records skips via `mark_skipped` (reason carried
  so the operator knows whether a re-run resolves it). Bumped
  `CHANGE_STATE_VERSION` to 4 so an older gx reading the new variant fails
  closed on the unknown enum (same protection the Proposed variants added).
  Handled the new variant in the two exhaustive `RepoChangeStatus` matches
  (`undo.rs::state_label`, `undo/core.rs::classify_action`) - a Skipped PR is
  still OPEN on GitHub, so undo treats it exactly like `PrOpen`.
- **`review approve` partitions open PRs into mergeable vs. skipped** —
  `review.rs::process_review_approve_command` — only the proven-mergeable
  subset is merged in parallel; skips are recorded via a single
  `record_approve_outcomes` helper (load-once/apply/save-once under the change
  lock, [A10]) covering merged + failed + skipped. An all-skipped batch records
  the skips and returns WITHOUT prompting or mutating anything. `print_skip_hints`
  prints the re-run hint ("N PR(s) skipped (mergeability not yet computed) -
  re-run `gx review approve`") plus a distinct conflict hint.
- **`review delete`'s consent prompt was already truthful** — `confirm.rs::
  DestructiveOp::action_phrase` (Phase 3) — `ReviewDelete` already renders
  "CLOSE {count} open (UNMERGED) PR(s) and DELETE their branches", which is
  exactly the Phase 4 requirement, so no change was needed. Its command still
  filters to `PrState::Open` and closes+deletes them (behavior kept, not
  flipped to merged-only).
- **`cleanup` fetched-ancestry guard** — `git.rs::branch_merged_into_base` +
  `cleanup.rs::cleanup_change` — before `git branch -D`, resolve the repo's
  default base branch NAME via `get_head_branch` (origin/HEAD, then main/master;
  never assumes `main`), FETCH `origin` first, then
  `git merge-base --is-ancestor <branch> origin/<base>`. Exit 0 -> ancestor
  (delete), exit 1 -> not-ancestor (skip + warn, preserve), other/fetch-error
  -> `Err` (fail closed, preserve). `--force` bypasses the guard entirely.
  `PrMerged` stays a fast-path signal (the pre-existing merged-status gate) but
  the fetched-ancestry check is the real guard, run even for `PrMerged` repos.

### Deviations
- **Enum named `Mergeability`, not `Mergeable`** — the brief pinned `enum
  Mergeable { Mergeable, ... }`, but that trips `clippy::enum_variant_names`
  ("variant name ends with the enum's name") under `-D warnings`. Renamed the
  enum to `Mergeability`; the variant stays `Mergeable`. Same effect, the name
  clippy allows.
- **`cleanup` base-branch resolution reuses `resolve_base_branch`'s LOCAL logic
  directly, not the private `create/core::resolve_base_branch`** — that helper
  takes `&Repo` + `&Config` and includes a GitHub-API middle step needing a
  token. `cleanup_change` has neither, and always operates on a repo that has an
  `origin` remote, so `git::branch_merged_into_base` calls `get_head_branch`
  (the same origin/HEAD -> main/master resolution `resolve_base_branch` tries
  first) and falls back to `main` with a warning (its same ultimate fallback),
  skipping only the token-requiring API step. Same intent ("resolve the name,
  don't assume main"), correct seam for cleanup.
- **`statusCheckRollup` / `reviewDecision` NOT surfaced** — the brief says these
  MAY be added "if clean"; `mergeable` is the required gate and Phase 0 pinned
  the guard on it, so I kept scope to the one field. Adding CI-rollup gating is
  a follow-up if wanted (the query path already proves them reachable).
- **Confirm-gate blast radius changed from `open_prs.len()` (Phase 3) to
  `mergeable_prs.len()`** — `review.rs` — the gate now counts the PRs that will
  ACTUALLY be merged (the mergeable subset), which is the truthful blast radius
  now that non-mergeable PRs are skipped rather than attempted. The Phase 3
  fail-closed bite test still passes (its shim PR is `MERGEABLE`).
- **Updated the Phase 3 `GH_APPROVE_SPY_SHIM` to carry `"mergeable":
  "MERGEABLE"`** — without it the shim's open PR would deserialize to `Unknown`
  and be skipped BEFORE the confirm gate, defeating the Phase 3 fail-closed test.
  Adding the field keeps that test exercising the gate (the shim predates the
  field).

### Tradeoffs
- **Skip hints printed as plain lines, not a new `ReviewAction::Skipped`
  variant** — adding a `ReviewAction` variant would force handling in
  `output.rs`'s exhaustive display matches (unrelated blast radius). The skipped
  PRs are tracked in a local `Vec<(&PrInfo, SkipReason)>`, recorded in state,
  and summarized via `print_skip_hints`; the displayed `ReviewResult` list stays
  merged/failed only.
- **Ancestry verification error -> `failed` (surfaced), not-ancestor ->
  `skipped`** — both preserve the branch (fail closed), but a fetch/verify error
  is an operational problem worth surfacing in the errors list, whereas a clean
  "not an ancestor" is an expected skip.
- **`Skipped` carries a `String` reason, not a typed enum, in state** — the
  richer `SkipReason` enum lives in `review.rs` for the summary hint; state only
  needs the human phrase, so it stores the flattened string (mirrors how
  `Failed` stores an error string).

### Open questions
- **A failed `--approve` now aborts the merge even under `--admin`.** For a solo
  SRE approving their OWN campaign's PRs, `gh pr review --approve` fails ("Can
  not approve your own pull request"), and `--admin` is the intended way to
  merge regardless. With this change that path now aborts on the approve
  failure. I implemented the doc as written ("a failed `--approve` step aborts
  THAT merge") and did NOT invent an `--admin` carve-out, but flag it: if the
  owner runs `review approve --admin` on self-authored PRs, they will now be
  blocked. Should `--admin` (or a "cannot approve own PR" classification) be
  exempted from the abort? Needs the owner's call.
