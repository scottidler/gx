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
