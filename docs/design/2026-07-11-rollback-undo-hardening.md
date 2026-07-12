# Design Document: GX Rollback and Undo Hardening

**Author:** Claude Code (from 2026-07-11 safety audit session)
**Reviewer:** Scott Idler
**Date:** 2026-07-11
**Status:** Implemented
**Review Passes Completed:** 5/5
**External review:** Architect (Gemini) + Staff Engineer (Codex), 2026-07-11; all findings folded (see Resolved Decisions)

## Summary

A 2026-07-11 audit of gx's safety machinery (post workflow-safety-hardening,
v0.3.2) found the interrupted-run recovery core is sound but fails at three
seams: a partially failing rollback deletes the backups a retry needs, a
recovery file that survives a finalize crash undoes a successful run when
executed, and there is no undo at all once a run completes. This document
hardens all three seams and adds the missing safe-point primitives so gx can
be run against a fleet with confidence that every state is recoverable.

## Problem Statement

### Background

gx v0.3.2 carries the full 2026-06-11 workflow-safety-hardening implementation:
write-ahead `RollbackStep` recovery JSON, out-of-tree backups, per-repo locks,
incremental state saves, atomic writes, blast-radius confirmation. That work
targeted the create pipeline. The 2026-07-11 audit reviewed the recovery and
undo surfaces themselves (`transaction.rs`, `rollback.rs`, `state.rs`,
`lock.rs`, `cleanup.rs`) and found the guarantees stop exactly where a run
stops being happy-path.

The load-bearing observation: gx never commits to a repo's base branch. The
pre-run HEAD SHA per repo IS the safe point. Everything in this document is
bookkeeping to make returning to that safe point always possible and always
safe, in every state a run can be in:

- interrupted mid-mutation -> restore worktree (exists today, has defects)
- interrupted after push or mid-finalize -> finish restoring the environment
  WITHOUT undoing the pushed work (does not exist; today's recovery file is a
  footgun here)
- completed and pushed / PR'd / merged -> campaign-level undo (does not exist)

### Problem

