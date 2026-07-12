# CLI Shakedown Report: gx v0.4.0

Date: 2026-07-12. Binary: `~/.cargo/bin/gx` (`gx v0.4.0`). All fixtures built
in a throwaway `mktemp -d` under `/tmp` with bare-file remotes; no real GitHub
org and nothing under `~/repos` was touched.

## Summary

| Metric | Count |
|--------|-------|
| Top-level commands | 9 (+ 14 subcommands across rollback/create/review) |
| Commands exercised | 9 top-level + all new-surface subcommands |
| Passed | all exercised paths behaved correctly |
| Failed (product bug) | 0 |
| Cosmetic issues | 1 (raw eyre `Location:` on a not-found error) |
| Edge cases tested | 6 |
| Focus | new surface: `undo`, `review sync`, `doctor`, `rollback` |

Verdict: v0.4.0 is solid. The crash -> recover -> clean cycle, the fail-closed
confirmation gate, and the base-branch-untouched invariant all hold on the live
binary. No product defects found. One cosmetic error-message nit.

## New-surface results (the point of this shakedown)

### `gx doctor` — PASS
Reports git 2.53.0 / gh 2.46.0, the XDG log path, `RECOVERY (FAILED STEPS)`,
and `ORPHANED ARTIFACTS` (both `none` on a clean machine). Correctly separated
the failed-steps section from orphans.

### `gx rollback` (list / validate / execute) — PASS, incl. fail-closed prompt
Crash-injected a real `gx create` at `after-commit` (`GX_CRASH_POINT`, exit 134):
- `rollback list` showed the transaction with **phase `mutating`** (correct: pre-push),
  4 write-ahead steps, all `pending`.
- `rollback validate <tx>` is read-only: printed phase + steps, "safe to execute".
- `rollback execute <tx>` **without `--yes` on non-interactive stdin FAILED CLOSED**:
  "Refusing to execute recovery ... without confirmation on non-interactive stdin;
  pass --yes to proceed" (exit 1). This is the designed fail-closed gate.
- `rollback execute <tx> --yes` did a full reverse: repo restored to `main`,
  `config.json` reverted, the GX branch deleted locally, working tree clean,
  and the recovery artifacts removed (`rollback list` and `doctor` both clean after).
- `rollback cleanup --older-than 7d` -> "No recovery states to clean up".

### `gx undo` — PASS (fail-closed offline; base branches untouched)
`gx undo GX-SHAKE01 --yes` against the fixture: printed a correct per-repo plan
("pushed, no PR -> delete remote branch -> delete local branch"), then surfaced
the `gh: Not Found (HTTP 404)` per repo (the fixture's `testorg` repos do not
exist on real GitHub), reported "3 failed" accurately, did not crash, and
**left `main` on every bare remote byte-identical**. Nonexistent change-id ->
"Nothing to undo". Missing `<CHANGE_ID>` -> clap usage error.

### `gx review sync` — surface present
`review` now exposes `sync` ("True-up recorded change state against GitHub PR
reality"). Not exercised end-to-end because it requires a real GitHub org; the
help/wiring is correct and it is covered by the gh-shim unit/e2e tests.

## Create lifecycle (offline, bare remotes) — PASS
- Dry-run (`create --files config.json sub false true`, no `--commit`): previewed
  "3 would change", 3 matches, changed nothing (tree stayed clean).
- Real (`--commit --yes -x GX-SHAKE01 sub false true`): printed the blast-radius
  preview ("Targeting 3 repositories"), committed on `GX-SHAKE01` in all 3,
  pushed the branch to each bare remote, finalized back to `main` (clean), and
  left `rollback list` empty (recovery deleted on success).

## Discovery / status — PASS
- `gx status` on the fixture: 3 repos, aligned columns, `🟢` up-to-date.
- `gx status` in an **empty** tmp dir: "No repositories found matching the
  criteria" -> confirms the blast-radius fix (no upward walk to `~/repos`).

## Edge cases

| Case | Result |
|------|--------|
| `--change-id badid` (non-`GX-`) | Rejected at parse: "change-id must start with 'GX-' ... gx review finds PRs by the GX- prefix" |
| `undo` nonexistent change-id | "Nothing to undo for ..." (graceful, exit clean) |
| `rollback validate`/`execute` nonexistent tx | "Recovery state not found: <tx>" (exit 1) |
| `undo` with no `<CHANGE_ID>` | clap usage error |
| `cleanup --list` (no merged changes) | "No changes need cleanup." |
| `rollback cleanup` (nothing old) | "No recovery states to clean up" |

## Failures & Bugs
None (product).

## Cosmetic
- `rollback validate`/`execute` on a nonexistent tx prints a raw eyre trailer
  (`Location: src/transaction.rs:520:24`) beneath the clear "Recovery state not
  found" cause. Fine for a dev, slightly noisy for a user-facing not-found;
  consider suppressing the source location on expected not-found errors.

## Observations
- `undo` reverses remotes exclusively through `gh api` (with a `git ls-remote
  --exit-code` existence pre-probe). The probe works against any git remote, but
  the delete itself is GitHub-only, so a bare-file-remote fixture cannot complete
  remote deletion (it 404s). This is by design ("undo owns everything remote"
  via gh); documented here so a future reader does not mistake it for a bug.
- The `GX_CRASH_POINT` hook is inert without the env var and drove a clean,
  deterministic crash for the recovery test. Exactly the operator affordance the
  design intended.
- Confirmation gates are consistent: `create --commit` and `rollback execute`
  both print a plan and fail closed on non-TTY without `--yes`.

## Not exercised (need real GitHub, covered by tests)
`review sync/approve/delete/purge/clone/ls`, `create --pr`, `undo` remote
deletion + merged-PR revert. These are covered by the gh-PATH-shim + bare-remote
integration suites (`tests/e2e_undo_lifecycle.rs`, `e2e_crash_injection.rs`,
`e2e_f12_failclosed.rs`). A live smoke test against a scratch GitHub repo is the
only remaining gap and is optional.
