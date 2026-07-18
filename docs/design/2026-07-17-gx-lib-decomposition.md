# Design Document: gx lib decomposition (local / remote)

**Author:** Scott Idler
**Date:** 2026-07-17
**Status:** Implemented
**Review Passes Completed:** 1/5 self + review-panel (Architect + Staff, both rc=0); consensus loop: all 6 findings folded, no escalations
**Track:** B0 of 2-in-B (lands after Track A `2026-07-17-gx-onto-mcp-io.md`; is a prerequisite for `2026-07-17-gx-intel-catalog.md`)

## Summary

Decompose the single `gx` lib crate into a credential-free `local` and a credential-bound `remote`. This is a pure refactor: no behavior change, `otto ci` green throughout. Its purpose is to make the cross-org boundary of the intel layer (Track B1) COMPILER-structural: `catalog` will depend on `local` only, so an intel tool physically cannot name `persona`/`github`/`ssh`/remote-git.

## Problem Statement

### Background

- gx is a single lib crate; every module can see every other (all `pub`, `src/lib.rs`).
- Track B1 (intel catalog) adds 4 read-only tools that must never reach credential/remote code, per the permanent cross-org-operations non-goal.
- Review-panel (2026-07-17) proved a same-crate "boundary" is not structural: the intel handler holds `Arc<Config>` and can call `persona::resolve_token_env`, `github::*`, `git::fetch_origin` because they are `pub` in the same crate.

### Problem

There is no crate boundary separating credential-free local logic (repo/git/file) from credential-bound remote logic. Without one, "intel cannot reach credentials" is a convention, not a compiler guarantee.

### Goals

- Split the `gx` lib into `local` (credential-free) + `remote` (credential-bound, depends on `local`).
- `local` has ZERO dependency on `ssh`/`persona`/`github` or any remote-git function.
- No behavior change; every existing test passes; `otto ci` green at each phase.
- Single flat version across all workspace crates (per git.md tag rule).

### Non-Goals

- **The intel crate + tools.** That is Track B1, built on `local` after this lands.
- **Any functional change.** Pure module/crate reorganization.
- **Splitting beyond local/remote.** Two crates (plus the existing bin), not a crate per module.

## Proposed Solution

### Overview

Lands after Track A (which moves the MCP handler into `gx/src/mcp/`). Target workspace:

- `local` (lib): `repo`, `config`, `subprocess`, `hash`, `utils`, `bare`, `diff`, `user_org`, `file`, `test_utils` (behind `#[cfg(test)] pub mod`), and the LOCAL half of `git`.
- `remote` (lib, depends on `local`): `ssh`, `persona`, `github`, the REMOTE half of `git`, plus `create`, `review`, `checkout`, `clone`, `cleanup`, `undo`, `rollback`, `transaction`, `state`, `doctor`, `status`, `output`, `cli`, and the migrated `mcp` handler.
- `gx` (bin): thin shim, depends on `remote`.

### Architecture

The load-bearing move is splitting `git.rs` (2592 lines) along the local/remote seam. **The seam is by transitive CALL GRAPH, not by function name or top-level import** (review-panel 2026-07-17 caught several status/branch functions that look local but transitively `git fetch`/`pull`). Phase 0 produces the authoritative classification; the lists below are the review-confirmed anchors it starts from, not a finished table.

**A new local-only status API is required, not a move.** `get_repo_status_with_options` (git.rs:94) is fetch-capable (it calls `get_remote_status_with_fetch` -> `git fetch` when `fetch_first`), so it CANNOT go to local. Track B1's walk needs local status, so `local` gets a NEW `get_repo_status_local(repo)` that computes branch/dirty/ahead-behind from local tracking refs only (the `get_remote_status_native` + `parse_branch_tracking_info` path), never fetching. The fetch-capable entry stays in `remote`.

