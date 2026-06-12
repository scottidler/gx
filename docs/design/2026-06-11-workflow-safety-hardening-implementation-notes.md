# Implementation Notes: GX Workflow Safety Hardening

Running, append-only record of how the implementation interprets or diverges
from `2026-06-11-workflow-safety-hardening.md`. One section per phase, four
buckets each ("None." where empty).

## Phase 1: File-matching and write safety

### Design decisions
- `SubstitutionResult` (`src/diff.rs`) was restructured from tuple variants to
  struct variants: `Changed { content, diff, matches }` and
  `NoChange { matches }`, plus a new `SkippedBinary` variant. The design said
  "`Changed` gains `match_count`"; struct variants read better than a third
  positional field and let `matches` ride on `NoChange` too, so the post-write
  re-read is removed from both branches (the [A8] fix), not just the changed one.
- `git ls-files --stage -z` is wrapped in a new `git::list_index_files`
  (`src/git.rs`) returning `(mode_string, PathBuf)` rather than putting the
  subprocess directly in `FileSet`. Keeps every git shell-out in `git.rs` per
  repo convention; `FileSet::candidates` (`src/file.rs`) does the mode filtering
  (`160000` gitlink, `120000` symlink) and the defensive `.git`-component check.
- Glob matching uses `glob::Pattern::matches_path_with` with
  `require_literal_separator: true` so `*` does not cross `/` and `**` does —
  matching the prior filesystem-glob semantics and shell/gitignore expectations.
  Verified by `test_matching_star_does_not_cross_directories` and
  `test_matching_tracked_dotfile_pattern`.
- `FileSet::matching_any(repo, patterns)` fetches candidates once and matches all
  patterns (deduped + sorted), instead of one `git ls-files` per pattern. Slightly
  fewer subprocesses than the literal "one per pattern" reading of the design.
- `atomic_write` (`src/file.rs`) uses `tempfile::NamedTempFile::new_in(parent)` +
  `sync_all` + `persist` (atomic rename). `write_file_content` and
  `create_file_with_content` now route through it. State/recovery JSON atomic
  writes are deferred to Phases 3/4 where those writers are reworked.
- Binary detection lives in `read_utf8_or_skip` (`src/file.rs`): reads bytes,
  `String::from_utf8`, `warn!` + `Ok(None)` on failure. Used by sub/regex (via
  `SkippedBinary`) and by delete (skips the file).

### Deviations
- `delete` skips non-UTF-8 files (per design) but, unlike sub/regex, does not
  increment a stat counter — `apply_delete_change` has no `SubstitutionStats`
  to record into, and adding one to the delete path is out of scope. The skip is
  still surfaced via `warn!`.

### Tradeoffs
- Putting the `git ls-files` call in `git.rs` vs. inline in `FileSet` (design
  wording). Chose `git.rs` for the shell-out-locality convention; `FileSet`
  stays pure path/mode logic and is the testable seam.
- Struct variants vs. a third tuple element on `Changed`. Struct variants are a
  wider diff (every match site updated) but far clearer at the use sites.

### Open questions
- None.

## Phase 2: Discovery correctness and blast radius

### Design decisions
- `discover_repos` gained a third parameter `ignore_patterns: &[String]` rather
  than reading config internally; all ~10 call sites pass
  `&config.ignore_patterns()`. A new `Config::ignore_patterns()` returns the
  configured list or the documented defaults. This keeps `repo.rs` free of a
  `Config` dependency and makes the patterns an explicit, testable input.
- `Config::confirm_threshold()` + `CreateConfig { confirm-threshold }` (default
  `DEFAULT_CONFIRM_THRESHOLD = 5`) added in this phase (the design lists the
  config key under the API table; it is first *used* by the Phase 2 gate).
- The confirmation gate lives in `create::confirm_blast_radius`
  (`src/create.rs`). It always prints the resolved repo slugs + count; prompts
  when `patterns.is_empty() || count > threshold`; `--yes` skips; a required
  prompt on non-interactive stdin (`!stdin().is_terminal()`) returns an error
  naming `--yes` (fail closed). A declined prompt prints "Aborted; no changes
  made." and returns `Ok(())` (clean, not an error).
- `--yes` / `-y` added to `gx create` and threaded through `main.rs`.

