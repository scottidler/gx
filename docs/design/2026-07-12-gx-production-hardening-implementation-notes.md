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
