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
