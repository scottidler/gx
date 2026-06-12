# Design Document: GX Workflow Safety Hardening

**Author:** Claude Code (from full-codebase review session, 2026-06-11)
**Reviewer:** Scott Idler
**Date:** 2026-06-11
**Status:** Implemented
**Review Passes Completed:** 5/5
**External review:** Architect (Gemini) + Staff Engineer (Codex), 2026-06-11

## Summary

A 2026-06-11 review of the gx create/review/cleanup/rollback workflow found that
several of the tool's advertised safety guarantees do not hold: the crash-recovery
subsystem is dead code, file matching reaches into `.git/` internals (verified
empirically), PR creation is hardcoded to `--base main` with failures silently
swallowed, and a successful run strands the user's uncommitted work in a stash.
This document rolls every finding from that review into an eight-phase remediation
plan that makes gx safe to run against a large fleet of repositories.

## Problem Statement

### Background

gx applies deterministic changes (add, delete, sub, regex) across many
repositories at once: discover repos, filter, stash dirty trees, branch, mutate
files with backups, commit, push, open PRs, then track the change through
review/approve/cleanup via a change-id. The architecture is sound, but the
implementation predates real fleet usage and a full review found 30+ defects
ranging from data corruption to silent partial failure.

The review verified the two most severe findings live against `gx v0.1.9`:

- `gx create --files '**/*'` matched `.git/config`, `.git/index`,
  `.git/objects/*`, hooks, and refs in a test repo. `sub`/`regex` would rewrite
  `.git/config`; `delete --commit` would destroy the object database.
- A repository named `reporting` was silently invisible to `gx status` (and
  therefore to every write operation) because of a pre-commit-cache name
  heuristic.

### Problem

gx's value proposition is *safely* making the same change across many repos.
Today the failure modes are concentrated exactly where safety matters most:

1. **Corruption surface:** the file matcher can target `.git/` internals and
   binary files, and writes are non-atomic.
2. **Recovery is fiction:** `gx rollback` reads recovery state that no code path
   ever writes; an interrupted run leaves branches, stashes, and backup litter
   with no record.
3. **Silent partial failure:** PR creation failures are downgraded to success,
   PR search caps at 100 results, repos can be silently excluded from discovery,
   and campaign statistics are computed on the wrong content.
4. **Data stranding:** a *successful* run leaves the user's uncommitted work in
   `git stash` and the repo on the GX branch, with no message.
5. **Blast radius:** discovery walks *up* the directory tree, no-pattern means
   all repos, and `--commit` asks no confirmation.

### Goals

- Every defect from the 2026-06-11 review is fixed (full inventory in Appendix A).
- An interrupted `gx create` run is recoverable by `gx rollback` using state that
  actually exists on disk.
- A successful run restores the user's working tree (original branch, stash
  popped) and reports accurate statistics.
- Failures are visible in the summary output, never only in the log file.
- Write operations never touch `.git/`, ignored files, untracked files, or
  binary files, and all file writes are atomic.
- The operator sees and confirms the blast radius before any commit-mode run.

### Non-Goals

- `Change::Agent(prompt)` (agent-per-repo changes). Explicitly deferred to a
  future design doc.
- New change types, new subcommands, or output/formatting redesign beyond what
  the fixes require.
- Replacing shell-outs to `git`/`gh` with libgit2/octocrab (see Alternatives).
- Windows support work beyond not regressing current behavior.

## Proposed Solution

### Overview

Eight phases, ordered by risk reduction, releasable in sequence (later phases
build on earlier ones - e.g. Phase 3 reuses Phase 1's atomic-write helper, and
Phases 3-4 share the XDG data layout), each verified by `otto ci` plus
phase-specific regression tests:

1. **File-matching safety** - candidate files come from git's index, never a
   filesystem glob; atomic writes; binary skip; correct match counting.
2. **Discovery and blast radius** - remove silent exclusions, remove the
   upward walk, add a confirmation gate.
3. **Transaction engine rework** - replace closures with a typed, serializable
   step enum; persist recovery state; out-of-tree backups; correct success-path
   finalization and operation ordering.
4. **State tracking** - XDG paths, incremental persistence, deterministic
   serialization, race-free updates, cleanup that uses recorded paths.
5. **GitHub layer** - dynamic base branch, surfaced PR failures, pagination,
   consistent token auth, change-id validation, guarded purge.
6. **Git layer hygiene** - `--ff-only` pulls, unified status parsing, literal
   pathspecs, detached-HEAD guard.
7. **CLI / config / logging** - `--log-level`, `gx doctor`, no CWD config
   fallback, honest help text.
8. **Test hardening** - regression tests for every fix; remove vacuous tests.

### Architecture

The change-making pipeline keeps its current shape. The phases alter three
load-bearing components:

```
discover_repos ──filter──► process_single_repo (per repo, rayon)
                              │ dirty (incl. untracked)? stash -u → switch to head → pull --ff-only
                              │ candidate files = git ls-files --cached (Phase 1)
                              │ mutate via FileSet + Transaction (Phases 1,3)
                              │ branch → stage → commit → push
                              │ finalize transaction (Phase 3)  ◄── before PR
                              │ create PR (Phase 5)
                              └─► RepoResult ──► StateManager (Phase 4, incremental)
```