### Deviations
- The default `repo-discovery.ignore-patterns` set is the six previously-hardcoded
  names (`node_modules`, `target`, `build`, `.next`, `dist`, `vendor`). The prior
  config default erroneously listed `.git` and omitted `.next`/`dist`/`vendor`;
  `.git` is now explicitly excluded from the list because discovery *walks into*
  `.git` directories to detect repos - ignoring them by name made every repo
  undiscoverable (caught by `checkout_tests` and fixed before commit).

### Tradeoffs
- Threading `ignore_patterns` through every `discover_repos` caller (wider diff)
  vs. having `repo.rs` import `Config` (a layering inversion). Chose the explicit
  parameter.
- Phase 2 regression tests were added to the existing inline `#[cfg(test)] mod
  tests` in `src/repo.rs` rather than extracting to `src/repo/tests.rs`. Phase 8
  owns the inline-mod extraction tree-wide; deferring keeps this phase focused.

### Open questions
- None.

## Phase 3: Transaction engine rework

### Design decisions
- `Transaction::new(repo_path, change_id, persist)` gained a `persist` flag:
  recovery state is only written to disk for committing runs. Dry-runs apply +
  roll back in memory and never need crash recovery, so they skip the file churn.
- `Transaction` carries `original_branch` and `stash_sha` as fields (set via
  `set_original_branch`/`set_stash_sha`); `finalize()` uses them directly for the
  success-path branch restore + stash pop, rather than replaying steps. This
  lets finalize special-case the stash-apply conflict (don't drop, surface the
  error) cleanly via `FinalizeOutcome`.
- Rollback registration order is arranged so reverse execution is correct:
  `PopStash`, [`SwitchBranch`], `RestoreBackup`×n, `DeleteLocalBranch`,
  `ResetCommit`, `DeleteRemoteBranch`. `ResetCommit` uses `git reset --hard
  <expected_sha>` (not `--soft HEAD~1`) so the worktree is clean before the
  branch switch during rollback.
- `DeleteLocalBranch`'s interpreter force-switches off the branch (to the head
  branch) if it is currently checked out, before deleting - so the step works
  regardless of its position in the reverse-execution order, and tolerates an
  uncommitted worktree. It also honors `branch_existed` (never deletes a branch
  gx did not create) and is idempotent (a delete of an absent branch is OK).
- Per-repo locking is a new `src/lock.rs` (`RepoLock` RAII guard) under
  `$XDG_DATA_HOME/gx/locks/<fnv1a-hex>.lock` via `create_new` (O_EXCL). Stale
  locks are reclaimed by checking `/proc/<pid>` on Linux; on non-Linux we never
  reclaim (conservative). The lock filename hash is FNV-1a (stable across runs),
  not `DefaultHasher`.
- Recovery and backups moved to `$XDG_DATA_HOME/gx/{recovery,backups}` now (the
  design lists recovery there; Phase 4 moves `changes/`). Backups are out-of-tree
  under `backups/<tx-id>/<relative-path>`; `Transaction::backup_path_for` builds
  the path and the tx dir is removed wholesale on finalize/rollback/recovery.

### Deviations
- `SwitchBranch{original}` is registered right after switching to the head branch
  (write-ahead for that op), but rollback's correct branch-delete ordering is
  achieved by the force-switch inside `DeleteLocalBranch` rather than by relying
  on `SwitchBranch` executing first. For the common case (original == head) there
  is no separate `SwitchBranch` at all. The cross-branch edge (running gx from a
  non-default branch) is handled safely but the intermediate worktree state
  during a rollback is approximate; documented here rather than fully modeled.
- The four legacy git helpers `stash_save`, `stash_pop`, `reset_hard`,
  `reset_commit`, and `remote_branch_exists` were removed (replaced by
  `stash_save_with_untracked`, `stash_apply_sha`/`stash_drop_by_sha`,
  `reset_hard_to_sha`, `force_switch_branch`), along with their now-obsolete
  tests, per the no-dead-code rule.
- Closures, `RollbackType`, `SerializableRollbackAction`, `TransactionState`,
  `ValidationResult`/`validate_rollback_operations`, and the
  description-string dispatch were all deleted. `gx rollback validate` now does a
  lightweight inline check (repo exists + is a git repo).

