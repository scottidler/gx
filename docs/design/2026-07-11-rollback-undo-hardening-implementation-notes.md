# Implementation Notes: GX Rollback and Undo Hardening

Running, append-only record of how the implementation interprets or diverges
from `2026-07-11-rollback-undo-hardening.md`. One section per phase, four
buckets each ("None." where empty).

## Phase 1: Rollback never destroys evidence

### Design decisions
- Per-step journal modeled as `StepEntry { step, status, error }` wrapping the
  existing `RollbackStep`; `RecoveryState.steps` changed from
  `Vec<RollbackStep>` to `Vec<StepEntry>` — `src/transaction.rs:StepEntry`,
  `RecoveryState`. The journaled JSON is `{ "step": {...}, "status": "done" }`
  per the Data Model.
- `StepStatus` enum (`pending|applied|done|failed|skipped-legacy`,
  `rename_all = "kebab-case"`) — `src/transaction.rs:StepStatus`. `Failed`
  carries the message in a sibling `error` field (the Architecture text's
  `failed:<err>` is split into `status: failed` + `error` exactly as the Data
  Model JSON shows). `SkippedLegacy` is in the vocabulary now because the
  completion predicate references it, but nothing produces it until Phase 2's
  `LegacyDeleteRemoteBranch`.
- One journaled interpreter, `run_recovery_journaled(&mut RecoveryState, persist)`,
  drives reverse execution for BOTH `Transaction::rollback` and
  `Transaction::execute_recovery` — `src/transaction.rs:run_recovery_journaled`.
  It skips `Done`/`SkippedLegacy`, runs `Pending`/`Failed`, and rewrites the
  recovery file (atomic) after every status transition via `set_status`.
- Two-beat `PopStash` handled inline in the interpreter (not through
  `execute_step`): `git stash apply` -> journal `Applied` -> best-effort drop
  -> journal `Done` — `src/transaction.rs:run_recovery_journaled`. A step
  already at `Applied` skips the apply and retries only the drop. The drop is
  best-effort (a stash already gone still converges to `Done`), matching the
  pre-existing `finalize`/`execute_step` behavior; an apply failure is the Q2
  conflict case (journal `Failed`, keep the stash, message carries the error).
- Artifacts removed only when `RecoveryState::all_complete()` (every step
  `Done`/`SkippedLegacy`) — `src/transaction.rs:Transaction::rollback`,
  `execute_recovery`. On any failed step both keep the recovery file and backup
  dir; `execute_recovery` returns an error built by `incomplete_report`
  (names each failed step and the `gx rollback execute <tx>` re-run command),
  and `Transaction::rollback` logs the same report at `error!`.
- `gx doctor` gained a distinct `RECOVERY (FAILED STEPS):` section for live,
  within-TTL recovery files that carry failed steps; these are reported (with
  the re-run command) and are NOT purge candidates — `src/doctor.rs:report_orphans`.
  A stale/repo-gone file still ages out as an orphan even with failed steps
  (nothing left to converge against).

### Deviations
- `StepEntry` deserialization is deliberately tolerant of the pre-journal file
  shape (a bare `RollbackStep` with no wrapper), reading it as `Pending`
  (`src/transaction.rs:StepEntry Deserialize`, untagged `Repr`). The spec's
  additive/serde-default scheme covers new *fields*, not the `steps` element
  *shape* change; this keeps recovery files written by an already-shipped
  gx (v0.3.2) loadable after upgrade, serving the whole-doc acceptance criterion
  that pre-existing files still load. Same effect, correct seam.
- Phase 1 adds ONLY the journal fields to `RecoveryState`. The `version`,
  `phase`, and `branch` fields shown in the Data Model, the
  `LegacyDeleteRemoteBranch` rename, and `PopStashByMessage` are explicitly
  Phase 2/4 per the doc and the orchestrator's scope note; they are not added
  here.

### Tradeoffs
- Journal rewrite happens per status transition (apply/done/failed), i.e. up to
  two writes per two-beat step, vs one write per step. Chosen for crash-safety:
  the `Applied` beat MUST be durable before the drop so a crash retries only the
  drop. The doc's Performance section already budgets ~6-8 small writes per repo
  during rollback.
- The interpreter routes `PopStash` inline rather than through `execute_step`,
  leaving `execute_step`'s own apply+drop branch in place (still used for
  single-shot execution and by later phases' in-process interpreter). Slight
  duplication of the two git calls, accepted to keep the two-beat journaling
  legible in one place.
- A journal-write failure inside `set_status` is logged (`warn!`) but does not
  abort recovery: the reversal itself is the priority, and every step tolerates
  a re-run repeating a `Done` step. Failing the whole rollback because the
  journal file could not be rewritten would be a worse outcome than a possibly
  re-run idempotent step.

### Open questions
- None.

## Phase 2: Phase-stamped recovery, remote-safe execute

### Design decisions
- `Phase` enum (`mutating|pushing|pushed|finalizing`, `rename_all = "kebab-case"`,
  `#[serde(default)]` = `Mutating`) added to both `RecoveryState` and
  `Transaction` — `src/transaction.rs:Phase`. Stamped write-ahead via
  `Transaction::set_phase` (persists immediately): `mutating` at construction,
  `pushing` before `git push`, `pushed` after push success
  (`src/create.rs:commit_changes_with_rollback`), `finalizing` at the top of
  `Transaction::finalize`.
- `RecoveryState` gained `version: u32` (`#[serde(default = "default_version")]`
  -> `RECOVERY_STATE_VERSION = 1`) and `branch: Option<String>`
  (`#[serde(default)]`), so version-less/pre-field files still load under
  `deny_unknown_fields` — `src/transaction.rs:RecoveryState`. `branch` is set
  once at branch creation via `Transaction::set_branch`
  (`src/create.rs:commit_changes_with_rollback`); the `pushing` probe and phase
  reporting read it rather than re-deriving.
- `DeleteRemoteBranch` renamed to `LegacyDeleteRemoteBranch` with
  `#[serde(alias = "DeleteRemoteBranch")]` — `src/transaction.rs:RollbackStep`.
  Its registration site (`create.rs:941`) is deleted; the interpreter marks it
  `SkippedLegacy` with a `gx undo` hint and `execute_step` treats it as a no-op.
  The rollback interpreter now contains NO remote-mutating call.