**Component 1 - `FileSet` (new, Phase 1, lives in `src/file.rs`).** Replaces
`find_files_in_repo`'s filesystem glob. Candidates come from
`git ls-files --cached -z` - **tracked files only** (see Decisions, Q6) - so
`.git/` contents, gitignored files, untracked files, and submodule internals
are structurally unreachable. Untracked files are the user's WIP: preserved by
`stash -u`, never matched, never committed; `gx add` is the sole path that
creates new files. Combined with the stash, this yields a load-bearing
invariant: **gx always mutates a pristine checkout of HEAD**. The user's glob
patterns match against the returned *relative paths* with `glob::Pattern`, so
glob metacharacters in the repo's absolute path no longer corrupt matching.

**Component 2 - `RollbackStep` (new, Phase 3).** The `Transaction`'s
`Box<dyn Fn()>` closures and description-string dispatch are replaced by one
data enum that is executed by a single interpreter and serialized verbatim as
recovery state:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RollbackStep {
    PopStash      { repo: PathBuf, stash_sha: String },
    SwitchBranch  { repo: PathBuf, branch: String },
    DeleteLocalBranch  { repo: PathBuf, branch: String, branch_existed: bool },
    DeleteRemoteBranch { repo: PathBuf, branch: String },
    ResetCommit   { repo: PathBuf, expected_sha: String },
    RestoreBackup { backup: PathBuf, original: PathBuf },
    RemoveCreatedFile { path: PathBuf },
}
```

Each step carries enough state to be reversed correctly without re-deriving it
later: `ResetCommit` records the pre-commit HEAD (`expected_sha`) so recovery
resets to a known target rather than blindly `HEAD~1`, and `DeleteLocalBranch`
records `branch_existed` so rollback skips deletion of a branch gx did not
create (the current code captures `branch_existed` at `create.rs:954`; the
prior enum draft dropped it).

Because steps are data, persisting recovery state is `serde_json::to_string`
of the live step list - the recovery file and the in-memory transaction can no
longer diverge, and `gx rollback` interprets the same enum.

**Component 3 - `gh` invocation helper (Phase 5).** All GitHub calls go through
one function that resolves the org's token file and sets `GH_TOKEN`, ending the
current split where repo listing uses token files but PR create/merge/close use
ambient `gh auth` (a real hazard on multi-account machines).

### Data Model

**Recovery state** (`$XDG_DATA_HOME/gx/recovery/<tx-id>.json`, new - currently
never written):

```json
{
  "transaction_id": "gx-tx-1760000000-7",
  "change_id": "GX-2026-06-11T12-30-00",
  "repo_path": "/home/u/repos/org/repo",
  "created_at": "2026-06-11T19:30:00Z",
  "steps": [ { "SwitchBranch": { "repo": "...", "branch": "main" } } ]
}
```

Written **write-ahead** - the step that undoes an operation is persisted before
that operation runs (see Recovery Invariant) - and deleted on successful
finalize or completed rollback. `steps` is stored in registration order;
rollback and recovery both execute it in reverse.

**Change state** (`$XDG_DATA_HOME/gx/changes/<change-id>.json`, moved from
`~/.gx/changes/`):

- `repositories` becomes `BTreeMap<String, RepoChangeState>` (deterministic
  serialization; `HashMap` today).
- `RepoChangeState.original_branch` is actually populated (field exists today,
  always `None`).
- `pr_number` stays `Option<u64>` but `0` is never stored (today a URL-parse
  failure stores `0`).
- Saved incrementally after each repo completes, not once at the end.
- All structs gain `#[serde(deny_unknown_fields)]`.

**Backups** move out of the worktree to
`$XDG_DATA_HOME/gx/backups/<tx-id>/<relative-path>` (today: `<file>.<ext>.backup`
beside the original). This eliminates: backup files matching later glob
patterns, collision with user files named `*.backup`, the
`run_backup_preflight_check` worktree scan, and the string-matched
"Cleanup backup file:" pseudo-actions. On finalize, the tx backup dir is
deleted; on rollback, files are copied back.

### Recovery Invariant (crash consistency)

The recovery log and git's object graph cannot be made atomic with respect to
each other, so gx orders operations **write-ahead**: the rollback step that
*undoes* an operation is persisted (atomic JSON write) **before** the operation
is performed, and every step is idempotent. Concretely,
`ResetCommit { expected_sha }` records the pre-commit HEAD and is written before
`git commit` runs; `PopStash { stash_sha }` is written immediately after the
stash is created; `DeleteLocalBranch`/`DeleteRemoteBranch` are written before
the branch is created.

The recovery log is therefore always a **superset** of what happened on disk: a
SIGKILL between an operation and its (already-persisted) step cannot strand an
unrecorded mutation. On recovery, idempotent steps tolerate the operation having
happened or not - resetting to `expected_sha` is a no-op if the commit never
landed, and deleting an absent branch succeeds.

This fixes the partial-failure state directly. After commit+push succeed but
local restore, PR creation, or state save fail, the repo is reported
`Committed` with the specific error, the recovery file is **retained** until the
user resolves it, and the working tree is left on the original branch. The
operator can reconstruct the exact state from the result row plus the recovery
JSON without re-reading source.

### API Design

User-visible behavior changes (all are safety gates or bug fixes, not features):