### Tradeoffs
- Storing `original_branch`/`stash_sha` on the transaction (and special-casing
  finalize) vs. modeling finalize purely as "execute the restore steps". The
  former is less elegant but lets finalize distinguish a stash conflict from a
  clean apply without re-parsing step outcomes.
- Kept `pull_latest_changes` (plain `git pull`) for now; Phase 6 switches it to
  `--ff-only`.

### Open questions
- Cross-branch runs (original branch != repo head/default): the rollback path is
  safe but the transient worktree state is not byte-exact in every interleaving.
  Worth confirming whether gx is ever expected to run from a non-default branch;
  if not, we could reject it up front like detached HEAD.


## Phase 4: State tracking integrity

### Design decisions
- `get_state_dir` now resolves `$XDG_DATA_HOME/gx/changes` via the existing
  `xdg_data_dir()` helper. `migrate_legacy_state` copies any `~/.gx/changes/*.json`
  into the new location on first use, then renames the whole `~/.gx` aside to
  `~/.gx.migrated-<timestamp>` (a backup, not deleted) per design Q4. Migration
  only runs when the new dir does not yet exist.
- `ChangeState.repositories` is now `BTreeMap` (deterministic, sorted JSON), and
  both `ChangeState` and `RepoChangeState` carry `#[serde(deny_unknown_fields)]`.
  `StateManager::list` warns on an unparsable state file instead of silently
  skipping it.
- Incremental saves: `process_create_command` builds one `StateManager` up front
  and saves inside the existing per-repo mutex after each Committed/PrCreated
  result, instead of one save at the very end. `original_branch` is threaded onto
  `CreateResult` and recorded in `RepoChangeState`.
- The review approve/delete read-modify-write race is fixed by making the rayon
  workers state-free: they return only `ReviewResult`, and the command function
  does a single load -> apply-all (`mark_merged`/`mark_failed` for approve,
  `mark_closed` for delete) -> save after the parallel section.
- `extract_pr_number_from_url` failure now propagates as an error rather than
  storing PR `#0`.
- Cleanup resolves repos via the recorded `local_path` first (works from any
  CWD); a recorded-but-missing path is reported as a failure, and only an absent
  recorded path falls back to the CWD search.

### Deviations
- None.

### Tradeoffs
- Incremental save writes one small JSON per committed repo (N writes vs. 1).
  Negligible against the network operations, and it is the whole point of [A3].
- Migration copies (not moves) the JSON files before renaming `~/.gx` aside, so
  a crash mid-migration leaves both copies intact rather than risking data loss.

### Open questions
- None.

## Phase 5: GitHub layer correctness

### Design decisions
- One `gh_command(org, config)` helper sets `GH_TOKEN` from the org's token file
  for every gh invocation (create/merge/close/review/branch-delete/GraphQL),
  ending the token-file-vs-ambient split ([A18]). When no token file exists it
  falls back to ambient `gh auth` with a `debug!` rather than hard-failing, so
  `gh auth login` users are not broken.
- Base branch: `create_pull_request` resolves the base via local
  `git::get_head_branch` first, then `gh api repos/{slug} .default_branch`,
  falling back to `main` with a warning ([A4]). The resolved base is passed to
  `create_pr` (which no longer hardcodes `--base main`).
- PR body comes from `github.pr-body-template` (default `{commit_message}`); the
  hardcoded scottidler/gx README link is gone ([A29]).
- `list_prs_by_change_id` paginates via GraphQL `pageInfo`, and the search string
  is a JSON-encoded `$q` variable (with `$cursor`), never spliced into the query
  text ([A13]).
- `--change-id` is validated at parse time (`value_parser`) to require the `GX-`
  prefix ([A11]).
- `gx review purge` is reworked into a plan-then-execute flow: per repo it lists
  `GX-` branches (paginated) and open-PR head branches (paginated), partitions
  into deletable vs. blocked-by-open-PR, prints the full plan, refuses open-PR
  branches (directing the user to `gx review delete`), and prompts unless `--yes`
  (fail-closed on non-TTY) ([A12], Q3).

