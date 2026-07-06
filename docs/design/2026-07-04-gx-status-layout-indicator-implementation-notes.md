# Implementation Notes: Three-State Layout Indicator for `gx status`

## Phase 1: `Layout` enum + `Repo.layout`

### Design decisions
- `Layout` enum added to `src/repo.rs` with exactly the derives and doc
  comments from the design doc's Data Model section (`Debug, Clone, Copy,
  PartialEq, Eq`; `Flat`/`Bare`/`Unknown`) - `src/repo.rs:Layout`.
- `layout: Layout` field added to `Repo`, set in the three constructors per
  spec: `Repo::new` -> `Layout::Flat`, `Repo::from_container` ->
  `Layout::Bare`, `Repo::from_slug` -> `Layout::Unknown` -
  `src/repo.rs:Repo::new,from_container,from_slug`.
- Grepped for `Repo {` literals repo-wide before editing; the only struct
  literals are the three constructors themselves (other matches were trait
  method signatures/return types), so no other call site needed updating.
- Extended `tests/bare_layout_test.rs`'s existing mixed-fixture test
  (`test_bare_container_counts_as_one_repo`) to assert the discovered set is
  exactly `{Flat, Flat, Bare}` (two flat repos + one bare container), and its
  container test (`test_bare_container_repo_points_at_default_worktree`) to
  assert `Layout::Bare` on the discovered container repo - satisfying the
  doc's "mixed fixture yields `{Flat, Bare}`" success criterion.
- Added direct unit tests in `src/repo.rs`'s existing `#[cfg(test)] mod tests`
  block for all three constructors (`test_new_sets_flat_layout`,
  `test_from_slug_sets_unknown_layout`, `test_from_container_sets_bare_layout`)
  so each constructor's layout assignment is bitten independently of
  discovery-level integration coverage.

### Deviations
- The design doc's Phase 1 scope only names `tests/bare_layout_test.rs` as the
  test surface. To keep the new field from being classified dead code by the
  crate's `-D warnings` clippy gate (the field has no reader until Phase 2's
  `classify_view`/`layout_view()` land), two existing `debug!` discovery-log
  lines in `discover_repos` (`src/repo.rs`) were extended to include
  `layout={:?}`. This is a genuine diagnostic read (consistent with the
  repo's function-level debug-logging convention), not a stub or fake
  consumer - same effect (field is live, no `#[allow(dead_code)]` needed),
  correct seam (logging, not manufactured business logic).
- Added direct unit tests for `Repo::new`/`Repo::from_slug`/
  `Repo::from_container` in `src/repo.rs` beyond the doc's literal instruction
  to "extend `tests/bare_layout_test.rs`" - this is coverage the doc's own
  Phase 1 success criteria explicitly ask for (`from_container(..).layout ==
  Bare`, `new(..).layout == Flat`, `from_slug(..).layout == Unknown`) and the
  repo's testing convention (every public function gets a happy-path test);
  no behavior beyond the spec was added.

### Tradeoffs
- Reading `repo.layout` via `debug!` logging vs. `#[allow(dead_code)]`: chose
  logging because `rust.md` forbids `#[allow(dead_code)]` outright (the
  "temporarily tolerated during active transitions" carve-out in the rule
  still reads as an exception to avoid, and a genuine log consumer is
  strictly better - it also gives `--log-level debug` real diagnostic value
  today, not just a silence-the-linter placeholder).

### Open questions
- None.