| Surface | Today | After |
|---|---|---|
| `gx create ... --commit` | runs immediately | prints resolved repo list/count; prompts unless `--yes` - always when no `-p` patterns are given (any count), otherwise when repo count exceeds confirm-threshold |
| `gx create --files '**/*'` | matches `.git/**`, gitignored files, untracked files | matches tracked files only |
| success on dirty repo | work left in stash (untracked files not stashed), repo on GX branch | `stash -u` applied+dropped, original branch restored |
| `gx create -x my-id` | accepted; PRs unfindable by `gx review` | rejected unless it starts with `GX-` (with hint) |
| `gx review purge` | deletes all `GX-*` branches instantly | targets only gx-created branches with no open PR; lists targets, prompts unless `--yes`; refuses branches with open PRs (run `gx review delete` first) |
| `gx review approve/delete` | silently capped at 100 PRs | paginates; exact counts |
| PR creation failure | logged only; summary says success | shown in result row and summary error count |
| `--log-level <level>` | absent (RUST_LOG only) | standard flag; RUST_LOG no longer consulted |
| `gx doctor` | absent (checks run on every `--help`) | new subcommand for git/gh version checks |
| `./gx.yml` CWD fallback | silently loaded | removed; explicit `-c` or XDG path only |

New config keys (kebab-case, mirroring flags):

```yaml
create:
  confirm-threshold: 5     # prompt when committing to more repos than this
github:
  pr-body-template: "{commit_message}"   # replaces hardcoded scottidler/gx link
```

### Implementation Plan

Phases are ordered by risk reduction. Each phase ends with `otto ci` green and
its regression tests passing. Items carry their review-finding numbers from
Appendix A.

#### Phase 1: File-matching and write safety
**Model:** opus

The corruption surface. Replaces the glob engine and the write path.

- [ ] New `FileSet::candidates(repo_path) -> Result<Vec<PathBuf>>` shelling to
      `git ls-files --cached -z` (tracked files only - untracked files are
      never candidates; see Decisions, Q6); parse NUL-delimited bytes (not
      UTF-8 lines) into relative paths. Filter to regular files: skip
      submodule gitlinks (mode 160000 entries) and symlinks - a symlink would
      let a substitution write through to a target outside the worktree, and
      delete/restore semantics differ. [A1, Q6]
- [ ] Match user patterns against relative paths with `glob::Pattern`
      (compiled once per pattern); delete the `repo_path.join(pattern)`
      filesystem-glob join. Hard-assert no candidate path has a `.git`
      component (defense in depth). [A1, A26]
- [ ] `sub`/`regex`/`delete`: on non-UTF-8 content, skip the file with a
      `warn!` and a new `files_skipped_binary` stat instead of aborting the
      whole repo. [A21]
- [ ] Atomic writes: one shared helper - write to a uniquely named temp file in
      the target's directory, fsync, rename over the target. Used by
      substitution writes, `create_file_with_content`, and state/recovery JSON
      writes. [A21]
- [ ] Validate the `add` write path, which bypasses `FileSet` today
      (`gx add` does `repo_path.join(file_path)` directly -
      `src/create.rs:653` → `src/file.rs:66`). Before any write: reject
      absolute paths, `..` components, and any `.git` path component, and
      reject when a parent is a symlink that would escape the worktree; the
      created file must canonicalize to inside `repo_path`. Same policy as
      `FileSet` candidates, applied to the one path that does not flow through
      it. [A32]
- [ ] Fix match counting: `SubstitutionResult::Changed` gains `match_count`
      computed from the *original* content inside `apply_substitution` /
      `apply_regex_substitution`; delete the post-write re-read in
      `apply_substitution_change` / `apply_regex_change`
      (`src/create.rs:816`, `:911`). [A8]
- [ ] Regression tests: `**/*` never matches `.git/**`, gitignored files, or
      untracked files; dotfile patterns like `.github/workflows/*.yml` still
      match (tracked dotfiles are candidates); binary file in the candidate
      set is skipped, not fatal; match counts correct for multi-match files.

#### Phase 2: Discovery correctness and blast radius
**Model:** sonnet

- [ ] Delete the `name.starts_with("repo")` heuristic in
      `is_ignored_directory` (`src/repo.rs:345`). Pre-commit caches are already
      excluded by the `/.cache/` path check. [A6]
- [ ] Wire `repo-discovery.ignore-patterns` config into
      `is_ignored_directory` (currently parsed and never used); hardcoded names
      become the documented defaults. [A27]
- [ ] Remove `find_workspace_root` case 3 (walking up from a repo-less CWD
      until repos are found, potentially reaching `$HOME`). Keep case 1
      (inside a repo → search from parent); note that case 1 intentionally
      includes sibling repos, which is why the confirmation gate below always
      shows the resolved list. A repo-less CWD now reports "no repositories
      found under <dir>" instead of widening scope. [A9]
- [ ] Confirmation gate: when `--commit` is present, print the resolved repo
      slugs and count before mutating; prompt for confirmation when the count
      exceeds `create.confirm-threshold` (default 5) or no `-p` patterns were
      given. `--yes` skips the prompt for automation. Fail closed: a prompt on
      non-TTY stdin without `--yes` aborts with an error naming the flag - it
      never silently proceeds. [A9]