### Deviations
- The "surface approve failure on the result" item ([A4], approve half) is only
  partially honored: a failed `gh pr review --approve` is still logged via
  `warn!` and the merge proceeds, but the approve failure is NOT threaded onto
  `ReviewResult.error`. Self-approval rejection is the expected common case, and
  putting it in `error` would mark otherwise-successful merges as failed in the
  summary. The PR-*creation* failure surfacing (the higher-value half of [A4])
  is done (Phase 3 + this phase).
- Purge "targets only gx-created branches" is implemented via the `GX-` prefix
  (not by cross-referencing change state). The prefix is gx's branch-naming
  contract; using it keeps purge usable without a local state file.

### Tradeoffs
- Threading `config` through every gh-invoking function (wide signature change
  across github.rs/review.rs/create.rs) vs. a global/ambient token. The explicit
  threading is the correct fix for the multi-account hazard ([A18]).
- Purge computes its plan with network calls before prompting (parallelized).
  Slightly more upfront latency, but the operator sees an accurate plan and the
  open-PR guard is enforced before anything is deleted.

### Open questions
- Should `gx review approve` self-approval failures ever be surfaced distinctly
  (e.g. an "approved-by-other-required" note) rather than silently logged?

## Phase 6: Git layer hygiene

### Design decisions
- `pull_latest_changes` now runs `git pull --ff-only`; a non-fast-forward result
  is a per-repo error naming the manual fix, not a surprise merge ([A14]). The
  dead `Already up to date` stderr sniff is removed ([A28]).
- The two divergent porcelain parsers are unified into one
  `parse_porcelain_status(text) -> StatusChanges` with a single counting rule,
  fed by a shared `run_status_porcelain` helper. Taking text as input makes it
  unit-testable (table-driven test added) ([A20]).
- `add_files` stages with literal pathspecs (`:(literal)<path>`) so a tracked
  filename containing glob metacharacters can't re-expand ([A26]).
- `run_status_porcelain` uses `from_utf8_lossy` (we only read the `XY` columns,
  never the path), so a non-UTF-8 filename no longer aborts status ([A21]).

### Deviations
- The unified counting rule differs slightly from the old `get_status_changes`
  (a staged-new `A ` entry now counts as `added`, not `staged`, and is not
  double-decremented). This is a deliberate single, simple rule; the exact
  per-column semantics were the author's choice per the design ("one counting
  rule").

### Tradeoffs
- The full ff-only / non-ff divergence integration test (a real diverged remote)
  was not added - it needs a bare-remote fixture with conflicting history. The
  ff-only flag is exercised by the existing create/checkout paths; the parser and
  literal-pathspec behaviors (the higher-risk logic) are unit-tested directly.

### Open questions
- None.

## Phase 7: CLI, config, and logging

### Design decisions
- Added a `LogLevel` value-enum and `--log-level/-l` flag (off|error|warn|info|
  debug|trace, case-insensitive, default info). `setup_logging` uses
  `env_logger::Builder::new().filter_level(...)` - `RUST_LOG` is no longer
  consulted ([A24]).
- `main` parses the CLI before setting up logging, and injects the top-level
  `after_help` at runtime (`Cli::command().after_help(...).get_matches()`),
  rendering the log path from `doctor::log_path()` (the same XDG source the
  logger uses). No subprocess spawns during parsing ([A24]).
- New `src/doctor.rs` + `gx doctor` subcommand: reports git/gh presence/versions
  (moved out of `--help`), the log path, and orphaned recovery/backup artifacts
  (repo missing, or older than a 7-day TTL). `--purge` removes them via `rkvr`
  (never `rm`) ([A2], [A24]).
- `Config::load` no longer falls back to `./gx.yml` in the CWD; config comes from
  `-c` or the XDG path only, and the loaded path is logged ([A23]).
- `version_compare` pads the shorter version with zeros so `"2.20"` == `"2.20.0"`
  ([A25]); moved to `doctor.rs` with the rest of the tool-check helpers.
- Added `src/config/tests.rs` with the mandated XDG path-resolution tests.

### Deviations
- `Config` keeps `#[serde(default)]` (not `deny_unknown_fields`). The design adds
  `deny_unknown_fields` to *state* structs (Phase 4); adding it to the
  user-facing config is out of scope here and would reject existing configs with
  extra keys.