- **-> `local` (review-verified local, no transitive remote call):** the NEW `get_repo_status_local`; `get_remote_status_native` (:333), `parse_branch_tracking_info` (:272), `get_current_branch` (:156), `get_current_commit_sha` (:134), `get_detached_head_info` (:182), `parse_porcelain_status` (:210), `run_status_porcelain` (:243), `get_status_changes` (:265), `get_remote_origin` (:951, local `git config` read), `is_same_repo` (:974), `resolve_branch_name` (:564), `get_default_branch_local` (:573), plus the pure-local ops (`add_files`, `commit_changes`, `has_uncommitted_changes`, `get_current_branch_name`, `branch_exists_locally`, `delete_local_branch`, `get_head_sha`, `stash_*`, `reset_hard_to_sha`, `switch_branch`, `force_switch_branch`, `list_index_files`, `worktree_*`, `stage_all`, `resolve_worktree_repo`, `diff_cached_*`, `commit_parent_count`, `create_branch_at`, `revert_commit`) + the `RepoStatus`/`RemoteStatus`/`BranchTrackingInfo`/`StatusChanges` types. Each pure-local op is call-graph-confirmed in Phase 0 before it moves.
- **-> `remote` (review-verified remote):** `get_repo_status_with_options` (:94, fetch-capable), `get_remote_status_with_fetch` (:405), `checkout_branch` (:434, `git pull --ff-only`), `create_branch` (:997, probes + checks out remote), `branch_merged_into_base` (:1083, fetches via `get_head_branch`), `get_head_branch` (:1458), `clone_or_update_repo` (:655), `clone_repo` (:725), `update_existing_repo` (:840, `github::get_default_branch`), `push_branch` (:1262), `branch_exists_on_remote` (:1359), `checkout_remote_branch` (:1376), `pull_latest*` (:1408/:1724), `clone_repository` (:1425), `branch_exists_remotely` (:1488), `remote_branch_exists_probe` (:1511), `delete_remote_branch` (:1544), `fetch_origin` (:1618), the ssh connectivity test (:733).