- [ ] Regression tests: `reporting`/`repository`-named repos are discovered;
      config ignore-patterns respected; workspace root never above the starting
      directory unless CWD is itself inside a repo.

#### Phase 3: Transaction engine rework
**Model:** opus

The heart of the fix. Closures become data; recovery becomes real.

- [ ] Introduce `RollbackStep` (see Data Model) and a single
      `execute_step(&RollbackStep) -> Result<()>` interpreter. Delete
      `Box<dyn Fn>` actions, description-string dispatch
      (`contains("Cleanup backup file:")`, `contains("Switch back to")`), and
      `SerializableRollbackAction`. Steps must be idempotent (e.g.
      delete-remote-branch on an already-deleted branch succeeds, as today) so
      a re-run of an interrupted recovery converges. [A17]
- [ ] Rename `Transaction::commit()` to `Transaction::finalize()` throughout.
      "Commit" currently means two unrelated things in `process_single_repo`
      (git commit vs. transaction success); the rename removes the ambiguity
      this document otherwise inherits.
- [ ] Persist recovery state **write-ahead** (see Recovery Invariant): write
      the JSON (atomic helper from Phase 1) inside `push_step`, *before* the
      operation that step reverts is performed, including `change_id` and
      `repo_path`; delete it on `finalize()` or completed `rollback()`.
      `gx rollback list/execute/validate` now operate on the same enum. Set
      `recovery_enabled` semantics by construction (the flag and its dead
      branches are removed). [A2]
- [ ] Out-of-tree backups under `$XDG_DATA_HOME/gx/backups/<tx-id>/...`;
      delete `backup_file`'s `.backup`-beside-original scheme,
      `find_backup_files_recursive`, and `run_backup_preflight_check`. [A17, A21]
- [ ] Stash with `-u` (`git stash push -u`), and the dirty predicate that
      triggers it must count untracked (`??`) entries as dirty - both halves
      of the Q6 decision. After the stash, the worktree is exactly HEAD:
      untracked files cannot be matched, modified, or committed because they
      are not present during the mutation window. Ignored files are neither
      stashed (`-a` is not used) nor candidates (never listed by
      `ls-files --cached`), so they are untouched in place. [Q6]
- [ ] Stash by SHA: `stash_save` returns the stash commit SHA
      (`git rev-parse stash@{0}` immediately after push). `PopStash` runs
      `git stash apply <sha>` (apply accepts any stash-shaped commit; `pop`
      does not take raw SHAs). Dropping is the subtle part: `git stash drop`
      takes no SHA, only a positional `stash@{n}`, so resolve `n` from
      `git reflog show stash` by matching `stash_sha` **immediately before** the
      drop and re-verify the SHA at that index, so a concurrent stash mutation
      cannot shift the entry and cause the wrong stash to be dropped (the
      per-repo lock below also guards this). If the SHA is gone, fail with an
      error naming it - never operate on positional `stash@{0}` at restore
      time. [A15]
- [ ] Success-path finalization: `Transaction::finalize()` runs explicit
      completion steps - switch back to the original branch and apply+drop the
      stash - then clears rollback steps and deletes the recovery file. A
      successful run leaves the working tree as the user had it. Output notes
      "restored stash" when one existed. [A5]
- [ ] Stash-apply conflict on finalize: if `git stash apply <sha>` conflicts
      (the user's stashed work collides with upstream changes pulled into the
      original/head branch - gx's own commits live on the GX branch and never
      touch the original, so this is the only realistic collision; with `-u`
      this includes git refusing to restore a stashed untracked file because
      the pull introduced a tracked file at the same path), gx does
      **not** attempt a merge and does **not** drop the stash. The repo result
      becomes `Committed` with a `stash-restore-failed` error so it appears in
      the result row and the summary error count, not only the log; the message
      names the stash SHA and the manual recovery command. The worktree is left
      on the original branch with the apply result in place so the collision is
      visible. [Q2]
- [ ] Per-repo lock: before `process_single_repo` mutates a repo, acquire an
      exclusive lock at `$XDG_DATA_HOME/gx/locks/<hash-of-canonical-repo-path>.lock`
      (atomic `O_EXCL` create) carrying pid / cwd / command / started-at. If the
      lock is held, fail that repo fast with a message naming the holder;
      release on finalize, rollback, or error. A stale lock (holder pid gone) is
      reclaimed with a `warn!`. This closes the concurrent-invocation
      stash/branch interleaving vector rather than documenting it as
      unsupported. [Q5]
- [ ] Reorder `process_single_repo`: branch → stage → commit → push →
      **finalize transaction** → create PR. This is sound because
      `gh pr create --repo <slug> --head <branch>` operates purely against the
      remote - the local checkout has already been restored to the original
      branch by finalize. A PR failure after a finalized push is reported as
      `Committed` *with the error populated* (Phase 5 surfaces it); rollback
      can no longer orphan a PR. [A7]
- [ ] Guard: if `get_current_branch_name` returns empty (detached HEAD), fail
      the repo up front with a clear error instead of later attempting
      `git checkout ""`. [A30]