- Phase dispatch lives in `Transaction::execute_recovery`, returning a new
  `RecoveryOutcome` (`FullReverse` | `KeepWork { branch }`) —
  `src/transaction.rs:execute_recovery`, `resolve_recovery_mode`. `mutating` ->
  full reverse; `pushing` -> `git::remote_branch_exists_probe` (read-only
  `ls-remote --exit-code`) for the recorded branch (absent -> full reverse,
  present -> keep-work); `pushed`/`finalizing` -> keep-work. Keep-work runs only
  `SwitchBranch`/`PopStash*` step kinds (`is_env_restore`), retains the pushed
  work, and exits 0. The offline probe fails closed (error, artifacts retained).
- `run_recovery_journaled` gained a `RecoveryMode` parameter —
  `src/transaction.rs:run_recovery_journaled`. `Transaction::rollback`
  (create-path abort) always passes `FullReverse` (it runs before a push
  completes and never touches a remote regardless); only `execute_recovery`
  dispatches by phase.
- `PopStashByMessage { repo, message }` step added, persisted BEFORE
  `git stash push` runs and swapped to `PopStash { stash_sha }` via
  `Transaction::swap_last_step` once the stash exists (F5 write-ahead gap) —
  `src/create.rs` stash block, `src/git.rs:stash_sha_by_message`. Two-beat
  journaled (apply -> `Applied` -> drop -> `Done`) exactly like `PopStash`,
  re-resolving message -> SHA for the drop beat; a message with no matching
  stash converges to a no-op `Done`.
- `gx rollback execute` gained a `--yes` flag and now prints a phase-aware plan
  (phase, branch, age, per-step status) and prompts `y/N` (fail-closed on
  non-interactive stdin) — `src/cli.rs:RollbackAction::Execute`,
  `src/rollback.rs:execute_recovery/print_recovery_plan/confirm_execute`.
  `--force` keeps meaning "skip validation" only. Keep-work outcomes report the
  retained branch and name `gx undo`.
- `transaction.rs` module-doc idempotency overclaim fixed: it now states
  convergence rests on the per-step journal (the two-beat stash steps are not
  purely idempotent) and that recovery never mutates a remote.

### Deviations
- Exact signatures in the doc are approximate; implemented at the correct seams.
  Phase dispatch + probe + outcome reporting live in `Transaction::execute_recovery`
  (engine) returning `RecoveryOutcome`, while the plan print + confirmation
  prompt live in `rollback.rs` (thin shell). Same effect, correct seam
  (data-returning core, side-effecting shell per the repo's shell/core split).
- Test-harness fix required to make CI green: three independent per-module
  `ENV_LOCK` mutexes (`transaction/tests.rs`, `lock/tests.rs`, `config/tests.rs`)
  all mutated the SAME global `XDG_DATA_HOME`, so they did not serialize each
  other — a latent race my git-heavy Phase 2 tests widened until it stranded a
  recovery fixture and cascaded PoisonErrors. Consolidated to one shared
  `crate::test_utils::ENV_LOCK`. Root-cause fix, not a Phase 2 behavior change.

### Tradeoffs
- Keep-work cleanup is gated on `run.failed == 0` rather than `all_complete()`,
  because keep-work intentionally leaves the retained-work steps
  (`ResetCommit`/`DeleteLocalBranch`/`RestoreBackup`) `Pending` — those are
  precisely what rollback must NOT do. The recovery file is removed once the
  environment-restore steps converge; the local GX branch is left for `gx undo`.
- `Transaction::rollback` ignores the recorded phase and always full-reverses.
  Alternative: reuse the phase dispatch. Rejected because the create-path abort
  only ever runs before a push completes (a successful push finalizes instead of
  rolling back), and full reverse registers no remote-mutating step anyway, so a
  probe would add a network round-trip for no behavioral difference.
- `swap_last_step` replaces the most-recently-pushed step by position rather
  than by a step id. The swap is immediate (no intervening push), so "last" is
  unambiguous; a step-id scheme would be heavier for no gain at this seam.

### Open questions
- None.

## Phase 3: Write mechanics

### Design decisions
- `atomic_write` now handles permissions explicitly rather than inheriting
  `NamedTempFile`'s restrictive creation mode — `src/file.rs:atomic_write`. It
  stats the EXISTING target's mode (`mode_of`) before the write and applies it
  to the temp file with an explicit `fchmod` before `persist`; a target that
  does not yet exist gets `NEW_FILE_MODE = 0o644` set the same explicit way
  (not derived from the temp file's creation-time mode or the process umask).
- `create_backup` now returns the backed-up file's mode (captured via
  `mode_of` before the copy) instead of `()` — `src/file.rs:create_backup`.
  `RollbackStep::RestoreBackup` gained `mode: u32` per the Data Model —
  `src/transaction.rs:RollbackStep`. `restore_backup` takes the mode as a
  parameter and applies it via an explicit `set_permissions` AFTER
  `atomic_write` — `src/file.rs:restore_backup`. This is load-bearing for the
  delete-then-restore path (`gx create delete`): by the time `RestoreBackup`
  runs, `original` may not exist at all, so `atomic_write`'s "preserve the
  existing target's mode" logic has nothing to preserve from and would default
  to 0644, silently dropping the executable bit. All three `create.rs` call
  sites (`apply_delete_change`, `apply_substitution_change`,
  `apply_regex_change`) thread the returned mode into the pushed
  `RestoreBackup` step.
- `StateManager::save` routes through `crate::file::atomic_write` instead of
  `fs::write` — `src/state.rs:StateManager::save` (F8). No other behavior
  change; `ChangeState`'s directory is already created in `StateManager::new`.
- Transaction id gained the pid: `gx-tx-<ts>-<pid>-<counter>` via
  `std::process::id()` — `src/transaction.rs:Transaction::new` (F9). Verified
  no other code parses the id's internal structure (`rollback.rs`,
  `cleanup.rs` treat it as an opaque string; age comes from `created_at`, not
  the id).
- `process_single_repo`'s head-branch resolution (F10) — `src/create.rs`
  (step 3, formerly `if let Ok(head) = git::get_head_branch(...)`) — now
  matches on `Result` and returns a hard `dry_run_error` (after
  `transaction.rollback()`) on failure, naming "Failed to determine head
  branch". The unrelated `if let Ok(branch) = git::get_head_branch(...)` in
  `resolve_base_branch` (PR base-branch resolution, `create.rs:1012`) is a
  deliberate, already-cascading fallback chain (head -> GitHub default ->
  `main`) per its own doc comment ("a lookup failure must never drop the PR")
  and is NOT the F10 site; left untouched.