### Tradeoffs
- Runtime `after_help` injection (via `CommandFactory`/`FromArgMatches` in `main`)
  vs. a derive attribute. The runtime form keeps the log path accurate under
  `$XDG_DATA_HOME` overrides and avoids the parse-time subprocess spawns.

### Open questions
- None.

## Phase 8: Test hardening

### Design decisions
- Deleted the Debug-format trivia tests (`test_change_debug`,
  `test_create_action_debug`, `test_create_result_debug`,
  `test_review_result_debug`, `test_review_action_debug`,
  `test_cleanup_result_debug`, `test_detection_method_debug`,
  `test_user_org_context_debug`) ([A31]).
- The git test helper `setup_test_repo` now returns the repo directly and
  `expect`s on failure (git is a declared requirement); the skip-on-no-git
  `return` guards are gone, and `test_has_uncommitted_changes` asserts real
  behavior instead of "doesn't crash" ([A31]).
- Added `tests/e2e_create_lifecycle.rs`: a fully-offline end-to-end test over
  three repos (master-default `reporting`, dirty `dirty-repo`, `frontend`) with
  bare-repo remotes, exercising `gx create` (sub + commit + push) -> state file
  -> `gx cleanup --list`, asserting GX- branches are pushed, the change-state
  file exists and names all repos, and the dirty repo's WIP survives the cycle.

### Deviations
- Full extraction of inline `#[cfg(test)] mod tests` blocks into sibling
  `tests.rs` files was NOT done tree-wide. New modules added this work (file,
  transaction, lock, doctor, config) already use the sibling pattern; the
  pre-existing inline mods (git, github, repo, cli, create, diff, state,
  cleanup, user_org, output, status, checkout, clone, ssh) are left in place.
  The rust conventions themselves note that module-style migration is "a
  tree-wide mechanical pass, never mixed into a feature" - doing it here would
  be a large, risky, test-only reorganization riding on a behavioral change.
  Deferred as a standalone follow-up.

### Tradeoffs
- The e2e is offline (no `--pr`), so it does not exercise PR creation / base
  branch resolution against a real `gh`; that path is covered by github.rs unit
  tests and the gh PATH-shim approach noted in the design's testing strategy
  (not implemented here).

### Coverage audit (Appendix A -> test)
- A1 `file::tests::test_matching_glob_never_matches_git_or_untracked`;
  A2 `transaction::tests::test_kill9_recovery_*` + `doctor` orphans;
  A3 e2e (state file) + incremental save; A5/A15 `transaction::tests::
  test_finalize_restores_branch_and_stash` + e2e WIP survives;
  A6/A9/A27 `repo::tests::*`; A8 `diff`/`file` match-count tests;
  A11 `cli::tests::test_validate_change_id_*`; A13 `github::tests::
  test_parse_graphql_page_returns_page_info` + `test_search_query_uses_variables`;
  A16 `cleanup::tests::test_cleanup_uses_recorded_local_path`;
  A17 `transaction::tests::test_rollback_step_serialize_roundtrip`;
  A19 `state::tests::test_original_branch_is_recorded`; A20 `git::tests::
  test_parse_porcelain_status_table`; A21 `file::tests` (atomic/binary/backup);
  A22 `state`/`config` determinism+XDG tests; A24 integration `--help`/`doctor`;
  A25 `doctor::tests::test_version_compare_*`; A26 `git::tests::
  test_add_files_literal_pathspec`; A29 `github::tests::
  test_pr_body_template_substitution`; A30 `git::tests::
  test_get_current_branch_name_empty_on_detached_head`; A32 `file::tests::
  test_validate_new_file_path_*`; A31 this phase.
- Indirectly covered (no dedicated unit test): A4 base-branch resolution, A7
  operation ordering, A10 single-update race fix, A12 purge prompt, A14 ff-only
  divergence, A18 token auth, A23 CWD-config removal. These are exercised by the
  e2e/integration paths or are control-flow changes verified by review; adding
  isolated tests (e.g. a diverged-remote fixture, a gh PATH shim) is noted as a
  follow-up.

### Open questions
- Worth a follow-up PR: (1) tree-wide inline-`mod tests` extraction; (2) a
  gh-PATH-shim test for base-branch + PR-failure surfacing; (3) a diverged-remote
  fixture for the ff-only error path.