- [ ] Regression tests: kill -9 simulation (registered steps → recovery file
      exists → `gx rollback execute` restores branch/stash/files); success path
      pops stash and restores branch, including a pre-existing untracked file
      surviving the full cycle byte-for-byte and never appearing in the gx
      commit; backups never appear in the worktree;
      stash-apply conflict leaves the stash intact and marks the result
      `stash-restore-failed`; a second invocation against a locked repo fails
      fast and a stale lock is reclaimed.

#### Phase 4: State tracking integrity
**Model:** sonnet

- [ ] Move state dirs from `~/.gx/{changes,recovery}` to
      `$XDG_DATA_HOME/gx/{changes,recovery}` using the existing
      `xdg_data_dir()` helper. One-time migration: if the old dir exists and
      the new one does not, move it and log. [A22]
- [ ] Incremental saves: `StateManager::save` is called inside the existing
      mutex after each repo's result is folded in (`src/create.rs:222`), not
      once at the end. [A3]
- [ ] `repositories: BTreeMap`; `#[serde(deny_unknown_fields)]` on all state
      structs; `state.list()` logs a `warn!` for unparsable state files instead
      of silently skipping. [A22]
- [ ] Record `original_branch` in `RepoChangeState`; thread it from
      `process_single_repo`. [A19]
- [ ] Fix the read-modify-write race in `review approve/delete`: parallel
      workers return results only; after `pool.install` completes, a single
      load → apply-all → save updates the change state once. [A10]
- [ ] `extract_pr_number_from_url` failure propagates as an error on the
      result instead of storing PR `#0`. [A19]
- [ ] Cleanup resolves repos via the recorded `local_path` first, falling back
      to the current CWD search only when absent; a missing recorded path is
      reported, not silently skipped. [A16]
- [ ] Regression tests: state file exists after first repo completes;
      concurrent approve updates all land; cleanup works from an unrelated CWD.

#### Phase 5: GitHub layer correctness
**Model:** opus

- [ ] Base branch: `create_pr` resolves the repo's default branch (local
      `get_head_branch`, falling back to `gh api repos/{slug} --jq .default_branch`)
      instead of hardcoded `--base main` (`src/github.rs:170`). If both the
      local lookup and the API call fail (ACL/403/404/offline), fall back to
      `main` then `master` with a `warn!` naming the repo, rather than aborting
      the PR pipeline - a default-branch lookup failure must not drop the PR. [A4]
- [ ] Surface PR failure: `create.rs:585-594` stops discarding the error -
      action stays `Committed`, `result.error` is set, the summary error count
      includes it, and the result row shows it. Same policy for approve: if
      `gh pr review --approve` fails, the failure is recorded on the result
      (merge still proceeds, since self-approval rejection is expected). [A4]
- [ ] Pagination: `list_prs_by_change_id` follows GraphQL `pageInfo
      { hasNextPage, endCursor }` until exhausted. The search string (which
      embeds org and pattern) is passed as a single GraphQL variable
      (`query($q: String!, $cursor: String)` with `-f q=...`), so org/pattern
      values are JSON-encoded rather than spliced into the query text. [A13]
- [ ] Change-id validation: `--change-id` values not starting with `GX-` are
      rejected at parse time with a message explaining the review-tooling
      contract. [A11]
- [ ] Purge guard: `gx review purge` targets only gx-created branches
      (recorded in change state, or matching the `GX-` prefix), lists every
      branch it will delete per repo, and **refuses any branch that still has an
      open PR** - the user must `gx review delete` first (which closes/handles
      the PR), preventing purge from silently closing open PRs by deleting their
      head branches. Branch and open-PR listing paginate past 100
      (`src/github.rs:523` is a single un-paginated call today). Prompts unless
      `--yes`. [A12, Q3]
- [ ] One `gh_command(org)` helper resolves the token file via the existing
      `read_token` and sets `GH_TOKEN` for *every* gh invocation (PR
      create/merge/close/review, branch delete, branch list, GraphQL). [A18]
- [ ] PR body from `github.pr-body-template` config (default: the commit
      message); delete the hardcoded `scottidler/gx` README link. [A29]
- [ ] Regression tests: PR against a `master`-default repo (mock gh via PATH
      shim); >100 PR pagination; non-`GX-` change-id rejected; purge prompt.

#### Phase 6: Git layer hygiene
**Model:** sonnet

- [ ] `pull_latest_changes` uses `git pull --ff-only` (matching the checkout
      path); a non-ff result is a per-repo error, not a surprise merge commit.
      Delete the dead `Already up to date` stderr sniff (that message goes to
      stdout on a zero exit). [A14, A28]
- [ ] Unify the two `git status --porcelain` parsers (`get_status_changes` vs
      `get_status_changes_for_path`) into one function with one counting rule,
      taking the output text as input so it is unit-testable. [A20]
- [ ] Stage files with literal pathspecs: `git add -A -- :(literal)<path>` so
      a tracked filename containing glob metacharacters cannot re-expand. [A26]
- [ ] Read git output as bytes where filenames appear (`-z` flags +
      NUL-splitting) instead of failing on non-UTF-8 filenames. [A21]
- [ ] Regression tests: porcelain parser table-driven cases; ff-only divergence
      error; literal-pathspec staging of a file named `f[1].txt`.

#### Phase 7: CLI, config, and logging
**Model:** sonnet

