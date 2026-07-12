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
