# Implementation Notes: gx Shakedown Fixes

Running record of how the implementation of `docs/design/2026-07-13-gx-shakedown-fixes.md`
diverges from or interprets the design doc. Append-only.

## Phase 0: Prove the squash-detection primitive (zero code)

### Design decisions
- None. Pure verification spike, no code committed.

### Deviations
- None.

### Tradeoffs
- None.

### Spike transcript (temp git repo, single-commit branches)
Confirmed the design doc's core claim and the review-panel's fail-closed finding (#3):

- **Case 1 -- squash-merged single-commit branch:** `git cherry HEAD feature-squash` emitted `- <sha>` (one `-` line, ZERO `+` lines) -> detected as merged. `git merge-base --is-ancestor feature-squash HEAD` exited 1 (false) -> the shipped bug, reproduced.
- **Case 2 -- branch with a change absent from base:** `git cherry <base> feature-unmerged` emitted `+ <sha>` (a `+` line) -> refused. The Phase-4 anti-divergence guarantee is preserved by the new primitive.
- **Case 3 -- fatal cherry (bad base ref):** `git cherry does-not-exist feature-squash` printed `fatal: unknown commit` and exited 128. Confirms the fail-closed contract: a non-zero exit must map to `Err` (preserve the branch), never to "no `+` lines -> merged -> delete".

### Detection logic proven
- Merged iff `git cherry <base_ref> <branch>` exits 0 AND emits zero `+` lines.
- Any non-zero exit -> `Err` -> cleanup preserves the branch.

### Open questions
- None. Primitive cleanly distinguishes squash-merged from unmerged; proceeding to Phase 1.

## Phase 1: Fix `--pr` parsing (Bug 2)

### Design decisions
- Removed the `PR` enum (`cli.rs`) entirely; `Commands::Create` and
  `Commands::Apply` now carry `pr: bool` + `draft: bool`, with
  `#[arg(long, requires = "pr")] draft: bool` on both — a bare `--draft`
  with no `--pr` is a clap `ErrorKind::MissingRequiredArgument`, never a
  silent no-op (review finding #1). Verified live: `gx create --files x
  --commit m --draft add f c` -> `error: the following required arguments
  were not provided: --pr`.
- Threaded the two booleans through the complete consumer chain named in the
  design doc's Phase 1 bullet, letting the compiler enforce exhaustiveness at
  every call site: `cli.rs` -> `main.rs:157,188,196-197` (destructure +
  dereference, `pr`/`draft` are `Copy`) -> `create.rs` (`process_create_command`
  :120-131, `run_llm`:328-338, `process_apply_command`:425-431) ->
  `create/core.rs` (`execute_create`:136-146, `process_single_repo`:304-314,
  `update_change_state`:236 now takes `draft: bool` directly instead of
  re-deriving `is_draft` from `Option<&PR>`, `record_final_state`:830-835,
  `create_pull_request`:1339-1345) -> `create/core/apply.rs`
  (`execute_apply`:49-56) -> `github.rs` (`create_pr`:172-179, the actual `gh
  pr create --draft` wiring at `:202-204` is now a plain `if draft`) ->
  `gx-mcp/src/logic.rs::create_apply` (`execute_apply` call now passes
  `false, false` for `pr, draft`; MCP's create-apply tool still does not
  expose PR creation, per its existing design-doc deviation note).
- At the PR-creation branch point (`create/core.rs` `process_single_repo`,
  the old `match pr { Some(pr) => ..., None => ... }`), rewrote as `if pr {
  ... } else { ... }` since `pr` is now a plain bool — same control flow,
  no behavior change.
- Added the CLI parse tests inline in `cli.rs`'s existing `#[cfg(test)] mod
  tests { ... }` block (matching that file's current convention) rather than
  extracting to `cli/tests.rs`: `cli.rs` has no `cli/` module directory today,
  and converting its test-file layout is an unrelated mechanical change out
  of scope for this phase's bug fix.

### Deviations
- The design doc's API Design block (lines 90-91) shows the fix as two
  fields directly on a struct with a bare `#[arg(long)] pr: bool` example;
  the real seam is two clap-derived fields on the `Create` and `Apply`
  `Commands` enum variants (there is no single flat struct), and the actual
  `requires = "pr"` attribute needed the full `#[arg(long, help = "...",
  requires = "pr")]` form to also carry updated help text. Same effect,
  correct seam — no behavior difference from what the doc specifies.
- `docs/subcommands.md` is NOT updated in this phase (the design doc assigns
  that to Phase 3, "Tests that bite + docs"); this phase touches only
  `cli.rs`'s own `--help`/`after_help` strings named in the Phase 1 bullet.

### Tradeoffs
- Kept the CLI parse tests inline in `cli.rs` rather than extracting to a
  `cli/tests.rs` submodule (the repo's `rust.md` convention for new test
  code) — extracting the file's pre-existing two `validate_change_id` tests
  at the same time would have mixed a test-layout refactor into this bug-fix
  phase's diff. Deferred; flag if a later phase wants the extraction.

### Open questions
- None. Both reviewers' three findings assigned to Phase 1 (#1 fail-loud,
  #2 complete consumer inventory, #5 apply coverage) are folded in and
  verified against the compiler (exhaustive match/field usage) and live
  `--help`/parse runs; `otto ci` is green.

## Phase 2: Fix cleanup squash detection (Bug 1)

### Design decisions
- Added `branch_changes_in_base(repo_path, base_ref, branch_name) -> Result<bool>`
  in `src/git.rs` beside `branch_merged_into_base`. It runs
  `git cherry <base_ref> <branch_name>` through the existing hardening
  `run_checked` and returns `Ok(true)` iff exit 0 AND zero `+` lines in stdout
  (patch-identity: every branch commit's diff is already in base). This handles
  gx's own squash merges, unlike the old commit-identity
  `git merge-base --is-ancestor`.
- FAIL CLOSED (review finding #3, correctness-critical) —
  `branch_changes_in_base` gates on a SUCCESS exit BEFORE reading stdout:
  `match output.status.code() { Some(0) => count + lines, other => Err(..) }`.
  Because `run_checked` returns `Ok` on a non-zero exit and `git cherry` exits 0
  regardless of `+`-line presence, a fatal cherry (bad ref, exit 128) yields
  empty stdout that would naively read as "no + lines -> merged -> delete";
  the success-exit gate maps it to `Err` instead, which propagates so cleanup
  PRESERVES the branch. Mirrors the exit-code mapping the old `--is-ancestor`
  guard did at the former `git.rs:1118-1130`.
- Replaced ONLY the proof step inside `branch_merged_into_base`: the
  `let base_ref = format!("origin/{base}")` line is kept, then delegates to
  `branch_changes_in_base(repo_path, &base_ref, branch_name)`. Base-name
  resolution (`get_head_branch`), the `fetch_origin` of the base, the
  `PrMerged` eligibility gate (`cleanup.rs:308`), the call site
  (`cleanup.rs:369`), and the `--force` bypass are all unchanged.
- Updated the `branch_merged_into_base` doc comment to describe the new
  patch-identity delegation instead of the removed `--is-ancestor` mechanics
  (names/docs tell the truth).
- DEBUG logging per the repo rule: entry log records
  `repo_path + base_ref + branch`; the exit log records the `+`-line count
  (the chosen branch: `Ok(plus_lines == 0)`).

### Deviations
- The design doc's API Design block (line 85) shows `fn branch_changes_in_base(repo: &Path, ...)`.
  Implemented as a private (module-visible) `fn` with parameter name
  `repo_path` to match the sibling `branch_merged_into_base` and the file's
  existing naming. Same effect, correct seam. It is not `pub` because the only
  production caller is `branch_merged_into_base` in the same module; the
  Phase-2 unit test reaches it via `use super::*` in the inline `mod tests`.
- Fail-closed unit test placed inline in `git.rs`'s existing
  `#[cfg(test)] mod tests { ... }` block (matching that file's current
  convention) rather than a `git/tests.rs` submodule — same reasoning recorded
  for Phase 1's `cli.rs` tests; extracting git.rs's large existing inline test
  module is an unrelated layout refactor out of scope for this bug fix.

### Tradeoffs
- Chose to keep the whole exit-code `match` (`Some(0)` vs `other`) rather than a
  terser `if output.status.success()` early-return, so the failure branch carries
  the exit code and stderr in its `Err` message (diagnosability), matching the
  old guard's error shape.

### Open questions
- None. Phase 3 owns the positive squash-merge cleanup bite test and the
  `--pr`/`--draft` parse tests plus `docs/subcommands.md`; this phase added
  only the primitive + the fail-closed unit test, per the phase boundary.

## Phase 3: Tests that bite + docs

### Design decisions
- Added `test_cleanup_squash_merged_branch_is_cleaned_without_force` inline in
  `src/cleanup.rs`'s existing `#[cfg(test)] mod tests { ... }` block,
  immediately after `test_cleanup_preserves_branch_with_commits_absent_from_base`
  (matching that test's harness: a real non-bare `upstream` repo, a `work`
  clone, no mocks). It creates a `GX-squash` branch with ONE commit, pushes it
  to `upstream` (as `gx create` would), simulates `gh pr merge --squash` by
  running `git merge --squash GX-squash` + `git commit` directly on
  `upstream`'s default branch (a new SHA, same diff — the branch's own commit
  never becomes a literal ancestor), then asserts
  `cleanup_change(force=false)` returns `repos_cleaned == 1` and the branch is
  gone. This is the test that would have caught Bug 1 — the earlier
  hardening phase shipped only the negative case.
- The `--pr`/`--draft` clap parse tests (Phase 1, inline in `cli.rs`) and the
  `branch_changes_in_base` fail-closed exit-status test
  (`test_branch_changes_in_base_fails_closed_on_bad_base_ref`, Phase 2, inline
  in `git.rs`) already existed from their respective phases; per this phase's
  scope they are confirmed still green, not re-added.
- Updated `docs/subcommands.md`: the `create` section's usage line and a new
  Behavior bullet now describe `--pr`/`--draft` as boolean flags (`--pr
  [--draft]`, `--draft` requires `--pr`, a bare `--draft` is a clap error);
  added a `--pr --draft` example. `docs/subcommands.md` had no `apply`
  section at all (only `create`/`review`/`cleanup`/etc. were documented), so
  added one describing `apply`'s `--pr [--draft]`/`--yes` surface — the
  design doc's own Phase 1 success criteria explicitly cover `apply`
  alongside `create`, and the CLI's `after_help` for `Apply` already
  documents the same flags this phase is recording in the user-facing docs.

### Deviations
- The design doc's Phase 3 bullet (line 127) describes adding a `--pr`
  clap-parse test and a Phase-2 fail-closed test as this phase's work. Both
  landed earlier than specified: the `--pr`/`--draft` parse tests shipped in
  Phase 1 (`cli.rs`) and the fail-closed exit-status test shipped in Phase 2
  (`git.rs`), per this phase's explicit instructions to not duplicate them.
  Confirmed both are present and green; this phase's own code change is only
  the positive squash-merge test plus docs.
- To make the new positive test deterministic, it checks `work`'s HEAD back
  to the resolved base branch before invoking `cleanup_change`. Without this,
  `git branch -D` legitimately refuses to delete a branch the repo's
  worktree currently has checked out — an orthogonal git constraint, not
  part of the guard under test, and not mentioned in the design doc's test
  description. Same effect as the doc's intent (prove the patch-identity
  guard cleans a squash-merged branch); the extra checkout step is
  scaffolding to isolate that proof from an unrelated git rule.

### Tradeoffs
- Kept the new test inline in `cleanup.rs`'s `mod tests` block rather than
  extracting to `cleanup/tests.rs` (the repo's stated preference for new test
  code) to match the immediately adjacent negative test's location, per this
  phase's explicit placement instruction. Extracting the whole file's
  pre-existing inline test module is an unrelated layout refactor, deferred
  as in Phases 1 and 2.
- Documented `apply` in `docs/subcommands.md` rather than leaving it
  undocumented: the alternative (silence) would leave the doc inaccurate
  about a subcommand whose `--pr`/`--draft` surface this exact phase is
  correcting language for elsewhere in the same file.

### Open questions
- None. All three `otto ci` runs (initial fix, bite-check revert, restored
  fix) are accounted for below; the design doc's Phase 3 and overall
  Acceptance Criteria are verified in the phase report.