- [ ] `--log-level/-l` flag (off|error|warn|info|debug|trace, case-insensitive,
      default info) wired into `env_logger::Builder::filter_level`; stop reading
      `RUST_LOG`. [A24]
- [ ] New `gx doctor` subcommand for git/gh presence + version checks;
      `after_help` becomes static text plus a log path rendered at runtime from
      the same `xdg_data_dir()` source the logger uses - no subprocess spawns
      during `Cli::parse()`. [A24]
- [ ] `gx doctor` also reports orphaned recovery/backup artifacts under
      `$XDG_DATA_HOME/gx` - transactions whose `repo_path` no longer exists, or
      older than a TTL - and removes them with `--purge` (via `rkvr`, not
      `rm`). Without this, a SIGKILLed run or a manually-deleted repo leaks
      recovery state forever. [A2]
- [ ] Remove the `./gx.yml` CWD fallback in `Config::load`; config comes from
      `-c <path>` or the XDG location only, and the loaded path is logged. [A23]
- [ ] Fix `version_compare`: pad the shorter version with zeros so
      `"2.20"` vs `"2.20.0"` compares equal. [A25]
- [ ] Regression tests: log-level flag controls output; config not picked up
      from CWD; version compare table.

#### Phase 8: Test hardening
**Model:** sonnet

- [ ] Delete Debug-formatting trivia tests (`test_change_debug`,
      `test_create_action_debug`, `test_review_action_debug`, etc.). [A31]
- [ ] Git-backed tests stop silently `return`ing on setup failure; the helper
      panics with context (git is a declared requirement; CI has it). [A31]
- [ ] Move inline `#[cfg(test)] mod tests` blocks into sibling `tests.rs`
      files per the repo's Rust conventions, as files are touched. [A31]
- [ ] Confirm a regression test exists for every Appendix A item; add any
      missing ones. End-to-end: temp "org" of 3 repos (one `master`-default,
      one dirty, one named `reporting`) through create→state→cleanup. [A31]

## Alternatives Considered

### Alternative 1: Replace git/gh shell-outs with libgit2 (git2) + octocrab
- **Description:** Use library bindings instead of subprocess calls.
- **Pros:** Typed errors end the string-sniffing problem wholesale; no PATH/auth ambiguity.
- **Cons:** Large rewrite of `git.rs`/`github.rs`; libgit2 diverges from git CLI behavior (stash, ff-only semantics); loses `gh`'s auth/enterprise handling; high regression risk while the current defects remain live.
- **Why not chosen:** The defects are fixable within the subprocess design with far less risk. Typed `RollbackStep` removes the worst string-dispatch internally.

### Alternative 2: Adopt turbolift + slam instead of fixing gx
- **Description:** Retire gx's create/review path in favor of existing fleet-PR tooling.
- **Pros:** Battle-tested campaign management; less code to own.
- **Cons:** Loses gx's funnel UX, change-id lifecycle, state tracking, and single-binary ergonomics; turbolift has no equivalent of gx's dry-run pattern analysis; migration cost for existing users.
- **Why not chosen:** gx's design is worth keeping; the problems are implementation defects, not architectural dead ends.