**Module placement on the seam:** `file` (imports `crate::{diff,git}`; the B1-reused `read_utf8_or_skip`/`validate_new_file_path`/`FileSet::candidates` are all local-only, needing only `git::list_index_files`) -> `local` (in Phase 2, after the git split, so its `git` dep points at `local::git`). `test_utils` (imported by moved modules' tests: repo.rs:498, bare/tests.rs, config/tests.rs) -> `local` behind `#[cfg(test)] pub mod`, in Phase 1, so the moved modules' test targets compile green.

### Data Model

N/A. No persistent state.

### API Design

Internal only. No CLI/tool surface change. Public function paths change from `gx::git::foo` to `local::git::foo` or `remote::git::foo`; every importer is rewired.

### Implementation Plan

#### Phase 0: call-graph analysis + Track-A preflight
**Model:** opus
- **Track-A preflight (falsifiable gate):** assert `gx-mcp` is gone from workspace `members`, `src/mcp/` exists, and the MCP deps live in the `gx` lib (slated for `remote` in Phase 3). B0 does not start until this holds.
- **Rewrite the git.rs classification from the actual call graph** (do NOT confirm the table above). For each `git.rs` fn, determine whether it transitively runs a network verb (`fetch`/`pull`/`push`/`ls-remote`/`clone`) or calls `ssh`/`github`. Produce the authoritative local/remote table and the shape of the new `get_repo_status_local`.
- Zero-commit spike: extract the confirmed local set + define `get_repo_status_local`; confirm the remainder compiles and no local-bound fn reaches remote. Identify shared private helpers (push to `subprocess`/`utils` or duplicate).
- **Success criteria:** (1) an authoritative, call-graph-derived local/remote function table that supersedes the provisional list above; (2) `get_repo_status_local` defined, returning local branch/dirty/ahead-behind with a zero-fetch assertion; (3) the local set compiles credential-free; (4) the Track-A preflight assertions pass.

#### Phase 1: create local, move the credential-free modules
**Model:** sonnet
- New `local` crate; move `repo`, `config`, `subprocess`, `hash`, `utils`, `bare`, `diff`, `user_org`, and `test_utils` (behind `#[cfg(test)] pub mod`) into it; rewire their internal `crate::` paths; add `local` to the workspace; point the existing `gx` lib at `local::`. (`file` and the git split wait for Phase 2.)
- Single flat version across crates.
- **Success criteria:** (1) `cargo tree -p local` shows NO `ssh`/`persona`/`github` and NO `gx` dependency (no cycle); (2) `otto ci` green including `local`'s test target (which needs `test_utils`); (3) no behavior change (existing tests pass unmodified except import paths).

#### Phase 2: split git.rs into local + remote
**Model:** opus
- Move the Phase-0-confirmed local functions + `get_repo_status_local` + status types into `local::git`; keep the remote functions in `remote::git`; resolve straddling helpers per the Phase 0 findings.
- Move `file` into `local` (its `git` dep now resolves to `local::git`).
- Rewire every importer (`create`, `review`, `undo`, `checkout`, `clone`, `cleanup`, `status`, `state`, `doctor`, `transaction`, `rollback`) to `local::git` or `remote::git`.
- Add the **biting boundary guard**: a CI grep/lint over `local/src` that fails on `crate::{ssh,github,persona}`, `Command::new("gh")`, or a remote git verb (`fetch`/`pull`/`push`/`ls-remote`/`clone`).
- **Success criteria:** (1) `local::git` compiles with no `ssh`/`github` import; (2) the CI grep guard BITES: planting a `git fetch` in a `local` module turns CI red (proven); (3) `otto ci` green with all existing git tests passing; (4) `file` lives in `local` and the B1-reused fns are reachable from `local`.

#### Phase 3: form remote, finalize the bin
**Model:** sonnet
- Move the credential-bound + orchestration modules (`ssh`, `persona`, `github`, `create`, `review`, `checkout`, `clone`, `cleanup`, `undo`, `rollback`, `transaction`, `state`, `doctor`, `status`, `output`, `cli`, `mcp`) into `remote` (depends on `local`); reduce the `gx` bin to a thin shim over `remote`.
- **Success criteria:** (1) workspace is `local` + `remote` + `gx` bin, single flat version; (2) `otto ci` green; (3) the full `gx --help` command matrix behaves identically (`status`, `create`, `apply`, `review`, `checkout`, `clone`, `cleanup`, `undo`, `doctor`, `mcp`).

## Acceptance Criteria

- [x] `local` has NO `remote` dependency and no `ssh`/`persona`/`github` dependency (`cargo tree -p local`).
- [x] A CI grep/lint over `local/src` fails on `crate::{ssh,github,persona}`, `Command::new("gh")`, or a remote git verb (`fetch`/`pull`/`push`/`ls-remote`/`clone`); planting a `git fetch` in a local module turns CI red (proven, per "tests must bite").
- [x] `otto ci` is green at every phase; no existing test changes except import paths (behavior unchanged).
- [x] `git.rs` is split: local status/branch ops + the new `get_repo_status_local` live in `local::git`, remote/credential ops in `remote::git`.
- [x] Workspace carries a single flat version across `local`, `remote`, `gx` (bin).

## Resolved Decisions

- 2026-07-18 (Scott): enforce the intel cross-org boundary with a separate crate (option b), which requires this decomposition; chosen over the import-lint test and `IntelConfig`.
- 2026-07-17 (author): two crates (local/remote) + bin, not a crate per module; deviating would fragment the workspace for no gain.
- 2026-07-18 (author, from review): the git.rs seam is by transitive call graph, not name/import; the doc's first function table was mislabeled "verified" and is corrected. B1's walk uses a NEW `local::git::get_repo_status_local` (no fetch), not the fetch-capable `get_repo_status_with_options`. Phase 0 produces the authoritative table.
- 2026-07-18 (author, from review): the boundary guard is (a) `local` has no `remote`/credential dep AND (b) a biting CI grep over `local/src` for credential imports, `gh` shell-outs, and remote git verbs; `cargo tree` alone is insufficient (it misses source-level shell-outs).
- 2026-07-18 (author, from review): `file` lands in `local` (Phase 2, after the git split); `test_utils` lands in `local` (Phase 1, `#[cfg(test)]`).

## Alternatives Considered

### Alternative 1: import-lint CI test (option c)
- **Description:** keep one crate; a CI test fails if `src/mcp/intel*` imports credential modules.
- **Pros:** one file, touches nothing else.
- **Cons:** not compiler-structural; a lint, not a boundary.
- **Why not chosen:** Scott chose the true structural boundary (2026-07-18).

### Alternative 2: `IntelConfig` (option a)
- **Description:** hand intel tools a config type lacking the `github` block.
- **Cons:** compile-time for config access only; does not stop `Command::new("gh")`; still needs the lint.
- **Why not chosen:** partial; superseded by the crate boundary.

## Technical Considerations

### Dependencies
- No new external deps. Internal restructuring only.

### Performance
- None. Same code, different crate boundaries.

### Testing Strategy
- Existing test suite is the oracle: it must pass unmodified except import paths. That IS the "no behavior change" proof.
- Add one assertion (or a documented `cargo tree` check) that `local` has no credential-module dependency.

### Rollout Plan
- Lands after Track A. Prerequisite for Track B1. No external/user-visible change; internal crate refactor only.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `git.rs` functions misclassified (a "local" fn transitively fetches) | Med | High | Phase 0 REWRITES the table from the call graph before any commit; the biting CI grep catches a remote verb that slips into local |
| `git.rs` local/remote functions share private helpers, making the split messy | Med | Med | Phase 0 spike enumerates straddlers before any commit; push shared helpers to `subprocess`/`utils` |
| Broad import rewrite breaks a distant module | Med | Low | `otto ci` green gate per phase; the compiler finds every stale path |
| Version/tag scheme drifts to per-crate | Low | High | Single flat version asserted (AC); git.md forbids per-crate tags |

## Open Questions

- (none: all 6 review findings folded. The git.rs local/remote table is authoritatively produced by Phase 0's call-graph analysis, not by this doc; the provisional lists above are its starting anchors.)

## References

- Companion: `2026-07-17-gx-onto-mcp-io.md` (Track A, lands first) and `2026-07-17-gx-intel-catalog.md` (Track B1, depends on this)
- Review-panel finding (2026-07-17): the same-crate boundary is not structural
- Research brief (2026-07-17): git.rs coupling map, credential-module importers