Requirement source: Scott, 2026-07-11 ("harden the safety... safe points where
we can rollback undo or whatever. it needs to be rock solid"). Findings from
the 2026-07-11 audit (full inventory with file:line in Appendix A):

1. **A failed rollback destroys its own recovery data.** Both
   `Transaction::rollback` (`transaction.rs:169-186`) and `execute_recovery`
   (`transaction.rs:290-319`) count step failures, then unconditionally clear
   steps and delete the recovery file AND the backup directory. One failed
   `RestoreBackup` and the backups a retry needs are gone. [F1, Critical]
2. **A recovery file that survives finalize is a loaded gun.** After
   commit+push succeed, the recovery file still holds the full undo list
   including `DeleteRemoteBranch`. `gx rollback execute` on it deletes the
   pushed branch, restores pre-change content onto the original branch, and
   re-applies the already-applied stash. No restore-only mode, no staleness or
   PR check, no confirmation prompt. [F2, High]
3. **`atomic_write` strips file permissions.** `NamedTempFile` is 0600 and
   `persist` keeps that mode (`file.rs:108-116`). Every sub/regex rewrite of an
   executable flips 755 -> 600 and git commits the mode change fleet-wide.
   Backup restore loses the mode too. [F3, High]
4. **No undo for a completed run.** `finalize()` deletes the recovery file;
   undoing a campaign after success means hand-composing `review delete` +
   `cleanup --force`, and nothing handles a merged PR. [F4, High]
5. Assorted integrity defects: the stash violates write-ahead (F5), locks
   cover only `create` (F6), stale-lock reclaim has a TOCTOU (F7),
   `StateManager::save` is non-atomic (F8), transaction ids collide across
   processes (F9), `get_head_branch` failure is silently swallowed (F10),
   state never reconciles with GitHub reality and has no schema version (F11),
   a crash between push and state save leaves a pushed branch unrecorded
   (F12), error detection by string-sniffing (F13), `Failed`/`Abandoned`
   statuses are unreachable (F14), and no test kills a real process, runs the
   rollback CLI, or exercises remote-branch deletion (F15).

### Goals

- A rollback that partially fails NEVER deletes evidence; re-running converges.
- A recovery file always encodes which phase the run died in; executing it
  does the right thing for that phase and can never destroy pushed work.
- `gx undo <change-id>` undoes a completed campaign end-to-end: close PRs,
  delete branches, revert merged PRs via revert PRs, true-up state.
- File permissions survive every write and every restore.
- State files reconcile against GitHub reality on demand and carry a schema
  version.
- Every mutating gx surface takes the per-repo lock; every campaign-state
  read-modify-write takes a change-level lock.
- Crash windows are covered by tests that kill a real gx process.

### Non-Goals

- `Change::Llm` / propose-apply patchsets (stochastic changes). Separate
  design doc; this work is its prerequisite.
- An MCP server surface for gx (`mcp-io-rs`). Separate design doc; also
  depends on this work.
- Replacing git/gh shell-outs with libgit2/octocrab (re-affirmed from the
  2026-06-11 doc).
- Fleet resume ("skip repos already completed for this change-id on re-run").
  Parked: revisit if partial-fleet reruns become a real pain; today a re-run
  is safe (branch_existed guard) just wasteful.
- Windows support beyond not regressing.

## Proposed Solution

### Overview

**The new invariant, stated once: `gx rollback` never mutates a remote;
`gx undo` owns everything remote.** Rollback restores a single repo's worktree
and environment from a recovery file (its only remote interaction is one
read-only `ls-remote` probe to classify an ambiguous push). Undo reverses a
campaign (PRs, remote branches, merged work) from change state. Nothing in the
rollback path can change a remote, so no crash-window artifact can ever
destroy pushed work.

Eight phases, ordered by risk reduction. Phases 1-3 fix the machinery that
exists (evidence preservation, phase-aware recovery, write mechanics).
Phases 4-6 add the missing half of the story (safe-point records,
reconciliation, campaign undo, merged-PR revert). Phases 7-8 close concurrency
holes and prove the whole thing with crash-injection tests.

### Architecture

Three state stores, one new concept each:

```
$XDG_DATA_HOME/gx/
  recovery/<tx-id>.json    + phase: mutating|pushing|pushed|finalizing
                           + per-step status: pending|applied|done|failed:<err>|skipped-legacy
  changes/<change-id>.json + version: 1
                           + base_sha per repo (the safe point)
                           + statuses reconciled against GitHub on demand
  backups/<tx-id>/...      deleted ONLY when every step is done
```

**Recovery phases.** The transaction stamps its recovery file with the phase
it is entering, write-ahead like the steps themselves:

- `mutating` — from stash through commit. The work is incomplete and cheaply
  re-creatable by re-running gx. Recovery = full reverse execution (today's
  behavior, minus the defects).
- `pushing` — stamped before `git push` runs. Whether the work is shared is
  unknowable from the stamp alone (the panel's hardest question: a kill after
  the stamp but before the push completes). Recovery resolves it at execution
  time with a read-only probe: `git ls-remote --exit-code` for the GX branch.
  Absent -> treat as `mutating` (full reverse). Present -> treat as `pushed`.
- `pushed` — stamped after the push succeeds. The work is complete and shared.
  Recovery = restore the environment ONLY (execute `SwitchBranch` /
  `PopStash*` step kinds, skip the rest), report the retained branch, and
  point at `gx undo` if the user wants the work gone.
- `finalizing` — stamped entering `finalize()`. Same recovery behavior as
  `pushed`.

Consequence: **`DeleteRemoteBranch` leaves the active `RollbackStep`
vocabulary** (registered-before-push today, `create.rs:941`). The phase field
records the push; reversing a push is `gx undo`'s job, where the PR is closed
first by construction. This deletes the F2 footgun rather than guarding it.
Because `RollbackStep` derives `Deserialize` with `deny_unknown_fields`, the
variant cannot simply be deleted or old recovery files stop loading: it is
renamed `LegacyDeleteRemoteBranch` with `#[serde(alias = "DeleteRemoteBranch")]`,
never registered by new code, and interpreted as a no-op that marks the step
`skipped-legacy` and prints the `gx undo` hint.

**Per-step journal.** `RecoveryState.steps` entries gain a status. The
interpreter rewrites the recovery file (atomic) after each step. A failed step
keeps the file and backups on disk; a re-run skips `done`, retries
`failed`/`pending`. Artifacts are removed only when all steps are `done` (or
`skipped-legacy`).

Two-beat steps are journaled per beat so convergence never depends on an
operation that is not idempotent: `PopStash` executes `git stash apply <sha>`,
rewrites the journal to `applied`, then drops the stash and rewrites to
`done`. A crash between apply and the journal rewrite re-runs the apply; if
the tree already contains the stash content the apply conflicts, and that
conflict follows the finalize policy from the 2026-06-11 doc (Q2): keep the
stash, leave the apply result visible, mark the step `failed` with the stash
SHA in the error. A crash after `applied` retries only the drop.

**Safe-point record.** `RepoChangeState` gains `base_sha`: the pre-commit HEAD
of the base branch (same value `ResetCommit` already captures). `gx undo` and
audits can always state exactly what the safe point was, and assert the base
branch was never moved by gx.

**Campaign undo.** New `gx undo <change-id>`: reconcile against GitHub, print
a per-repo plan, prompt (fail-closed non-TTY, `--yes`), then per repo:

| Repo state (reconciled) | Undo action |
|---|---|
| live recovery file for this change-id | run the rollback interpreter first (per its phase), then continue below |
| PR open | close PR -> delete remote branch -> delete local branch |
| pushed, no PR | delete remote branch -> delete local branch |
| committed local only | delete local branch |
| PR merged | create revert branch from base -> revert the landed commit(s) -> push -> open revert PR (never touches the base branch directly) |
| already gone (manual cleanup) | record, skip |

The first row is load-bearing (panel finding): without it, undo on a
`mutating`-phase crash would classify the repo "committed local only," delete
the branch, and strand the user's WIP in an un-recorded stash. Undo invokes
the same interpreter `rollback execute` uses, in-process, under the same
per-repo lock.

Sources: the change state file, plus any recovery files carrying the same
change-id (covers a crash between push and state save, F12). Local repos are
resolved via recorded `local_path` first (cleanup precedent); a missing path
is reported, not skipped. On success the change state is marked `Abandoned`
(merged-PR repos: `RevertPrOpen` noted per repo). Output uses the same
unified-results rendering and progress UX as `review`.

**Merged-PR revert (mechanics).** gx's own `review approve` always merges
`--squash --delete-branch` (`github.rs:535`), so the common landed shape is a
single squash commit; merges performed outside gx may be true merge commits.
`list_prs_by_change_id` gains the GraphQL fields `state`, `mergedAt`,
`mergeCommit { oid }`, `baseRefName` (today it returns open PRs only and
`PrInfo` carries none of these, `github.rs:300,396`). Revert = branch
`revert/<change-id>` off the base branch head; `git revert <oid>` when the
merge commit has one parent (squash/rebase), `git revert -m 1 <oid>` when it
has two (true merge); push; open a revert PR whose body links the original.
An existing `revert/<change-id>` branch fails that repo with a message naming
the branch (no reuse, no force). A revert that conflicts is reported per repo
and the revert branch is left in place for manual resolution; undo never
force-resolves.

### Data Model

**RecoveryState** (`recovery/<tx-id>.json`) — additive:

```json
{
  "version": 1,
  "transaction_id": "gx-tx-1760000000-84213-7",
  "change_id": "GX-2026-07-11T12-30-00",
  "repo_path": "/home/u/repos/org/repo",
  "created_at": "...",
  "phase": "pushed",
  "branch": "GX-2026-07-11T12-30-00",
  "steps": [
    { "step": { "SwitchBranch": { "repo": "...", "branch": "main" } }, "status": "done" },
    { "step": { "RestoreBackup": { "backup": "...", "original": "...", "mode": 493 } }, "status": "failed", "error": "..." }
  ]
}
```

- `version`: serde default-fn returning 1 so existing version-less files load;
  written on every save. Same field added to `ChangeState`.
- `branch`: the GX branch name, so phase reporting, the `pushing` probe, and
  `gx undo`'s recovery-file sweep don't re-derive it.
- Transaction id gains the pid: `gx-tx-<ts>-<pid>-<counter>` (F9).
- New step variant `PopStashByMessage { repo, message }`: persisted BEFORE
  `git stash push -u` runs (closing F5's write-ahead gap); replaced by
  `PopStash { stash_sha }` right after the stash exists. Same two-beat
  apply/drop journaling as `PopStash`.
- `RestoreBackup` gains `mode: u32` captured at backup time; restore applies it.
- `DeleteRemoteBranch` -> `LegacyDeleteRemoteBranch` no-op (see Architecture).

**ChangeState** (`changes/<change-id>.json`) — additive:

- `version: 1` (same serde default-fn scheme).
- `RepoChangeState.base_sha: Option<String>`.
- `RepoChangeState.status` gains `RevertPrOpen`; `ChangeStatus` `Failed` and
  `Abandoned` become reachable (`mark_failed` updates the aggregate; `gx undo`
  sets `Abandoned`).
- Saved via `atomic_write` (F8); every read-modify-write holds the
  change-level lock (see Phase 7).
- Write order per repo: stamp `phase: pushing` -> push -> stamp
  `phase: pushed` -> save state entry -> stamp `phase: finalizing` ->
  finalize -> delete recovery file. A pushed branch is therefore always
  recorded in at least one of the two stores (F12). This moves the state save
  INSIDE `process_single_repo` (today it happens later, in the outer rayon
  fold at `create.rs:250`); the `StateManager` + mutex are passed into the
  per-repo path and the outer fold becomes display-only. Named here because it
  changes the per-repo function contract, not just an implementation detail
  (panel finding).

### API Design

| Surface | Today | After |
|---|---|---|
| `gx rollback execute <tx>` | runs immediately, no prompt, full undo regardless of phase, can delete remote branches | prints the plan (phase, steps, age); prompts unless `--yes` (fail-closed non-TTY); phase-aware; never mutates a remote. Exit 0 when its (possibly keep-work-limited) mandate completes, including the `pushed`/`finalizing` handoff that retains work; non-zero when any step failed |
| `gx rollback list` | id + step counts | adds phase, per-step status summary, age |
| `gx rollback validate <tx>` | repo-exists check | adds phase and per-step status report |
| `gx undo <change-id>` | absent | new: reconciled plan -> prompt -> drain recovery files / close PRs / delete branches / revert-PR merged ones |
| `gx review sync <change-id>` | absent | new: true-up PR states (merged/closed via gh), update aggregate status |
| failed rollback | deletes recovery file + backups | retains both, journals per-step status, re-run converges |
| `atomic_write` | strips mode to 0600 | preserves existing file mode; new files 0644 |
| locks | per-repo, `create` only | per-repo: also `rollback execute`, `cleanup`, `review clone`, `undo`; new change-level lock around every `changes/<id>.json` read-modify-write |

No new config keys. `--yes` semantics copy `create`/`purge`.

### Implementation Plan

#### Phase 1: Rollback never destroys evidence [F1]
**Model:** opus

- [ ] Per-step status journal in `RecoveryState` (see Data Model); interpreter
      rewrites the file (atomic) after each step in both `Transaction::rollback`
      and `execute_recovery`; two-beat journaling for `PopStash`/
      `PopStashByMessage` (`applied` between apply and drop).
- [ ] On any `failed` step: do NOT clear steps, do NOT delete the recovery
      file or backup dir. Print exactly what failed and the re-run command.
- [ ] Re-run skips `done`, executes `failed` + `pending`, retries only the
      drop for `applied`; artifacts removed only when all steps are `done` or
      `skipped-legacy`.
- [ ] `gx doctor` reports recovery files with failed steps distinctly from
      orphans (they are NOT purge candidates inside the TTL).
- **Success criteria:** `test_rollback_retains_artifacts_on_failed_step` —
  with one injected failing step, recovery JSON + backup dir survive and carry
  the failure; a second run with the failure removed converges and cleans up;
  `test_popstash_applied_state_skips_reapply` — a journal at `applied` retries
  only the drop.

#### Phase 2: Phase-stamped recovery, remote-safe execute [F2, F5]
**Model:** opus

- [ ] `phase` field written write-ahead: `mutating` (tx creation), `pushing`
      (before `git push`), `pushed` (after push success), `finalizing`
      (entering `finalize()`).
- [ ] Rename `DeleteRemoteBranch` -> `LegacyDeleteRemoteBranch`
      (`#[serde(alias = "DeleteRemoteBranch")]`), delete its registration
      site, interpret as no-op -> `skipped-legacy` + `gx undo` hint. The
      rollback interpreter contains no remote-mutating call.
- [ ] `rollback execute` dispatches on phase: `mutating` = full reverse;
      `pushing` = `ls-remote --exit-code` probe for the recorded branch ->
      absent = full reverse, present = keep-work; `pushed`/`finalizing` =
      keep-work (execute only `SwitchBranch`/`PopStash*` kinds, report the
      retained branch, name `gx undo`). Keep-work handoff exits 0.
- [ ] Confirmation prompt on `execute` (plan, then y/N; `--yes`; fail-closed
      non-TTY). `--force` keeps meaning "skip validation" only.
- [ ] `PopStashByMessage` persisted before the stash op (F5); swap to
      `PopStash{sha}` after; fix the `transaction.rs` module-doc overclaim.
- **Success criteria:** hand-authored `finalizing`-phase recovery file with a
  pushed bare-remote fixture -> `execute` restores branch+stash and the remote
  branch still exists; `pushing`-phase file with NO remote branch -> full
  reverse; with the remote branch present -> keep-work; grep-proof: no code
  path from `rollback` to a remote-mutating git/gh invocation.

#### Phase 3: Write mechanics [F3, F8, F9, F10]
**Model:** sonnet

- [ ] `atomic_write` preserves the existing target's mode (stat before, apply
      to temp before persist); new files get 0644 explicitly (set, not
      umask-inherited). `RestoreBackup` records and restores mode.
- [ ] `StateManager::save` -> `atomic_write`.
- [ ] Transaction id: `gx-tx-<ts>-<pid>-<counter>`.
- [ ] `get_head_branch` failure in `process_single_repo` is a hard per-repo
      error (kill the `if let Ok` swallow at `create.rs:489`).
- **Success criteria:** `test_atomic_write_preserves_mode` (0755 in, 0755
  out), `test_atomic_write_new_file_mode_under_restrictive_umask` (0644
  regardless of umask), `test_restore_backup_restores_mode`; e2e sub run over
  an executable shows no mode change in `git status --porcelain`; tx-id embeds
  the pid.

#### Phase 4: State integrity and reconciliation [F11, F12, F14]
**Model:** sonnet

- [ ] `version: 1` on `ChangeState` + `RecoveryState` (serde default-fn for
      old files).
- [ ] `base_sha` recorded per repo at commit time.
- [ ] Control-flow refactor (named, panel finding): state save moves inside
      `process_single_repo` per the Data Model write order; `StateManager` +
      mutex passed in; outer rayon fold becomes display-only.
- [ ] `gx review sync <change-id>`: gh PR lookups -> `mark_merged`/`mark_closed`,
      aggregate `ChangeStatus` updated; `mark_failed` updates the aggregate so
      `Failed` is reachable. `PrInfo` + the change-id GraphQL query gain
      `state`, `mergedAt`, `mergeCommit { oid }`, `baseRefName`.
- [ ] `rollback cleanup --older-than` and `cleanup --all` operate on the
      trued-up statuses.
- **Success criteria:** version-less fixture files load; a PR merged via gh
  (shimmed) shows `Merged` after `review sync`; kill-after-push fixture leaves
  the pushed branch recorded in state or recovery (asserted both orders).

#### Phase 5: `gx undo <change-id>` core [F4]
**Model:** opus

- [ ] New `undo.rs` + subcommand: reconcile (Phase 4 sync) -> plan table
      (repo | state | action) -> prompt (`--yes`, fail-closed) -> execute per
      repo under the repo lock, parallel with `review`-style progress and
      unified results.
- [ ] Recovery-file drain first (panel finding): any live recovery file for
      the change-id is executed via the rollback interpreter (per its phase)
      before the campaign action; sources = change state plus recovery files
      matching the change-id; local repos via recorded `local_path` first.
- [ ] Actions: close open PR -> delete remote branch -> delete local branch;
      pushed-no-PR and local-only rows per the Architecture table. Merged rows
      are REPORTED as "requires revert (next phase)" until Phase 6 lands, and
      are never silently skipped.
- [ ] Partial failure: per-repo results reported like create; state updated
      per repo; re-running `undo` converges (already-gone rows skip).
- [ ] Sets `Abandoned` on completion (merged rows pending revert hold the
      aggregate at `PartiallyMerged`).
- **Success criteria:** e2e (bare remotes + gh shim): open-PR campaign undone
  end-to-end, base branches byte-identical before/after; a `mutating`-phase
  recovery file in the campaign is drained (stash restored) before its branch
  is deleted; second `undo` run is a no-op.

#### Phase 6: Merged-PR revert path [F4]
**Model:** opus

- [ ] Revert mechanics per Architecture: `revert/<change-id>` branch off base
      head; parent-count dispatch (`git revert <oid>` vs `-m 1`); push; revert
      PR linking the original; collision -> per-repo failure naming the
      branch; conflict -> report + leave branch, never force-resolve.
- [ ] Per-repo status -> `RevertPrOpen`; aggregate `Abandoned` once every
      merged row has a revert PR open.
- **Success criteria:** merged-PR fixture (squash) produces a revert PR whose
  diff is the inverse of the original; true-merge fixture reverts with `-m 1`;
  pre-existing `revert/<change-id>` branch fails that repo with the naming
  message and touches nothing.

#### Phase 7: Lock coverage and reclaim race [F6, F7, F13]
**Model:** sonnet

- [ ] `RepoLock` taken by `rollback execute`, `cleanup`, `review clone`, and
      `undo` (same acquire/fail-fast semantics as create).
- [ ] New change-level lock (`locks/change-<fnv-of-change-id>.lock`, same RAII
      shape) held around every read-modify-write of `changes/<id>.json`:
      `review sync`, `review approve/delete`, `cleanup`, `undo`, and the
      create-path incremental saves (panel finding: atomic save prevents torn
      files, not lost updates).
- [ ] Reclaim TOCTOU fix: reclaim by atomic rename to a unique name, re-verify
      staleness on the renamed file, then remove; a losing racer sees ENOENT
      and retries acquire. Never `remove_file` a path another process may have
      re-created.
- [ ] Replace string-sniffed git errors (`contains("not found")`,
      `contains("remote ref does not exist")`) with explicit existence checks
      (`git show-ref`, `ls-remote --exit-code`) before the destructive op.
- **Success criteria:** two-process contention test (spawned binaries) shows
  exactly one winner and one fast failure naming the holder; reclaim race test
  never deletes a live lock; concurrent `review sync` + `undo` on one
  change-id lose no updates.

#### Phase 8: Crash-injection tests [F15]
**Model:** opus

- [ ] `GX_CRASH_POINT=<name>` hook: compiled in, inert unless the env var is
      set; the process aborts at named points — `after-stash`, `after-branch`,
      `after-commit`, `before-push` (after the `pushing` stamp), `after-push`,
      `mid-finalize`.
- [ ] e2e: fixture org with bare remotes; for each crash point, spawn the real
      `gx create --commit`, let it die, assert `gx rollback list` shows the
      right phase, run `execute`, assert worktree byte-identical (content AND
      modes) to pre-run and remote state correct for the phase (branch
      retained for `after-push`/`mid-finalize`; absent for `before-push` after
      the probe dispatches full reverse; absent for earlier points).
- [ ] Direct tests for: `execute_recovery` against a real interrupted-run
      file, the finalize stash-conflict path (`FinalizeOutcome.stash_error`),
      rollback CLI list/validate output, legacy `DeleteRemoteBranch` file
      loading as `skipped-legacy`.
- [ ] Break-the-code proof: each new safety test demonstrated to fail against
      the pre-fix code (recorded in implementation notes).
- **Success criteria:** the crash-point matrix passes for all six points;
  reverting the Phase 1 retain-on-failure change makes
  `test_rollback_retains_artifacts_on_failed_step` fail (bite proof).

## Acceptance Criteria

- [ ] A rollback with an injected failing step leaves the recovery file and
      backup dir on disk with per-step status recorded; a second `gx rollback
      execute` converges and only then removes artifacts.
- [ ] For every `GX_CRASH_POINT` (all six), `gx rollback execute` restores the
      worktree byte-identical (content and permissions) to pre-run state, and
      no crash point can result in rollback deleting a remote branch
      (grep-provable: no remote-mutating call reachable from `rollback`).
- [ ] `atomic_write` on a 0755 file yields a 0755 file; a fleet sub run over
      executables produces zero mode changes in `git status --porcelain`.
- [ ] `gx undo <change-id>` against an open-PR campaign closes every PR,
      deletes every gx branch (remote and local), drains any live recovery
      files first, leaves every base branch untouched, and marks the change
      `Abandoned`; against a merged PR it opens a revert PR.
- [ ] Pre-existing version-less state and recovery files (including ones
      containing `DeleteRemoteBranch` steps) still load; all new writes carry
      `version: 1` and go through `atomic_write`.

## Resolved Decisions

- **2026-07-11, author (pass 2): rollback never mutates remotes.**
  `DeleteRemoteBranch` is retired instead of guarded. Once pushed, work is
  kept by recovery and reversed only by `gx undo` (which closes the PR first).
  Rationale: a guard can be bypassed; an absent code path cannot. Endorsed by
  both reviewers.
- **2026-07-11, panel (both, must-fix), folded: legacy step compatibility.**
  Deleting the variant breaks deserialization of existing recovery files under
  `deny_unknown_fields`. Kept as `LegacyDeleteRemoteBranch` no-op via serde
  alias; `skipped-legacy` added to the status vocabulary.
- **2026-07-11, panel (both, must-fix), folded: `PopStash` two-beat journal.**
  Apply-then-drop is not idempotent across a crash between the two; the
  journal now records `applied` between beats and a re-run retries only the
  drop. The doc no longer claims pure idempotency for stash restore; the Q2
  conflict policy covers the re-apply edge.
- **2026-07-11, panel (Staff, must-fix; Architect initially disagreed,
  resolved by evidence for Staff), folded: `pushing` phase.** Stamping
  `pushed` before the push runs made an unpushed crash look shared. The
  pre-push stamp is now `pushing`, and recovery classifies it at execution
  time with a read-only `ls-remote` probe. `before-push` added to the crash
  matrix.
- **2026-07-11, panel (Architect, critical), folded: undo drains recovery
  files first.** Without it, undo on a `mutating`-phase crash orphans the
  user's stash. Undo invokes the rollback interpreter in-process before any
  campaign action.
- **2026-07-11, panel (Staff), folded: the Phase 4 state-save move is a named
  control-flow refactor** of `process_single_repo`'s contract, not an
  implementation detail.
- **2026-07-11, panel (Staff), folded: Phase 5 split.** Undo core and
  merged-PR revert are separate phases; revert mechanics (GraphQL fields,
  parent-count dispatch, collision and conflict behavior) specified. Both
  reviewers agreed revert stays in v1 (deferring would half-solve F4).
- **2026-07-11, panel (Staff), folded: change-level lock.** Per-repo locks do
  not protect `changes/<id>.json` read-modify-writes; a change-level lock
  closes the lost-update race (Phase 7).
- **2026-07-11, panel (both), folded: 0644 rationale corrected.**
  `atomic_write` does write repo content, but NEW files come only from
  `gx add` (user-supplied config/text content); existing files keep their
  mode. 0644 is set explicitly with an umask-controlled test proving it is
  intentional. Umask-derivation rejected as platform-dependent complexity.
- **2026-07-11, panel (Architect), folded:** keep-work handoff exits 0;
  `gx undo` uses `review`-style progress and unified results.
- **2026-07-11, author (pass 2): `pushing`/`pushed`-phase recovery keeps the
  work.** Before push the work is cheap to re-create (re-run gx); after push
  it is shared; the probe resolves the boundary. Full reverse execution
  applies only to `mutating` (and probe-refuted `pushing`).
- **2026-07-11, author (pass 4): merged-PR revert ships in v1** (now its own
  phase). Conflict handling: report + leave the revert branch, never
  force-resolve.

## Alternatives Considered

### Alternative 1: SQLite journal instead of per-tx JSON files
- **Description:** one `gx.db` with transactions/steps/status tables.
- **Pros:** real transactions for the journal itself; queryable history.
- **Cons:** new dependency and failure domain; recovery tooling must work when
  gx itself is broken (a human can read/edit JSON with an editor); migration
  of the existing scheme; WAL/lock interplay across processes.
- **Why not chosen:** the JSON files with atomic rewrite are already
  crash-safe enough for this write volume, and human-inspectable recovery
  state is a feature when things are on fire.

### Alternative 2: Rely on git reflog / stash list as the recovery source
- **Description:** skip gx-side journaling; reconstruct undo from git's own
  records.
- **Pros:** less state to maintain; git already records everything local.
- **Cons:** nothing records the cross-repo campaign or remote effects (pushed
  branches, PRs); reflog expiry; reconstructing "what did gx do" from reflog
  is exactly the string-archaeology the 2026-06-11 doc killed.
- **Why not chosen:** git records local history, not intent; the typed step
  journal is the intent record. Reflog remains the manual escape hatch.

### Alternative 3: Guard `DeleteRemoteBranch` with a PR check instead of retiring it
- **Description:** keep remote deletion in rollback; refuse when a PR exists
  (the pass-1 draft design).
- **Pros:** rollback stays a single full-undo mode.
- **Cons:** the guard depends on gh availability and staleness at exactly the
  worst moment; `--force` semantics get murky; the footgun survives as a
  narrower footgun.
- **Why not chosen:** deleting the code path is strictly stronger than
  guarding it, and `gx undo` covers the legitimate use.

### Alternative 4: `gx undo` deletes merged work by force-pushing base branches
- **Description:** true inverse: reset base branches to `base_sha` and
  force-push.
- **Pros:** literal restoration of the safe point.
- **Cons:** rewrites shared history; races other people's commits; violates
  every git-safety rule in the house.
- **Why not chosen:** revert PRs are the only responsible undo for merged
  work. Force-push is never on the table.

## Technical Considerations

### Dependencies
- No new crates expected. Mode preservation uses `std::fs::Permissions`;
  reclaim rename uses `std::fs::rename`. Any surprise dependency goes through
  `cargo add`.

### Performance
- Per-step atomic rewrite of the recovery file adds ~6-8 small writes per repo
  during rollback only. The `pushing` probe is one `ls-remote` per ambiguous
  recovery. `review sync`/`undo` add gh calls proportional to repo count,
  parallelized like `review` today.

### Security
- Rollback structurally cannot delete a remote branch, so no recovery artifact
  can silently close someone's in-review PR by deleting its head.
- `gx undo` never force-pushes and never touches base branches; merged work is
  reverted via PRs subject to normal review.
- Lock coverage on all mutating surfaces removes the rollback-vs-create
  interleaving; the change-level lock removes cross-command state clobbering.

### Testing Strategy
- Unit: journal round-trip (incl. `applied` and `skipped-legacy`), phase
  dispatch table, probe dispatch, mode preservation (incl. umask-controlled),
  tx-id format, reclaim rename race, sync status mapping, parent-count
  dispatch.
- Integration/e2e: six-point crash matrix with the real binary (Phase 8),
  undo lifecycle with bare remotes + a `gh` PATH shim (open, merged-squash,
  merged-true-merge fixtures), two-process lock contention.
- Every phase lands with its bite-proof (test shown failing on pre-fix code).

### Rollout Plan
- One phase per PR, merged in order; each independently releasable via `bump`
  per the repo's release flow.
- Recovery/state schema changes are additive with serde defaults + the legacy
  alias; no migration step. Old binaries reading new files fail loudly
  (deny_unknown_fields), which is correct: do not run mixed gx versions
  against one data dir.
- Changelog calls out the behavior changes: `rollback execute` now prompts
  (`--yes` for automation), never mutates remotes, and `gx undo` is the new
  remote-reversal surface.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Phase machinery bug worse than the footgun it fixes | Medium | High | Phase 2 is opus-modeled, ships alone, six-point crash matrix covers every phase transition including `before-push` |
| Revert-PR path meets un-revertable merges (conflicts) | Medium | Medium | undo reports the conflict per repo and leaves the revert branch for manual resolution; never force-resolves |
| `GX_CRASH_POINT` hook leaks into production behavior | Low | Medium | hook is a no-op unless the env var is set; grep-guard test asserts no other call sites |
| gh shim tests diverge from real gh | Medium | Medium | shim asserts exact argv (2026-06-11 precedent); one env-gated live smoke test |
| Prompt on `rollback execute` breaks scripted recovery | Low | Low | `--yes` documented in the changelog and `--help` |
| `ls-remote` probe fails (offline) during `pushing` recovery | Low | Medium | fail closed: refuse to dispatch, report "cannot classify push state offline," artifacts retained for a retry |

## Open Questions

- (none — all panel findings folded; see Resolved Decisions)

## References

- 2026-07-11 safety audit (session) — findings F1-F15.
- 2026-07-11 review panel: Architect (Gemini) + Staff Engineer (Codex);
  synthesis folded into Resolved Decisions.
- `docs/design/2026-06-11-workflow-safety-hardening.md` — prior art; this doc
  extends it and re-affirms its Alternatives.
- `docs/design/2026-06-11-workflow-safety-hardening-implementation-notes.md`
  — Phase 3/8 open questions that F5/F15 formalize.
- Rust conventions: `~/repos/.claude/rules/rust.md` (atomic writes, typed
  seams, fail-closed defaults, test placement).

## Appendix A: Finding Inventory (traceability)

Severity: C=critical, H=high, M=medium, L=low. All verified against v0.3.2
(`4e52edf`).

| # | Sev | Finding | Location | Phase |
|---|-----|---------|----------|-------|
| F1 | C | Failed rollback deletes recovery file + backups unconditionally | `transaction.rs:169-186`, `:290-319` | 1 |
| F2 | H | Post-finalize-crash recovery file undoes a successful run; no prompt on execute | `transaction.rs:191-222`, `rollback.rs` | 2 |
| F3 | H | `atomic_write` strips mode to 0600; restore loses mode | `file.rs:108-116` | 3 |
| F4 | H | No campaign-level undo after success; nothing for merged PRs | (absent) | 5, 6 |
| F5 | M | Stash created before its step can be persisted; module doc overclaims | `create.rs:455-465`, `transaction.rs:5-8` | 2 |
| F6 | M | Locks only on create; rollback/cleanup/review-clone mutate unlocked | `create.rs:422` (sole site) | 7 |
| F7 | M | Stale-lock reclaim TOCTOU can delete a live lock | `lock.rs:127-147` | 7 |
| F8 | M | `StateManager::save` plain `fs::write`; torn file hides campaign | `state.rs:269` | 3 |
| F9 | M | Tx id `<ts>-<counter>` collides across processes | `transaction.rs:89-91` | 3 |
| F10 | M | `get_head_branch` failure swallowed; mutates whatever branch user was on | `create.rs:489` | 3 |
| F11 | M | No GitHub reconciliation; no schema version field | `state.rs`, `review.rs` | 4 |
| F12 | M | Crash between push and state save leaves pushed branch unrecorded | `create.rs:253-267` | 4 |
| F13 | L | String-sniffed git errors | `cleanup.rs:268`, `git.rs:1369` | 7 |
| F14 | L | `Failed`/`Abandoned` unreachable; failed campaigns never age out | `state.rs:337-340` | 4 |
| F15 | L | No test kills a real process, runs the rollback CLI, or executes remote deletion | `tests/`, `transaction/tests.rs` | 8 |

## Implementation Audit Addendum (2026-07-12)

The 8 phases landed one commit each (`743f3f3`..`b515570`), then a review-panel
implementation audit ran (Architect/Gemini + Staff Engineer/Codex, Mode 2).
Split verdict: Architect PASS (zero gaps); Staff Engineer found unhappy-path
robustness gaps. Both confirmed the load-bearing invariant (no remote-mutating
call reachable from `rollback`). Findings were adjudicated against the code and
resolved over two hardening commits, then re-audited to consensus.

### Findings and resolution

- **A1 [Critical] F12 not fail-closed.** `record_pushed_state` was best-effort
  and `finalize()` deleted the recovery file unconditionally, and
  `StateManager::new()` degraded to `None` with a warning in commit mode. On a
  double fault (push succeeds, then state save fails or the manager is absent) a
  pushed branch was recorded in NEITHER store, breaking the design's F12
  guarantee. **Fixed** (`15f2397`): commit-mode `StateManager::new()` failure is
  now a hard error before any repo is mutated; `record_pushed_state` returns a
  result and the recovery file is deleted ONLY when the pushed safe point is
  durably saved (else `finalize_retaining_recovery()` keeps it and the repo is
  reported `Committed` with the retained path). Invariant now enforced: recovery
  file deleted implies state contains the repo.
- **A2 [Med] undo offline merged-state hazard.** When GitHub reconcile failed,
  `gx undo` could plan branch-deletion instead of a revert for a merged PR.
  **Fixed** (`15f2397`): an org whose PR state cannot be verified is held
  `UnverifiedOffline` (no remote mutation); local-only cleanup and recovery
  drains still proceed.
- **A3 [Med] `cleanup_old` TOCTOU.** The first fix locked the delete but decided
  from a stale pre-lock `list()` snapshot, so a change revived between the
  listing and the lock could be wrongly deleted. **Fixed** (`f8a3d26`):
  `cleanup_if_stale` reloads the change file under its `ChangeLock` and
  re-evaluates the predicate on the fresh copy; lock now covers reload +
  re-check + delete.
- **A4 [new gap] undo stranded a recovery-only pushed REMOTE.** The A1 retain
  path points the user at `gx undo`, but undo refused to run without a
  change-state file and classified a recovery-only repo as local-only regardless
  of recovery phase, orphaning the pushed remote branch. **Fixed** (`f8a3d26`):
  undo runs from recovery files alone; a recovery-only `Pushed`/`Finalizing`/
  `Pushing` repo is classified pushed-no-PR and its REMOTE branch is deleted
  (pre-probed with `ls-remote --exit-code`) before the recovery record is
  discarded; a `Mutating`-phase recovery-only repo stays local-only.
- **[Pushed back] review approve/delete best-effort state save.** A state-save
  failure after a GitHub merge/delete is logged, not surfaced on the result.
  Left as-is by design: `gx review sync` is the reconciliation mechanism that
  trues this up, so the loop is self-healing. Not a defect.

Each fix carries a biting test with a break-the-code proof (recorded in the
implementation-notes file). The Staff Engineer re-audited both hardening commits
and confirmed all findings CLOSED with no new gap. Consensus reached; no open
questions remain.

**Shipped in:** hardening commits `15f2397` and `f8a3d26` on top of the 8 phase
commits. (Release version recorded here once the tag is cut.)