### Alternative 3: Filesystem walk with an ignore crate instead of `git ls-files`
- **Description:** Use the `ignore` crate (ripgrep's walker) for candidate files.
- **Pros:** Pure-Rust, respects `.gitignore`, skips hidden dirs.
- **Cons:** Reimplements what git already knows; subtle divergences (global excludes, `.git/info/exclude`, submodule boundaries); another dependency.
- **Why not chosen:** gx already requires git ≥ 2.30 and shells out everywhere; `git ls-files` is the authoritative answer to "which files does this repo contain."

### Alternative 4: Keep closure-based Transaction, persist a parallel description log
- **Description:** Minimal change - keep `Box<dyn Fn>` actions and write the existing `SerializableRollbackAction` records alongside.
- **Pros:** Smaller diff.
- **Cons:** Two representations of the same actions that can drift (this is exactly today's bug: the serializable form exists and is never written, and recovery re-parses description strings); string dispatch remains.
- **Why not chosen:** The single-enum design makes the recovery file correct by construction.

## Technical Considerations

### Dependencies
- No new crates required. `glob` stays (for `Pattern` matching against relative paths); `walkdir` stays (discovery); candidate listing uses the already-required git binary.
- Any new dependency, if one emerges during implementation, is added via `cargo add`.

### Performance
- `git ls-files` per repo replaces a filesystem glob walk: comparable or faster on large repos (git reads its index; no directory traversal), one extra subprocess per repo per pattern set.
- Incremental state saves add one small JSON write per repo; recovery-state writes add one per transaction step (~6 per repo). Both use the atomic-write helper; negligible against network operations (push, PR creation).
- Removing the eager `git --version`/`gh --version` spawns from `Cli::parse()` makes every invocation faster.

### Security
- GraphQL variables instead of string-interpolated queries close an injection path via crafted change-ids or org names. [A13]
- Consistent `GH_TOKEN` resolution prevents cross-identity operations on multi-account machines (work vs personal). [A18]
- Removing the CWD config fallback prevents an untrusted directory from reconfiguring the tool (e.g. redirecting `token-path`). [A23]
- Hard `.git` exclusion in `FileSet` prevents both accidental and crafted-pattern repository corruption. [A1]

### Testing Strategy
- Unit: substitution/match-count, porcelain parsing, version compare, pattern matching against relative paths, `RollbackStep` round-trip (serialize → execute).
- Integration (tempdir git repos, current test style but fail-loud): full create lifecycle, kill-and-recover, dirty-repo stash restore (tracked and untracked dirt), master-default-branch PR (gh stubbed via a PATH shim script), cleanup from foreign CWD.
- Every Appendix A item maps to at least one test; Phase 8 audits the mapping.
- `otto ci` green at the end of every phase.

### Rollout Plan
- One phase per PR (or small PR series), merged in order; each phase is independently releasable.
- Behavior changes that could surprise existing users (confirmation gate, hidden-file matching via git candidates, `GX-` change-id enforcement, CWD config removal) land with clear changelog entries; `--yes` preserves scriptability.
- State migration (Phase 4) is automatic and one-way with the old directory left renamed (`~/.gx.migrated-<date>`) rather than deleted.
- Version bumps via `bump` per the repo's release flow; no version numbers are predicted in this document.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Tracked-only candidate set narrows what existing patterns match (gitignored, untracked, hidden-but-untracked files) | Medium | Medium | Changelog + `files_skipped` stats in pattern analysis make exclusions visible; dry-run remains the default; `gx add` covers intentional file creation |
| Transaction rework introduces a regression worse than the bugs it fixes | Medium | High | Phase 3 is opus-modeled, has kill-and-recover integration tests, and ships alone in its own PR |
| Confirmation gate breaks existing automation | Medium | Low | Gate only triggers in commit mode; `--yes` and non-TTY detection preserve scripts |
| State-dir migration loses in-flight change records | Low | Medium | Move (not copy-delete), keep old dir renamed, log both paths |
| gh stub tests diverge from real gh behavior | Medium | Medium | Stubs assert on exact argv; a thin live smoke test (opt-in, env-gated) exercises real gh |
| `--ff-only` pull starts failing repos that previously "worked" via silent merges | Low | Low | Error message names the repo and instructs `git pull` manually; this is the correct behavior |

## Decisions (resolved 2026-06-11)

Q1-Q5 were resolved in design review with the Architect (Gemini) and Staff
Engineer (Codex) with three-way consensus. Q6 (the untracked-file model the
reviewers surfaced) and the stash-conflict branch question were closed
afterward by the author with Scott's sign-off, completing the review. Each
decision is folded into the phase it affects; no open questions remain.

- **Confirmation gate.** A no-pattern commit run always prompts, regardless of
  repo count (least expressed intent, maximum blast radius); the count
  threshold (default 5) applies only to patterned runs. The draft API table
  contradicted Phase 2 here - the table is now corrected to match. Phase 2. [Q1]
- **Stash-apply conflict on finalize.** Never auto-merge. Preserve the stash
  (do not drop), leave the worktree on the original branch with the apply
  result visible, and surface the failure in the repo result row and summary
  error count - not only the log. Phase 3. [Q2]
- **`gx review purge`.** Refuses branches with open PRs (run `gx review delete`
  first), targets only gx-created branches, and paginates. Phase 5. [Q3]
- **`~/.gx` migration.** Migrate once and drop the shim immediately; the old
  directory is renamed (not deleted) as a one-time backup. Phase 4 / Rollout.
  [Q4]
- **Concurrent invocations.** A per-repo lockfile under
  `$XDG_DATA_HOME/gx/locks` is added in Phase 3, not deferred. [Q5]
- **Untracked-file model.** Candidates come from tracked files only
  (`git ls-files --cached`); the stash uses `-u` and the dirty predicate
  counts untracked files; `gx add` is the sole path that creates new files.
  Untracked files are the user's WIP: preserved in full, never matched, never
  committed. Two reasons, in order of weight. First, committing an untracked
  file is a **data-leak hazard**, not merely a surprise: untracked files are
  exactly where not-yet-gitignored secrets and `.env` drafts live, and a fleet
  tool that sweeps one into a commit pushes it to a GitHub PR - exfiltration,
  a strictly worse failure mode than local corruption. Tracked-only candidates
  make it structurally impossible rather than policy-discouraged. Second, the
  pair yields the invariant that gx always mutates a pristine checkout of
  HEAD, which makes match counts, diffs, and finalize behavior deterministic.
  The cost - a pattern no longer reaches a file nobody has committed - is the
  correct default for a bulk-change tool; `gx add` covers intentional file
  creation. The draft's internal contradiction (candidates included
  `--others` while the stash omitted `-u`) is resolved by this decision.
  Phases 1 and 3. [Q6]
- **Stash-conflict resulting branch.** On a stash-apply conflict at finalize,
  the user is left on the **original branch** (not the GX branch, as the
  Architect proposed). The stash is applied *after* finalize switches back,
  and its only realistic conflict is with upstream changes pulled into the
  original/head branch - gx's commits live on the GX branch and never touch
  the original, so the GX branch adds nothing to conflict resolution and
  leaving the user there would misattribute the collision to gx's change.
  Divergence from the Architect's review noted for the record. Phase 3. [Q2]

## References

- 2026-06-11 full-codebase review (chat session) - source of findings A1-A31.
- 2026-06-11 design review (Architect/Gemini + Staff Engineer/Codex) - source
  of the Q1-Q5 resolutions, the Q6 question, finding A32, and the Recovery
  Invariant subsection.
- `docs/gx-rollback-enhancement-implementation.md` - prior rollback design this
  plan supersedes where they conflict.
- `docs/slam-create-parity-implementation.md` - origin of the create funnel.
- Rust conventions: `~/repos/.claude/rules/rust.md` (atomic writes, typed seams,
  XDG helpers, test placement).

## Appendix A: Finding Inventory (traceability)

Every finding from the 2026-06-11 review, with its phase. Severity: C=critical,
H=high, M=medium. A32 was added during the 2026-06-11 design review.

| # | Sev | Finding | Location | Phase |
|---|-----|---------|----------|-------|
| A1 | C | Glob matches `.git/**` and hidden files; no gitignore awareness (verified) | `src/file.rs:9` | 1 |
| A2 | C | Recovery subsystem dead: `TransactionState` never persisted, `recovery_enabled` never true | `src/transaction.rs:71` | 3 |
| A3 | C | ChangeState saved only after all repos; crash loses all state | `src/create.rs:234` | 4 |
| A4 | C | PR hardcodes `--base main`; failure downgraded to success with `error: None` | `src/github.rs:170`, `src/create.rs:591` | 5 |
| A5 | C | Success strands user's stash and leaves repo on GX branch | `src/create.rs:329`, `transaction.rs:183` | 3 |
| A6 | H | Repos named `repo*` (≥8 alnum chars) silently excluded - `reporting` invisible (verified) | `src/repo.rs:345` | 2 |
| A7 | H | PR created before transaction finalized; preflight failure orphans the PR | `src/create.rs:584-601` | 3 |
| A8 | H | `total_matches` counted on post-substitution content; stats wrong | `src/create.rs:816,911` | 1 |
| A9 | H | Blast radius: upward workspace walk + no-pattern=all + no confirmation | `src/repo.rs:134` | 2 |
| A10 | H | State load-modify-save race across rayon threads in review approve/delete | `src/review.rs:617` | 4 |
| A11 | H | Non-`GX-` `--change-id` accepted but unfindable by review tooling | `src/github.rs:390` | 5 |
| A12 | H | `gx review purge` deletes all `GX-*` branches with no confirmation | `src/review.rs:715` | 5 |
| A13 | H | PR search caps at `first: 100`, no pagination; query string-interpolated | `src/github.rs:329` | 5 |
| A14 | M | `pull_latest_changes` bare `git pull` (merge); checkout path uses `--ff-only` | `src/git.rs:1481` | 6 |
| A15 | M | Stash rollback pops positional `stash@{0}` instead of a SHA | `src/git.rs:1330` | 3 |
| A16 | M | Cleanup ignores recorded `local_path`, re-guesses from CWD | `src/cleanup.rs:215` | 4 |
| A17 | M | String-typed dispatch: `contains("Cleanup backup file:")`, description-parsed recovery | `src/transaction.rs:137,224,268,356-407` | 3 |
| A18 | M | Auth split-brain: token files for listing, ambient `gh auth` for PR ops | `src/github.rs` | 5 |
| A19 | M | PR `#0` stored on URL-parse failure; `original_branch` never populated | `src/github.rs:185`, `src/state.rs:127` | 4 |
| A20 | M | Two divergent porcelain parsers with different counting rules | `src/git.rs:197,927` | 6 |
| A21 | M | Non-atomic `fs::write`; non-UTF-8 file aborts whole repo; backups inside worktree | `src/file.rs:66,114` | 1, 3, 6 |
| A22 | M | State in hidden `~/.gx/` instead of XDG; `HashMap` serialization nondeterminism; corrupt state silently skipped | `src/state.rs:347`, `src/transaction.rs:462` | 4 |
| A23 | M | Config silently falls back to `./gx.yml` in CWD (gx repo ships one) | `src/config.rs:177` | 7 |
| A24 | M | RUST_LOG-only logging, no `--log-level`; `--help` spawns git/gh; hardcoded log path string | `src/main.rs:62`, `src/cli.rs:17,460` | 7 |
| A25 | M | `version_compare("2.20","2.20.0")` returns false | `src/cli.rs:542` | 7 |
| A26 | M | Glob built by joining repo path + pattern (metachars in path break matching); staging pathspecs unescaped | `src/file.rs:10`, `src/git.rs:1080` | 1, 6 |
| A27 | M | `repo-discovery.ignore-patterns` config parsed but never used | `src/config.rs:71`, `src/repo.rs:319` | 2 |
| A28 | M | Dead `Already up to date` stderr check (message goes to stdout on success) | `src/git.rs:1497` | 6 |
| A29 | M | PR body hardcodes scottidler/gx README link | `src/github.rs:156` | 5 |
| A30 | M | Detached HEAD: empty branch name later used in `git checkout ""` | `src/create.rs:380` | 3 |
| A31 | M | Test suite: Debug-format trivia tests, vacuous skip-on-failure git tests, inline test mods, no coverage of the above failure modes | `tests/`, inline mods | 8 |
| A32 | H | `gx add` write path bypasses `FileSet`; `repo_path.join(file_path)` unvalidated (abs path / `..` / `.git` / symlink escape) | `src/create.rs:653`, `src/file.rs:66` | 1 |
