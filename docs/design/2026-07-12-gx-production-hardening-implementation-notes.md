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