### Deviations
- The e2e success criterion ("a fleet sub run over an executable produces zero
  mode changes in `git status --porcelain`") is verified against the GX
  branch's own committed tree (`git ls-tree <branch> -- run.sh` starts with
  `100755`, `git diff --summary main <branch> -- run.sh` carries no `mode`
  line), not the working directory after the full run completes —
  `tests/e2e_create_lifecycle.rs:test_create_sub_preserves_executable_mode`.
  Investigated by instrumenting `atomic_write` and `commit_changes_with_rollback`
  with temporary debug prints: the file is confirmed at the correct mode
  (0755) through every `atomic_write` call and immediately after `git commit`,
  inside the gx process. The drift to 0775 happens strictly AFTER the process
  exits, and is reproduced by `Transaction::finalize`'s `switch_branch` back to
  the user's original branch: `git checkout`/`switch` recreates any file whose
  blob differs between branches using the process umask (0777/0666 & ~umask)
  rather than preserving the exact prior permission bits — this is git's own
  long-standing checkout behavior (confirmed with a bare `chmod`+`rename`
  reproduction outside gx entirely) and is orthogonal to `atomic_write`; git
  itself only ever tracks a binary executable bit (100644 vs 100755) for
  regular files, never the finer-grained rwx bits, so it is not an invariant
  `atomic_write` can or should try to preserve across a branch switch. Same
  effect (proves F3 - the executable bit never gets dropped), correct seam
  (checked where git's own tracking lives, not the umask-dependent working
  tree after finalize's branch switch).
- `NEW_FILE_MODE` and the mode-preservation logic are behind `#[cfg(unix)]`,
  matching the doc's Windows non-goal ("not regressing", not new behavior);
  non-unix builds keep the pre-existing behavior (mode considerations are a
  no-op, `create_backup` returns `NEW_FILE_MODE` as a placeholder that
  `restore_backup` then can't meaningfully apply either, since the
  `set_permissions` call is also `#[cfg(unix)]`).

### Tradeoffs
- `mode_of` masks to `0o7777` (rwxrwxrwx + setuid/setgid/sticky), not just
  `0o777`, so an existing setuid/setgid/sticky bit is preserved rather than
  silently dropped. Chosen over a `0o777` mask because dropping those bits
  would be a second, narrower version of the same F3 bug for the (rare but
  real) tracked file that carries one.
- The umask-independence test (`test_atomic_write_new_file_mode_under_restrictive_umask`)
  uses a raw `extern "C" { fn umask(mask: u32) -> u32; }` FFI declaration
  rather than the `libc` crate, per the doc's explicit "no new crates
  expected" for mode handling — `src/file/tests.rs`. `mode_t` is a 32-bit
  unsigned int on every unix target this crate builds for.

### Open questions
- None.

## Phase 4: State integrity and reconciliation

### Design decisions
- `ChangeState` gained `version: u32` (`#[serde(default = "default_version")]` ->
  `CHANGE_STATE_VERSION = 1`), matching `RecoveryState`'s Phase 2 scheme exactly
  — `src/state.rs:ChangeState`, `default_version`. `ChangeState::new` stamps the
  current version; version-less files load under `deny_unknown_fields` via the
  serde default.
- `RepoChangeState.base_sha: Option<String>` added (`#[serde(default)]`) —
  `src/state.rs:RepoChangeState`. Populated from the SAME value `ResetCommit`
  already captures (the pre-commit HEAD), returned now from
  `commit_changes_with_rollback` (changed `Result<()>` -> `Result<String>`) —
  `src/create.rs:commit_changes_with_rollback`.
- **Control-flow refactor (named, per the Resolved Decisions and Data Model
  write order)**: `process_single_repo` gained two new parameters,
  `change_state: Option<&Mutex<ChangeState>>` and
  `state_manager: Option<&StateManager>`, and now owns BOTH state saves itself
  — `src/create.rs:process_single_repo`. The write order matches the doc
  exactly: stamp `pushing` -> push -> stamp `pushed` (both inside
  `commit_changes_with_rollback`, unchanged from Phase 2) -> **`record_pushed_state`**
  (new: adds/updates the repo entry with `branch_name`, `local_path`,
  `files_modified`, `original_branch`, `base_sha`, saves via `StateManager` if
  present) -> stamp `finalizing` -> `finalize()` (deletes the recovery file) ->
  **`record_final_state`** (new: folds the finished `CreateResult`, including
  any PR, via the pre-existing `update_change_state` helper, then re-stamps
  `base_sha` since `add_repository` resets the entry, and saves). The caller's
  outer rayon fold in `process_create_command` is now display-only — it just
  collects `CreateResult`s and passes `change_state.as_ref()` /
  `state_manager.as_ref()` into each `process_single_repo` call — closing F12:
  a pushed branch is now recorded in state OR recovery (or both) in every crash
  window, because the state save happens BEFORE the recovery file is deleted,
  not after the whole per-repo function (including PR creation) returns.
- `gx review sync <change-id>` — `src/review.rs:process_review_sync_command`
  (CLI shell: repo discovery, org auto-detection, calls `github::list_prs_by_change_id`
  per org) + `sync_change_state` (core: reconciles already-fetched `PrInfo`s
  into `mark_merged`/`mark_closed`, saves once, returns
  `(merged, closed, ChangeStatus)`). Split shell/core so tests exercise the
  reconciliation logic directly with a `gh`-shimmed fetch, without repo
  discovery. Wired as `ReviewAction::Sync { change_id }` in `src/cli.rs` and
  dispatched in `src/main.rs`.
- `PrInfo` gained `merged_at: Option<String>`, `merge_commit_oid: Option<String>`,
  `base_ref_name: String`; `PrState` gained a `Merged` variant distinct from
  `Closed` (GraphQL's `PullRequest.state` is OPEN/CLOSED/MERGED, not two-valued)
  — `src/github.rs`. `PR_SEARCH_QUERY` gained `mergedAt`, `mergeCommit { oid }`,
  `baseRefName`. The search string (extracted into `pr_search_string(org, pattern)`
  for testability) DROPS the `is:open` filter — `gx review sync` needs
  merged/closed PRs, and the doc calls out today's open-only restriction as the
  thing Phase 4 fixes. `review approve`/`review delete` are unaffected: they
  already filter locally to `PrState::Open`. `review clone`'s `include_closed`
  gate now treats `Merged` the same as `Closed` (`!matches!(pr.state, Closed | Merged)`)
  to preserve its prior behavior (previously GraphQL bucketed MERGED under
  "not OPEN", i.e. the old two-valued `Closed`).
- F14: `ChangeState::mark_failed` now calls `update_overall_status()` (it
  previously didn't), and `update_overall_status` gained a `failed == total`
  branch that resolves the aggregate to `ChangeStatus::Failed` — `src/state.rs`.
  `mark_closed` also now calls `update_overall_status()` for the same reason
  (aggregate must reflect closed PRs, not just merged ones). `StateManager::cleanup_old`
  now also ages out `ChangeStatus::Failed` changes (previously only
  `FullyMerged`/`Abandoned`), since a failed campaign can now actually reach
  that status and needs a path to age out — `src/state.rs:cleanup_old`. This is
  the concrete meaning of the Phase 4 bullet "`rollback cleanup --older-than`
  ... operate on the trued-up statuses": `gx rollback cleanup --older-than`
  calls `cleanup_old` under the hood.
- New `merged_at`/`merge_commit_oid`/`base_ref_name` fields are read (not dead
  code) via a `trace!` log per PR inside `sync_change_state`'s reconciliation
  loop — `src/review.rs:sync_change_state`. This is a real, if minimal, use:
  diagnosability for exactly the command whose job is surfacing GitHub truth;
  `merge_commit_oid`/`base_ref_name` are otherwise unconsumed until Phase 6's
  revert path.

### Deviations
- The doc's Phase 4 bullet says "`version: 1` on `ChangeState` + `RecoveryState`"
  — `RecoveryState.version` was ALREADY added in Phase 2 (per the orchestrator's
  scope note and confirmed by reading `src/transaction.rs`); this phase adds it
  ONLY to `ChangeState`, matching the exact same serde-default scheme. Same
  effect, no duplicate work.
- The doc says `gx review sync` does "gh PR lookups -> mark_merged/mark_closed".
  Implemented as a shell/core split (`process_review_sync_command` +
  `sync_change_state`) rather than one function, matching the repo's existing
  shell/thin-core convention and making the reconciliation logic directly
  testable with a `gh`-shim without needing repo discovery / org
  auto-detection scaffolding in the test. Same effect, correct seam.
- The gh PATH shim (test harness) did not exist in this repo before this phase
  (confirmed: no `shim` hits anywhere in `src/`). Added inline as a `const`
  shell script in `src/review.rs`'s test module, asserting exact argv (the
  `api graphql` invocation and a `q=` arg containing the change-id pattern)
  before returning canned JSON, per the 2026-06-11 gh-shim precedent named in
  the design doc. No new test fixture files; kept as one self-contained test.

### Tradeoffs
- `record_pushed_state`/`record_final_state` both take `Option<&Mutex<ChangeState>>`
  / `Option<&StateManager>` and silently no-op when either is `None` (dry-run
  path, where `process_create_command` never constructs them). Alternative:
  thread a single `Option<(&Mutex<ChangeState>, &StateManager)>` pair. Rejected
  as no real improvement — both must be present or absent together today, but
  keeping them as two independent options matches how `process_create_command`
  already constructs them separately and costs nothing.
- `record_final_state` re-invokes `update_change_state` (unchanged) rather than
  inlining its logic, then patches `base_sha` back in afterward (since
  `add_repository` resets the whole `RepoChangeState`). Alternative: thread
  `base_sha` as a parameter into `update_change_state` itself. Rejected to keep
  `update_change_state`'s signature stable (it is also exercised by its own
  existing call shape/tests) and because the patch-back is a single field
  write, not worth widening a shared helper's signature for.
- `process_single_repo` now takes 9 parameters, past the default clippy
  threshold; `#[allow(clippy::too_many_arguments)]` added, mirroring the
  identical annotation already on `process_create_command` in the same file
  for the same reason (`clippy.toml` does not exist in this repo to raise the
  threshold repo-wide, and adding one crate-wide for a single call site is a
  bigger change than a scoped allow at the two sites that need it).
- `pr_search_string`/`PR_SEARCH_QUERY` broadening to drop `is:open` increases
  the result size of every `list_prs_by_change_id` call (more gh/GraphQL rows
  returned per query for repos with long-closed history under the same
  change-id prefix). Accepted: the design doc explicitly calls this out as the
  fix Phase 4 makes, pagination already handles arbitrary result counts
  (`hasNextPage`/`endCursor`), and existing consumers (`approve`/`delete`)
  filter locally so their behavior is unchanged.

### Open questions
- None.

## Phase 5: gx undo core

### Design decisions
- New `src/undo.rs` module + `gx undo <change-id>` subcommand
  (`src/cli.rs:Commands::Undo`, dispatched in `src/main.rs`), with `--yes` and
  `--org`. The command shell (`undo::process_undo_command`) reconciles against
  GitHub, prints the plan, prompts (fail-closed on non-TTY, `--yes`), then
  executes per repo in parallel; the core is split into pure, directly-testable
  functions (`classify_action`, `build_plan`, `needs_action`, `finalize_state`)
  and side-effecting ones (`undo_one`, `delete_branches`, `reconcile`), per the
  repo's shell/core split.
- Reconciliation reuses Phase 4's `review::sync_change_state` (widened to
  `pub(crate)`) — `src/undo.rs:reconcile`. Orgs are derived from the recorded
  repo slugs (no repo discovery needed: undo already knows exactly which repos
  and orgs are in the campaign), or an explicit `--org`. A per-org fetch failure
  is a `warn!`, not a hard error, so an offline undo of local branches still
  proceeds on the recorded state (matching `review sync`'s warn-and-continue).
- Recovery-file drain FIRST (load-bearing panel finding) — `src/undo.rs:undo_one`.
  Every plan entry carries the transaction ids of live recovery files whose
  `change_id` matches AND whose `repo_path` matches the repo (`build_plan` +
  `same_repo`, canonical-path comparison). `undo_one` invokes the SAME rollback
  interpreter via `Transaction::execute_recovery` (in-process, phase-aware)
  BEFORE the campaign action, under the per-repo lock — restoring a
  `mutating`-phase crash's stash before the branch is deleted, so WIP is never
  stranded.
- Sources = change state PLUS recovery files matching the change-id (F12): a
  recovery file whose repo is NOT in the state file becomes its own
  committed-local-only plan entry (`build_plan`), so a crash between push and
  state save is never stranded.
- Actions per reconciled state (`classify_action`, matching the Architecture
  table): `PrOpen`/`PrDraft` (+ number) -> close PR -> delete remote -> delete
  local; `BranchCreated`/`PrClosed`/`Failed` -> delete remote -> delete local;
  recovery-only -> delete local; `PrMerged` -> `RequiresRevert` (REPORTED, never
  reversed here); `CleanedUp` -> `AlreadyGone` (skip).
- Remote-branch deletion uses `github::delete_remote_branch` (built on the
  token-consistent `gh_command`) and PR close uses `github::close_pr`, per the
  orchestrator's "all gh calls through gh_command" instruction. Local-branch
  deletion resolves the recorded `local_path` first (`crate::bare::is_git_path`
  gate); a recorded-but-missing path is REPORTED as a per-repo failure, not
  silently skipped. The local delete is guarded by `git::branch_exists_locally`
  so an already-gone branch (e.g. deleted by the recovery drain's full reverse)
  is a no-op rather than an error — an existence check, not error-string
  sniffing.
- Partial failure converges (`finalize_state`): only `Undone` repos are marked
  `CleanedUp`; `Failed` repos are left as reconciled so a re-run retries them
  and `AlreadyGone` rows skip. The aggregate flips to `Abandoned` only when
  every repo is resolved with no merged rows, `PartiallyMerged` when merged rows
  remain (they need the Phase 6 revert), and is left untouched on partial
  failure (never falsely `Abandoned`).
- Output reuses `review`'s unified-results rendering
  (`output::display_review_results`) + a review-style summary line —
  `src/undo.rs:render_results`.

### Deviations
- Per-repo undo results are rendered as `ReviewResult { action:
  ReviewAction::Deleted }` to reuse `review`'s unified renderer verbatim (the
  orchestrator asked undo to "look like review") rather than introducing a new
  `ReviewAction` variant that would touch `output.rs`'s two `UnifiedDisplay`
  impls and the byte-identical review-renderer regression test. Merged rows are
  surfaced as an explicit error line ("PR #N merged; requires revert (Phase 6) -
  not reversed") so they are visibly REPORTED, never silently skipped. Same
  effect (review-style unified results, merged rows visible), minimal blast
  radius.
- Remote-branch deletion is via `github::delete_remote_branch` (gh api DELETE,
  per the orchestrator's gh_command instruction), NOT `git push --delete`. The
  e2e's `gh` PATH shim therefore performs the real ref deletion on the bare
  remote (locating it via `$GX_TEST_REMOTES` + the repo name parsed from the api
  path), consistent with the 2026-06-11 gh-shim precedent (assert argv shape,
  perform the real effect).
- Reconcile is best-effort (warn-and-continue) rather than fail-closed on an
  offline gh: undoing local branches must still work offline, and the recorded
  state is the authority for the plan. This mirrors `review sync`'s existing
  per-org warn behavior. Same seam, no new failure mode.

### Tradeoffs
- `github::delete_remote_branch` (gh api) returns an error on an already-absent
  remote branch (404); undo does not yet pre-probe remote existence, so a
  within-run remote delete of a never-pushed branch would report a per-repo
  failure. Convergence still holds: a successful undo marks the repo
  `CleanedUp`, so a re-run skips it, and a genuinely-absent-but-recorded branch
  is a rare partial-failure that retries. Pre-delete existence checks are
  explicitly Phase 7's scope (F13, replacing string-sniffed git errors with
  `show-ref`/`ls-remote --exit-code`); deferred here to avoid an extra network
  round-trip per repo and to keep Phase 5 to its mandate.
- The per-repo lock is acquired inside `undo_one` (mirroring `create`'s
  `process_single_repo`) rather than around the whole command, so the parallel
  rayon workers each hold only their own repo's lock. Phase 7 EXTENDS lock
  coverage and adds a change-level lock; this phase deliberately adds only the
  per-repo lock the orchestrator scoped.
- `build_plan` associates a recovery file to a state repo by canonical-path
  equality (`same_repo`), falling back to a raw comparison when a path does not
  resolve. A recovery file whose `repo_path` matches no state repo becomes a
  standalone committed-local-only entry rather than being dropped — chosen so
  F12's crash-between-push-and-save window can never strand a repo.

### Open questions
- The 404-on-already-deleted-remote-branch tolerance (above) is left for Phase 7
  to harden with an explicit existence check; flagged so it is not assumed
  already handled. No action needed from the user.
- Cross-repo/system-mutating bullets: none in Phase 5.

## Phase 6: Merged-PR revert path

### Design decisions
- Revert mechanics implemented as `revert_merged` in `src/undo.rs`, called from
  `undo_one`'s `UndoAction::RequiresRevert` arm (Phase 5 only reported it). Flow:
  collision check (local + remote) -> `git fetch origin` -> `git checkout -b
  revert/<change-id> origin/<base>` -> parent-count dispatch -> `git revert`
  (or `-m 1`) -> `push_branch` -> `github::create_revert_pr`. Under the same
  per-repo lock Phase 5 already takes in `undo_one`.
- Parent-count dispatch is authoritative from git, not the PR merge method:
  `git::commit_parent_count` runs `git rev-list --parents -n 1 <oid>` and counts
  tokens-1 — `src/git.rs:commit_parent_count`. `>= 2` -> `-m 1` (true merge),
  else plain revert (squash/rebase). Per the doc's "get it via `git rev-list
  --parents`, NOT by guessing from PR type."
- The merge commit oid + base branch are threaded from the GitHub reconcile, not
  persisted to state: `reconcile` now returns the merged `PrInfo`s, and
  `build_plan` copies `merge_commit_oid`/`base_ref_name` onto the merged repo's
  `UndoPlan` — `src/undo.rs:reconcile`, `build_plan`, `UndoPlan`. This keeps the
  state file schema unchanged for Phase 6 and sources the oid from GitHub reality
  at undo time.
- New git seams, each with function-level debug logging and fail-loud errors:
  `git::commit_parent_count`, `git::fetch_origin`, `git::create_branch_at`,
  `git::revert_commit` (`--no-edit`, optional `-m` mainline) — `src/git.rs`.
- `github::create_revert_pr` opens the revert PR against the original base with a
  body linking the original PR (`Reverts #<n>`), reusing `retry_gh` and
  `extract_pr_number_from_url` — `src/github.rs:create_revert_pr`.
- New terminal per-repo status `RepoChangeStatus::RevertPrOpen` +
  `ChangeState::mark_revert_pr_open` — `src/state.rs`. `finalize_state` marks a
  reverted merged row `RevertPrOpen` and flips the aggregate to `Abandoned` once
  every row is resolved (`CleanedUp | RevertPrOpen`); a failed revert leaves the
  row `PrMerged` so a re-run retries — `src/undo.rs:finalize_state`.
- Collision fails closed and touches nothing: an existing `revert/<change-id>`
  branch locally OR on the remote returns `Failed` naming the branch, checked
  BEFORE any fetch/branch/commit. The remote probe (`remote_branch_exists_probe`)
  failing (offline) is ALSO a fail-closed `Failed`, never a silent proceed.
- Conflict handling: `git::revert_commit` returning `Err` (conflict or otherwise)
  is reported per repo and the revert branch is left mid-revert for manual
  resolution — no `git revert --abort`, no force. Push/PR are skipped on conflict.

### Deviations
- Doc row wording said merged rows hold at `PartiallyMerged` until reverted
  (Phase 5 text); Phase 6 supersedes that: a reverted merged row is `RevertPrOpen`
  and the aggregate reaches `Abandoned` once all rows resolve. Same effect the doc
  intends ("aggregate `Abandoned` once every merged row has a revert PR open");
  the Phase 5 `OutcomeKind::MergedPendingRevert` was replaced by
  `RevertPrOpened { pr_number }` and its `finalize_state` test inverted
  (`finalize_state_abandons_when_merged_row_reverted`) rather than left green.
- The merge commit oid/base are carried on `UndoPlan` (populated from the
  reconcile) rather than added to `RepoChangeState`. Same effect (the revert has
  the oid it needs), correct seam — the design's Data Model section did not add
  these to `ChangeState`, and sourcing from live GitHub reconcile is more
  truthful than a persisted copy that could go stale.

### Tradeoffs
- Revert branch cut from `origin/<base>` (after `git fetch origin`) rather than a
  local base ref — chosen so the revert lands on the base head the merge actually
  landed on, at the cost of one fetch per merged repo. Matches the doc's "off the
  BASE branch head."
- If the revert branch pushes but `create_revert_pr` fails, the branch is left on
  the remote and a re-run hits the remote-collision guard (reported, not
  auto-recovered). Chosen over auto-deleting a just-pushed branch (undo never
  force/deletes speculatively); the failure message tells the user. An observed
  re-run pain would justify a "reuse my own pushed revert branch" refinement
  later, not now.
- Phase 6 revert tests run in-process against `undo_one` with a `gh` PATH shim for
  `pr create` and real bare remotes, rather than spawning the full `gx undo`
  binary with a GraphQL shim that would have to bake in the dynamic merge oid.
  This tests the load-bearing mechanics (parent-count dispatch, inverse diff,
  collision) directly and deterministically.

### Open questions
- None. No cross-repo/system-mutating bullets in Phase 6.

## Phase 7: Lock coverage and reclaim race

### Design decisions
- `RepoLock` now covers `rollback execute` (`src/rollback.rs:execute_recovery`,
  acquired right after `Transaction::load_recovery_state`, held through
  validate/confirm/execute), `cleanup` (`src/cleanup.rs:cleanup_change`, per
  repo, acquired before the local/remote branch deletes), and `review clone`
  (`src/review.rs:clone_repo_for_pr`, acquired before the clone/pull). `undo`
  already took it in Phase 5 (`src/undo.rs:undo_one`) — untouched here.
- New `ChangeLock` (`src/lock.rs`), same RAII/acquire/reclaim shape as
  `RepoLock`, keyed `locks/change-<fnv1a-hex(change_id)>.lock`. Both lock kinds
  now share one `acquire_lock_file`/`reclaim_if_stale` implementation
  (`src/lock.rs`) instead of duplicating the loop, so the F7 fix applies to
  both uniformly.
- `ChangeLock` acquired around every `changes/<id>.json` read-modify-write:
  `review sync` (`src/review.rs:sync_change_state`, whole function body),
  `review approve`/`delete` (`src/review.rs`, the load-mutate-save block after
  the parallel PR pass), `cleanup` (`src/cleanup.rs:cleanup_single_change` and
  `cleanup_all_merged`, the latter now RELOADS state fresh under the lock
  rather than reusing `list()`'s pre-lock snapshot — see Deviations), and
  `undo` (`src/undo.rs:process_undo_command`, only around the FINAL
  reload-finalize-save, not the whole command — see Deviations). The
  create-path incremental saves (`record_pushed_state`/`record_final_state`)
  are covered by ONE `ChangeLock` acquired once for the whole `create` run in
  `process_create_command`, not per-save — see Deviations.
- F7 reclaim TOCTOU: `reclaim_if_stale` (`src/lock.rs`) renames the candidate
  to a private, counter-suffixed staging name FIRST, re-verifies staleness on
  the file it now exclusively owns via `is_stale_lock_content`, and only then
  removes it. A losing racer's rename fails (ENOENT) and returns `Ok(true)`
  (retry `acquire` from scratch) rather than ever calling `remove_file` on the
  shared path. If the swept-up file turns out to be live (a racing reclaimer
  already recreated it), it is restored via `fs::hard_link` (never a blind
  `rename`, which would silently clobber a THIRD racer that legitimately
  claimed the path in the interim) — see Deviations for how this was found.
- F13: `git::delete_remote_branch` (`src/git.rs`) now pre-probes with
  `remote_branch_exists_probe` (`ls-remote --exit-code`, already existed from
  Phase 2) and no-ops on an absent branch, replacing the
  `stderr.contains("remote ref does not exist")` sniff. `cleanup.rs`'s local
  branch delete now pre-checks `git::branch_exists_locally` (`git rev-parse
  --verify`, i.e. `show-ref`-equivalent) instead of sniffing
  `err.contains("not found")`/`"does not exist"` on `delete_local_branch`'s
  error.
- `undo`'s `delete_branches` (`src/undo.rs`) pre-probes the remote branch via
  `git::remote_branch_exists_probe` (using the recorded local checkout) before
  ever calling `github::delete_remote_branch` (the `gh api ... DELETE` path),
  so a never-pushed or already-deleted branch is a no-op for that repo instead
  of a per-repo failure (the item Phase 5 explicitly deferred here). When no
  local checkout is available to probe from, it falls back to attempting the
  delete (the pre-Phase-7 behavior for that edge).

### Deviations
- **Root-caused a real concurrency bug found while writing the reclaim-race
  test, not spec-mandated but load-bearing:** the first `reclaim_if_stale`
  draft treated unparseable lock content (a file mid-write, between
  `create_new` succeeding and its holder's `writeln!` landing) as STALE. Under
  an 8-thread stress test this let every racer reclaim (and, via the naive
  `rename`-based restore, sometimes clobber) every OTHER racer's
  just-created-but-not-yet-written lock, so more than one thread's
  `RepoLock::acquire` returned success. Fixed two ways: (1)
  `is_stale_lock_content` now treats unparseable content as NOT stale (a lock
  we cannot positively confirm dead is never reclaimed — this is the ORIGINAL
  code's behavior too; my first rewrite had flipped it); (2) the restore
  branch uses `fs::hard_link` (fails on an existing destination) instead of
  `fs::rename` (silently clobbers), so a third racer that legitimately grabbed
  the path during the check is never destroyed; (3) `acquire_lock_file`
  re-reads `path` immediately after writing and confirms its OWN content is
  still there before reporting success, retrying instead if a concurrent
  reclaimer swept it away in that gap. All three are needed together; each is
  covered by its own test (`test_is_stale_lock_content_treats_unparseable_as_not_stale`,
  the hard_link comment in `reclaim_if_stale`, and
  `test_concurrent_reclaim_never_loses_the_winning_live_lock`, which failed
  reproducibly (2-3 times per 10-30 runs) before all three fixes and is now
  clean across 50+ consecutive runs plus 8 full `cargo test` runs).
- `undo`'s final `ChangeLock` acquisition wraps ONLY the reload-finalize-save
  span (`src/undo.rs:process_undo_command`, after the parallel campaign
  execution), not the whole command. The doc says the lock is "held around
  every read-modify-write of `changes/<id>.json`" — the actual RMW unit here
  is that final reload+mutate+save; the earlier reads (used for planning) and
  `reconcile`'s own internal `sync_change_state` call (which takes its own
  `ChangeLock` for its own RMW) are separate, narrower critical sections.
  Holding one lock across the ENTIRE command (including user confirmation and
  the potentially-long per-repo network/git work) would block a concurrent
  `review sync` for that whole duration for no correctness benefit; the
  narrower placement still closes the lost-update race the panel finding
  requires.
- `create`'s `ChangeLock` is acquired ONCE for the whole `process_create_command`
  run (`src/create.rs`), not per-repo around each `record_pushed_state`/
  `record_final_state` call. Per-repo acquisition would make sibling rayon
  workers in the SAME run fail-fast against EACH OTHER (the lock has no
  queueing/blocking semantics, matching `RepoLock`'s design), which would
  silently drop incremental saves within a single `create` invocation. The
  existing in-process `Mutex<ChangeState>` already serializes those workers
  against each other; the single per-run `ChangeLock` adds the missing
  cross-process half without conflicting with it.
- `cleanup_all_merged` (`src/cleanup.rs`) now RELOADS each change's state
  fresh, under its `ChangeLock`, discarding the pre-lock snapshot from
  `state_manager.list()`. Not explicitly called out in the doc, but required
  for the lock to actually close the lost-update race here: `list()`'s
  snapshot predates lock acquisition, so saving it unmodified-except-for-our-
  mutation would silently clobber any concurrent update that landed between
  the listing and our save.
- F13 fix scope: only the two Appendix A sites (`cleanup.rs`, `git.rs`) and
  `undo`'s remote-branch delete (explicitly named in the task) were converted
  to explicit existence checks. `src/review.rs`'s two OTHER
  `github::delete_remote_branch` call sites (`purge_repo_branches`,
  `delete_pr_and_branch`) still call the gh-api path directly without a
  pre-probe; they were not named in Phase 7's scope and are left as-is to
  avoid scope creep.

### Tradeoffs
- Shared `acquire_lock_file`/`reclaim_if_stale` between `RepoLock` and
  `ChangeLock` vs. two independent copies — chosen for a single point of fix
  for the reclaim TOCTOU (and the concurrency bug found above) rather than
  keeping the logic in sync by hand across two files.
- The two-process contention test hook (`GX_TEST_LOCK_DELAY_MS`, `src/lock.rs`)
  is a small, narrowly-scoped, inert-unless-set env var read once right after
  a successful acquire — the same "compiled in, inert by default" shape the
  design already prescribes for Phase 8's `GX_CRASH_POINT`, borrowed early and
  narrowly (one `sleep`, nothing else) so the two-process test can create a
  deterministic contention window against a real spawned `gx rollback execute`
  rather than racing on uncontrolled process-startup timing.
- Considered a full OS-level advisory lock (`std::fs::File::lock`, stable
  since Rust 1.89) to make reclaim airtight under ARBITRARY concurrent stress
  with zero residual race window, which would have eliminated the need for the
  reclaim-rename dance and the bug above entirely. Not chosen: the design doc
  explicitly prescribes the rename-based reclaim mechanism; switching primitives
  is a bigger, unrequested architecture change. The three-part fix above closes
  the actual bug found (verified clean across 50+ stress-test runs and 8 full
  `cargo test` runs) without deviating from the doc's prescribed mechanism.

### Open questions
- None. No cross-repo/system-mutating bullets in Phase 7.

## Phase 8: Crash-injection tests

### Design decisions
- New `src/crash.rs` module (`pub mod crash` in `lib.rs`, `mod crash` in
  `main.rs`) exposing `maybe_crash(point)` + the `CRASH_POINTS` vocabulary and
  the `GX_CRASH_POINT` env-var name. Compiled into every build, INERT unless
  `$GX_CRASH_POINT` names a point; on a match it logs and calls
  `std::process::abort()` (NOT `exit`) so NO `Drop` runs and the write-ahead
  recovery file/backups are left exactly as a real SIGKILL would — the whole
  point of the hook. Every crash point sits AFTER the write-ahead persist that
  makes it recoverable.
- Six call sites wired at the exact phase boundaries: `after-stash`,
  `after-branch`, `after-commit`, `before-push` (after the `pushing` stamp,
  before `git push`), `after-push` (after the `pushed` stamp) in
  `src/create.rs`, and `mid-finalize` (after the `finalizing` stamp) in
  `src/transaction.rs:Transaction::finalize`.
- `maybe_crash` fails LOUD on a typo'd point: a set-but-unrecognized
  `$GX_CRASH_POINT` logs an error naming the known vocabulary and no-ops, rather
  than silently never crashing (also gives `CRASH_POINTS` a real production use
  so it is not dead code) — `src/crash.rs:maybe_crash`.
- e2e crash matrix (`tests/e2e_crash_injection.rs:test_crash_matrix_all_six_points`):
  one dirty single-repo fixture with a bare remote per point (dirty via an
  untracked `wip.txt`, so every run stashes and arms `after-stash`; a tracked
  0755 `run.sh` proves git's tracked exec bit survives). For each of the six
  points: spawn the REAL `gx create --commit`, assert it dies; read the sole
  recovery file and assert its recorded phase; assert `gx rollback list` shows
  `Phase: <expected>`; run `gx rollback execute --force --yes`; then assert
  worktree byte-identity and remote/local branch retention.
- Worktree byte-identity is asserted via a `worktree_snapshot` = HEAD sha +
  sorted `git status --porcelain` + `git ls-files -s` (tracked mode + blob sha)
  + the two data files' contents, compared before/after. This is the MODE
  CAVEAT-correct comparison: git tracks only the exec bit (100755/100644), and
  `finalize`'s branch switch recreates changed files under the process umask, so
  raw `std::fs` rwx bits would false-fail on group-bit drift — `ls-files -s`
  compares git's own tracked state (mode + blob) instead.
- Remote/local retention matrix: `after-push`/`mid-finalize` (`pushed`/
  `finalizing` -> keep-work) RETAIN the branch remote AND local; `before-push`
  (`pushing` -> ls-remote probe finds it absent -> full reverse) and the three
  `mutating` points (`after-stash`/`after-branch`/`after-commit`) leave the
  branch ABSENT.
- Direct tests added to `src/transaction/tests.rs`:
  `test_execute_recovery_against_real_interrupted_run_file` (recovery from a file
  a real `Transaction` persisted, not a hand-authored fixture),
  `test_finalize_stash_conflict_surfaces_stash_error` (the Q2
  `FinalizeOutcome.stash_error` path: a conflicting stash re-apply surfaces the
  SHA and leaves the stash undropped), and
  `test_legacy_delete_remote_branch_file_executes_as_skipped_legacy`
  (hand-authored JSON with the pre-rename `DeleteRemoteBranch` key loads via the
  serde alias and executes as `skipped-legacy`, converging + cleaning up).
- Grep-guard (`src/crash/tests.rs:test_crash_hook_call_sites_are_exactly_the_wired_points`):
  scans production source (excludes `*/tests.rs` and the hook's own module),
  asserts EXACTLY six `crate::crash::maybe_crash("<point>")` call sites, one per
  vocabulary point, only in `create.rs` (5) and `transaction.rs` (1), no call
  with an unknown point, and that `std::process::abort(` appears in no
  production file but the hook (Risks-table mitigation). Plus a behavioral
  inertness test that `maybe_crash` returns without aborting when the env var is
  unset or names a different point.
- `gx rollback list` and `gx rollback validate` now surface the recorded phase
  (and `list` a per-step status summary; `validate` a per-step status line) —
  `src/rollback.rs`. See Deviations: this closes an API-table item Phase 2 left
  unimplemented and is load-bearing for the crash matrix's "list shows the right
  phase" assertion.

### Deviations
- **Behavior fix in the create path (correct seam, recorded):** the design's
  keep-work recovery ("execute SwitchBranch/PopStash step kinds") presupposes a
  SwitchBranch-to-original step exists, but Phase 2/3 only registered one when
  `head != original_branch`. In the common case (user already on the base
  branch) NO switch-back step existed, so a keep-work recovery after an
  `after-push`/`mid-finalize` crash would strand the worktree on the GX branch
  (finalize's own switch-back never runs on a crash). Fixed by ALWAYS registering
  the write-ahead `SwitchBranch { branch: original }` step in
  `commit_changes_with_rollback`; the actual switch-to-head stays conditional.
  This is a NO-OP for full reverse (DeleteLocalBranch already force-switches off
  the GX branch to head, which equals original in the common case), so no
  full-reverse test changed — `src/create.rs`. Same effect the design intends,
  correct seam; the crash matrix is what exposed it.
- `gx rollback list`/`validate` gaining the phase (+ per-step status) output was
  listed in the design's API table but not implemented in Phase 2. Added here
  because the Phase 8 crash matrix asserts `list` shows the right phase and the
  Phase 8 direct-test bullet covers `list`/`validate` output. Small, aligned
  with the API table, `src/rollback.rs`.
- Exact signatures/placement are approximate in the doc; the hook is a single
  `crate::crash::maybe_crash(point)` seam rather than inline `if env::var(...)`
  blocks at each site, keeping the six call sites one-liners and grep-checkable.
  Same effect, correct seam.
- The crash matrix uses ONE repo per point (not a multi-repo org) so the crash
  fires deterministically and the recovery file is unambiguous (`abort()` kills
  the whole process, so multiple in-flight repos would race). The design says
  "a fixture org with bare remotes"; a single-repo org per point is the precise
  form that makes the phase assertion deterministic. Same effect.

### Tradeoffs
- The e2e spawns the real binary and aborts it (real SIGKILL-equivalent) rather
  than simulating a crash in-process, per F15's mandate ("no test kills a real
  process"). Cost: six full `gx create` spawns + rollbacks (~8s total); accepted
  as the whole point of the phase.
- `maybe_crash` reads the env var on every call (six times per committing run)
  rather than caching it once. A committing gx run makes far heavier git/network
  calls per repo; one `getenv` per phase boundary is negligible and keeps the
  hook a pure, stateless, inert-by-default function.
- The grep-guard matches the fully-qualified `crate::crash::maybe_crash(` form
  and excludes test files. A call written as `maybe_crash(` after a local `use`
  would evade it; accepted because the repo convention (and every wired site)
  uses the fully-qualified form, and the exclusion is required so the guard does
  not count its own assertion strings.

### Break-the-code proofs
- **Phase 1 retain-on-failure (REQUIRED bite):** temporarily reverted
  `Transaction::execute_recovery`'s `FullReverse` arm to remove artifacts
  unconditionally (dropping the `if !state.all_complete() { return Err }`
  guard). `cargo test --lib test_rollback_retains_artifacts_on_failed_step`
  then FAILED (panic at `src/transaction/tests.rs:407` — the first run no longer
  surfaces an error / the recovery file no longer survives). Restored; test
  passes again.
- **Phase 8 keep-work switch-back fix (crash matrix bite):** temporarily
  reverted `commit_changes_with_rollback` to register the SwitchBranch step only
  when `head != original` (the pre-Phase-8 behavior). `cargo test --test
  e2e_crash_injection` then FAILED with `[after-push] worktree must be
  byte-identical to pre-run after recovery` (keep-work stranded the worktree on
  the GX branch). Restored; the full six-point matrix passes again. This proves
  both that the crash matrix bites and that the switch-back fix is load-bearing.
- Bite rationale for the other new tests (not separately re-run, cheap-revert
  cost not warranted; each targets an already-bite-proven earlier-phase fix):
  `test_execute_recovery_against_real_interrupted_run_file` exercises the same
  Phase 1/2 journal+phase machinery already bite-proven by the retain test;
  `test_finalize_stash_conflict_surfaces_stash_error` pins the Phase 2 Q2
  conflict policy (drop the stash and it disappears — the pre-Q2 footgun);
  `test_legacy_delete_remote_branch_file_executes_as_skipped_legacy` pins the
  Phase 2 retire-not-delete decision (remove the serde alias / no-op and the
  file fails to load or mutates a remote); the grep-guard fails the instant a
  seventh crash site or a stray `abort()` is added.

### Open questions
- None. No cross-repo/system-mutating bullets in Phase 8.
