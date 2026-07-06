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

## Phase 2: Three-state rendering (status only) + starship palette

### Design decisions
- Added `LayoutView<'a>` + `classify_view` to `src/output.rs` exactly per the
  doc's API Design section (`Flat`/`WorktreeMatched{leaf}`/
  `WorktreeDiverged{leaf}`, and the identical match arms) - pure, no I/O,
  directly unit-testable (`src/output.rs:classify_view`).
- Added `layout_view()` to the `UnifiedDisplay` trait with a default `None`
  body, overridden ONLY in `impl UnifiedDisplay for RepoStatus`
  (`src/output.rs:UnifiedDisplay::layout_view`, `RepoStatus::layout_view`) -
  `CheckoutResult`, `CreateResult`, `ReviewResult`, and the pre-existing
  (currently unused) `&RepoStatus`/`&CheckoutResult`/`&CreateResult`/
  `&ReviewResult` impls all keep the default, per the doc's explicit "only
  `RepoStatus` overrides it."
- Added the four Catppuccin Mocha RGB consts (`LAYOUT_SLUG_RGB`,
  `LAYOUT_BRANCH_RGB`, `LAYOUT_MATCHED_LEAF_RGB`, `LAYOUT_DIVERGED_RGB`) as
  module-level consts in `src/output.rs`, matching the doc's table verbatim.
- Refactored `display_unified_format` to delegate to a new pure
  `render_unified_line<T>` that branches on `item.layout_view()`: `None`
  calls the original inline formula unchanged (still using
  `format_repo_path_with_colors` for the repo-identity column, magenta for
  the branch column); `Some(view)` calls two new pure helpers,
  `format_layout_identity` (connector glyph + leaf glued onto the slug,
  Catppuccin palette) and `format_layout_branch` (blank for
  `WorktreeMatched`, bold-green branch otherwise) -
  `src/output.rs:render_unified_line,format_layout_identity,format_layout_branch`.
  `display_unified_format` itself is now a two-line shell: call the pure
  renderer, `println!` it, then the untouched error-line handling. This
  return-data-not-side-effects split is what makes the render tests possible
  without capturing stdout.
- SHA and emoji columns are computed once, before the `None`/`Some` branch,
  and used identically on both paths - the doc's color table names only
  slug/branch/connector/leaf, so SHA (`bright_black`) and emoji styling are
  intentionally unchanged for status rows.
- Verified manually with a real mixed fixture (`cargo build --release`, a
  temp workspace with one flat repo and two bare containers - one
  leaf==branch, one leaf!=branch): `gx status`, `gx status --no-color`, and
  `CLICOLOR_FORCE=1 COLORTERM=truecolor gx status` all matched the doc's
  worked example exactly, including the exact
  `\x1b[1;38;2;203;166;247m` (slug) / `\x1b[38;2;166;227;161m` (`\u{2261}`) /
  `\x1b[38;2;142;116;173m` (matched leaf) / `\x1b[1;38;2;166;227;161m` (shown
  branch) / `\x1b[38;2;250;179;135m` (`\u{2248}` + diverged leaf) ANSI
  sequences; `gx checkout` against the same workspace rendered unchanged
  (plain cyan slug, no glyph).
- Updated `README.md` and `gx.yml`'s `output:` section with the `\u{2261}`/
  `\u{2248}` scheme, per the doc's Phase 2 rollout note.

### Deviations
- **Test placement: inline `#[cfg(test)] mod tests { ... }` at the bottom of
  `src/output.rs`, not a `src/output/tests.rs` submodule file.**
  `~/repos/.claude/rules/rust.md` states tests should live in their own
  submodule file; however every existing source file in this repo (including
  `src/repo.rs`, edited by Phase 1 of this very design doc) uses the inline
  `mod tests { ... }` block. Matched the repo's actual, consistent, 100%
  convention over the aspirational global rule, per the phase-implementer
  instruction to match surrounding code's style. Same effect (tests exist,
  are colocated, and run under `cargo test`); correct seam for this specific
  codebase.
- **`colored`'s truecolor output silently downgrades to the nearest 4-bit
  ANSI color without `COLORTERM=truecolor`/`24bit`, and its "should colorize"
  flag is a process-global that defaults to off when stdout isn't a TTY (as
  it isn't under `cargo test`).** Neither behavior is mentioned in the design
  doc's color table. Render tests that assert exact RGB ANSI sequences force
  `colored::control::set_override(true)` and `COLORTERM=truecolor` for their
  duration (restored after, guarded by a `Mutex` to serialize against other
  tests touching the same process-global state - the same env-var-lock
  pattern `rust.md` mandates for platform-path tests). This is a test-harness
  detail, not a behavior change: a real terminal that advertises truecolor
  gets the exact RGB values in production; one that doesn't gets `colored`'s
  own nearest-color degrade, exactly as the pre-existing magenta/cyan/
  bright_black status/checkout output already did before this feature.

### Tradeoffs
- Pure-formatter-plus-thin-`println!`-shell (`render_unified_line` /
  `format_layout_identity` / `format_layout_branch`) vs. testing
  `display_unified_format` by capturing stdout: chose the pure-function split
  because it matches the repo's "return data, not side effects" convention,
  makes the render tests deterministic and parallel-safe, and is the only way
  the doc's own stated rationale for a pure classifier ("the alignment bug's
  test could not bite because it validated printed output") is actually
  honored for the printed-line assertions too.
- `format_layout_branch`/`format_layout_identity` as free functions taking
  `&LayoutView<'_>` vs. methods on `LayoutView`: kept them as free functions
  in `output.rs` since they also need `use_colors`/`width`/the raw
  `repo_slug`/`branch` strings that don't belong on the pure classification
  type; keeps `LayoutView` a plain data enum with no rendering knowledge.

### Open questions
- None.
