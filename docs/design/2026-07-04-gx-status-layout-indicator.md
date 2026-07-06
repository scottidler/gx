# Design Document: Three-State Layout Indicator for `gx status`

**Author:** Scott A. Idler (via Claude)
**Date:** 2026-07-04
**Status:** In Review
**Review Passes Completed:** 5/5 (draft + correctness + clarity + edge-cases + excellence; review panel folded)

## Summary

`gx` discovers bare containers (`.git` pointer -> `.bare/` + linked worktrees)
as one logical repo, but renders them identically to a flat clone. This doc
makes `gx status` report **three states** - normal flat checkout, bare worktree
where folder == branch, and bare worktree where folder != branch - using the
exact `≡`/`≈` connector-glyph rendering and Catppuccin palette already proven in
Scott's starship prompt. Scope is `gx status` only; other verbs are unchanged.

## Problem Statement

### Background

- **flat clone** - `.git` is a directory; the repo root is the work tree. gx
  discovers it via `Repo::new`.
- **bare container** - `.git` is a pointer to `.bare/` (shared object db) with
  linked worktrees under the container dir. gx discovers it via
  `Repo::from_container`, emitting ONE `Repo` whose `slug` is `org/repo` and
  whose `path` is the *default worktree* (e.g. `.../clyde/main`).
- Scott already solved "how do I show this" in his shell prompt. His starship
  `custom.path`/`custom.branch` render three cases by **replacing the `/`
  between the repo slug and the worktree leaf with a connector glyph**, gated on
  `--git-common-dir` ending in `.bare`:
  1. flat clone -> `~/repos/tatari-tv/verify`, branch trails separately
  2. worktree, leaf == branch -> `tatari-tv/clyde≡main` (branch suppressed)
  3. worktree, leaf != branch -> `tatari-tv/clyde≈main`, real branch trails

### Problem

`gx status` collapses all three into one rendering. A viewer cannot tell a flat
clone from a bare worktree, nor - within a bare worktree - whether the folder
name still agrees with the checked-out branch. State 3 (folder != branch) is the
one worth flagging: the directory name is lying about what is checked out.

The distinction is **known at discovery** (which constructor ran) but discarded:
`Repo` (`src/repo.rs:6-11`) stores only `path`, `name`, `slug` - no layout.

### Goals

- `gx status` positively distinguishes the three states, per row.
- Render using the SAME visual language as Scott's starship prompt: `≡`/`≈`
  connector glyphs, Catppuccin Mocha colors.
- The state is **derived**, never a stored field that can drift:
  - `layout` (Flat vs Bare) is structural, set once at discovery.
  - match vs mismatch is computed at render from `path.file_name()` (leaf) vs
    `branch` - both already in hand, no extra git calls.

### Non-Goals

- **`gx checkout` / `gx create` / `gx review` output.** They share the generic
  `display_unified_format`, but their `UnifiedDisplay::get_branch()` returns a
  `change_id` (`src/output.rs:407,475,543,611`), NOT a checked-out branch, and
  `create`/`review` are not "a repo on a branch." Applying the marker there would
  compare the leaf against a `GX-*` id (always `≈`) and could blank a change-id
  column. The design leaves them byte-identical to today, and exposes an
  extensible seam (below) so a future doc can opt a verb in with a real
  current-branch source. (Scott asked about `gx status`; this is that scope.)
- Changing which layout `clone` defaults to (`[[clone-flat-default-decision]]`).
- `gx clone` output (holds only a `repo_slug: String`, no `Repo` -
  `src/output.rs`), and `gx diff` (a file-level renderer). Parked.
- Expanding a container to show every linked worktree; one row per repo stays.
- The ahead/behind alignment bug - already fixed as a separate targeted change
  (VS16 `⬆️`/`⬇️` -> width-1 `↑`/`↓`); this design builds on that baseline
  (see Resolved Decisions).

## Proposed Solution

### Overview

Carry a `Layout` enum on `Repo`. Add ONE trait method `layout_view()` that
defaults to `None` (verb does not participate) and is overridden only by
`RepoStatus` to classify the row into one of three views. `display_unified_format`
renders the new palette + `≡`/`≈` marker only when `layout_view()` is `Some`;
otherwise it takes the existing rendering path unchanged.

### Architecture

```
discovery (src/repo.rs)
  ├─ is_bare_container -> Repo::from_container -> layout = Bare
  └─ .git is dir ──────> Repo::new ──────────> layout = Flat
                          Repo::from_slug ────> layout = Unknown

render (src/output.rs display_unified_format :747), per row:
  match item.layout_view() {
    None       => existing rendering (branch magenta, slug cyan, no marker)   // checkout/create/review
    Some(view) => new rendering (Catppuccin palette + connector marker)       // status only
  }

  RepoStatus::layout_view() = Some(classify_view(
      repo.layout,
      repo.path.file_name().and_then(|n| n.to_str()),   // leaf: Option<&str>
      self.branch.as_deref()))

  view -> branch column     -> repo-identity column
  Flat              -> branch shown        -> slug (plain)
  WorktreeMatched   -> branch BLANK        -> slug≡leaf   (≡ green, leaf dull-mauve)
  WorktreeDiverged  -> branch shown        -> slug≈leaf   (≈ + leaf peach)
```

Worked example (`gx status`):

```
branch         sha      rmt  repo
               a1b2c3d  🟢   tatari-tv/clyde≡main       WorktreeMatched  (branch blank)
main           e4f5a6b  🟢   scottidler/otto            Flat
feature-x      9c8d7e0  ↑2   tatari-tv/clyde≈main       WorktreeDiverged (branch shown)
HEAD@9c8d7e0   0011223  🟢   tatari-tv/pulse≈main       WorktreeDiverged (detached; HEAD@sha shown)
```

### Data Model

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Flat,    // .git is a directory; repo root is a work tree
    Bare,    // repo IS the default worktree of a bare container (path = that worktree, not the .bare root)
    Unknown, // synthetic repo (from_slug); no filesystem to classify
}

pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub slug: String,
    pub layout: Layout,
}
```

Set in exactly the three constructors (grep confirms no `Repo { .. }` literals
elsewhere): `from_container` -> `Bare`, `new` -> `Flat`, `from_slug` ->
`Unknown`. `RepoStatus` (`src/git.rs:8-16`) is unchanged - it already carries
`repo: Repo`.

### API Design

**Pure classifier** (unit-testable, no stdout - the alignment bug's test could
not bite because it validated printed output against the width crate):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutView<'a> {
    Flat,
    WorktreeMatched  { leaf: &'a str },
    WorktreeDiverged { leaf: &'a str },
}

fn classify_view<'a>(layout: Layout, leaf: Option<&'a str>, branch: Option<&str>) -> LayoutView<'a> {
    match (layout, leaf, branch) {
        (Layout::Bare, Some(l), Some(b)) if l == b => LayoutView::WorktreeMatched  { leaf: l },
        (Layout::Bare, Some(l), _)                 => LayoutView::WorktreeDiverged { leaf: l },
        _                                          => LayoutView::Flat,
    }
}
```

**Opt-in seam** on `UnifiedDisplay` (`src/output.rs:48`):

```rust
fn layout_view(&self) -> Option<LayoutView<'_>> { None }   // default: verb does not participate
```

Only `RepoStatus` overrides it:

```rust
fn layout_view(&self) -> Option<LayoutView<'_>> {
    Some(classify_view(
        self.repo.layout,
        self.repo.path.file_name().and_then(|n| n.to_str()),
        self.branch.as_deref(),
    ))
}
```

Edge behavior, all covered by the classifier + `Some/None` split:
- **detached HEAD** - `get_current_branch` returns `Some("HEAD@<sha>")`
  (`src/git.rs:190`), never `None`. So a detached bare worktree has
  `leaf != "HEAD@<sha>"` -> `WorktreeDiverged`, and the branch column shows
  `HEAD@<sha>` (informative). A detached flat repo stays `Flat`, `HEAD@<sha>`
  shown - unchanged from today.
- **non-UTF-8 leaf** - `file_name().and_then(to_str)` yields `None` ->
  `WorktreeDiverged`'s guard fails -> `Flat`. A safe degrade to plain rendering
  (vanishingly rare; no diagnostic warranted).
- **`Unknown` layout** (`from_slug`) -> `Flat` view.
- **checkout/create/review** -> `layout_view() == None` -> existing rendering,
  untouched. Their `get_branch()`/`change_id` semantics are never fed to the
  classifier.

**Colors** = starship Catppuccin Mocha roles, `colored` `.truecolor`, as module
consts. Applied ONLY on the `Some(view)` (status) path:

| role | RGB | weight | applies to |
|------|-----|--------|------------|
| slug (org/repo) | `203,166,247` | bold | every status row |
| trailing branch (Flat / Diverged) | `166,227,161` | bold | branch column |
| `≡` connector | `166,227,161` | normal | matched connector |
| matched leaf | `142,116,173` | normal | leaf after `≡` |
| `≈` connector + diverged leaf | `250,179,135` | normal | mismatched connector + leaf |

Rendering notes:
- The connector + leaf live in the **repo-identity column, which is last and
  left-aligned**, so the connector's cell width cannot shift any other column and
  `AlignmentWidths` (`src/output.rs:683`) needs no change regardless of how
  `≡`/`≈` measure. `format_repo_path_with_colors` (`src/output.rs:719`) stays as
  the `None`-path renderer; a new view-aware renderer handles the `Some` path.
- **`use_colors == false`** must still emit `slug≡leaf` / `slug≈leaf` (glyph
  carries the signal) and a blank branch column for matched - with ZERO ANSI, so
  piping to a file or `less` stays clean.
- Branch-column blanking happens only for `Some(WorktreeMatched)`, which only
  `RepoStatus` produces; no other verb's column can be blanked.

### Implementation Plan

Deterministic/cheap first, rendering second. No opus phase (mechanical wiring on
an existing seam). No Phase 0 (container detection + default-worktree resolution
shipped in `77092ea`/`ce72d10`; the alignment baseline is fixed).

#### Phase 1: `Layout` enum + `Repo.layout`
**Model:** sonnet
- Add `Layout` enum and `layout` field; set it in `from_container`/`new`/`from_slug`.
- Extend `tests/bare_layout_test.rs`: a discovered container asserts `Bare`, a
  flat repo asserts `Flat`.
- **Success criteria:** `from_container(..).layout == Bare`, `new(..).layout == Flat`,
  `from_slug(..).layout == Unknown`; discovery of a mixed fixture yields `{Flat, Bare}`;
  `otto ci` green.

#### Phase 2: Three-state rendering (status only) + starship palette
**Model:** sonnet
- Add pure `classify_view` + `LayoutView`; add the `layout_view()` trait method
  (default `None`, `RepoStatus` override); add the Catppuccin color consts.
- In `display_unified_format`, branch on `layout_view()`: `None` -> existing path
  unchanged; `Some(view)` -> branch suppression for matched + `≡`/`≈` + leaf
  gluing + palette. `display_review_result` and the checkout/create paths are not
  modified.
- Unit-test `classify_view` for all three states, detached `HEAD@<sha>`, and
  non-UTF-8 leaf. This is the bite target.
- Add render tests for status rows in both `use_colors` modes; document the
  scheme in `README` and the annotated `gx.yml`.
- **Success criteria:**
  - `classify_view(Bare, Some("main"), Some("main")) == WorktreeMatched{"main"}`;
    `classify_view(Bare, Some("main"), Some("HEAD@abc")) == WorktreeDiverged{"main"}`;
    `classify_view(Bare, None, _) == Flat`; `classify_view(Flat, .., ..) == Flat`.
    Altering any arm fails the test (bites).
  - A status matched-row render contains `≡` + leaf and an empty branch field; a
    diverged-row contains `≈` + leaf + the branch (incl. `HEAD@<sha>` when
    detached); a flat-row contains neither glyph.
  - With `use_colors=false`, status rows emit `slug≡leaf`/`slug≈leaf` with zero
    ANSI; with colors, slug + branch are bold and carry RGB `203,166,247` /
    `166,227,161`, the connector/leaf are non-bold and carry the `≡` green /
    dull-mauve / `≈` peach RGBs.
  - A `CheckoutResult`/`CreateResult`/`ReviewResult` render is byte-identical to
    pre-change output (`layout_view() == None`).
  - `otto ci` green.

## Acceptance Criteria

- [ ] The three constructors set `Bare`/`Flat`/`Unknown` respectively (unit).
- [ ] Discovering a directory with one flat repo + one bare container yields two
      `Repo`s with layouts `{Flat, Bare}` (`tests/bare_layout_test.rs`).
- [ ] `classify_view` returns `WorktreeMatched` iff `Bare && leaf == branch`,
      `WorktreeDiverged` for `Bare && leaf != branch` (incl. detached
      `HEAD@<sha>`) and `Bare && leaf None`->`Flat`, `Flat` otherwise; altering an
      arm fails the unit test.
- [ ] In `gx status`: matched row -> `slug≡leaf` + blank branch; diverged row ->
      `slug≈leaf` + branch; flat row -> plain slug + branch. `gx checkout`/`create`/
      `review` output is byte-identical to before (render tests).
- [ ] `use_colors=false` status rows emit `slug≡leaf`/`slug≈leaf` with zero ANSI;
      `use_colors=true` applies the exact bold/non-bold + RGB roles above.

## Resolved Decisions

- **2026-07-04 - Rendering language = Scott's starship prompt.** Adopt the
  `≡`/`≈` connector glyphs and Catppuccin Mocha palette verbatim. Text tags and
  status emoji were explicitly rejected by Scott.
- **2026-07-04 - Scope = `gx status` only.** Traceable to Scott's request. The
  earlier "all repo-line verbs, rides for free" decision was WITHDRAWN after the
  review panel confirmed `get_branch()` returns a `change_id` for create/review
  (`src/output.rs:407,475,543,611`) - the marker does not ride for free there.
  The `layout_view()` default-`None` seam keeps other verbs unchanged and lets a
  future doc opt one in with a real current-branch source.
- **2026-07-04 - Branch column blank in the matched case.** Mirrors the prompt
  (`custom.branch` suppresses itself when leaf == branch).
- **2026-07-04 - Always-on, no toggle.** Layout is ground truth, not a behavior
  mode; the prompt has no toggle. (taste.md: config drives behavior, not
  `enabled:` flags.)
- **2026-07-04 - Detached HEAD is `HEAD@<sha>`, not `None`** (`src/git.rs:190`).
  Classified `WorktreeDiverged`; `HEAD@<sha>` shown in the branch column.
- **2026-07-04 - `Layout::Bare` kept single-variant** (not split into
  container/worktree). gx only ever emits the worktree; a second, never-
  constructed variant would be dead code. A doc comment states `path` is the
  worktree.
- **2026-07-04 - Alignment prerequisite shipped.** VS16 arrows (`⬆️`/`⬇️`
  measured 2, rendered 1) -> width-1 `↑`/`↓`; `otto ci` green. `≡`/`≈` sit in the
  last column, so alignment is unaffected regardless of their measured width.

## Alternatives Considered

### Alternative 1: Emoji / text-tag marker
Scott rejected both. Emoji reopened the VS16 width problem; text tags are noise.

### Alternative 2: Marker on all repo-line verbs via the shared formatter
Rejected: `get_branch()` is overloaded and returns a `change_id` for
create/review, so the classifier would compare the leaf against a `GX-*` id and
could blank a non-branch column (review-panel blocker, verified
`src/output.rs:407,475,543,611`). The `layout_view()` seam leaves them opt-out by
default and extensible later.

### Alternative 3: Two-state (flat vs bare) only
Rejected: misses state 3 (folder != branch), the one worth flagging.

### Alternative 4: Re-detect layout downstream from the path
Rejected: for a container `path` is the default worktree, not the root;
re-detection means a probe per row and two code paths deciding one fact (drift).

## Technical Considerations

- **Dependencies:** internal only; `colored` supplies `.truecolor`. No new crates.
- **Performance:** zero new subprocess calls; `classify_view` is a match.
- **Security:** none (display-only).
- **Testing:** unit (constructors; `classify_view` incl. detached/non-UTF-8),
  integration (`bare_layout_test.rs` mixed fixture), render (status rows in both
  color modes; a create/review byte-identical check; classifier bite check).
- **Rollout:** ship with the alignment fix in the next patch release; additive
  appearance change; `--help`/README/`gx.yml` updated in Phase 2. Install
  (`cargo install --path .`) and eyeball a real mixed fleet before calling it done.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `≡`/`≈` render wide in some terminal | Low | Low | Cosmetic only - last (left-aligned) column can't misalign others; proven single-width in Scott's terminals via the prompt |
| Palette on status only looks inconsistent vs other verbs | Low | Low | Intentional scope; the `layout_view()` seam makes uniform adoption a one-line-per-verb follow-up if Scott wants it |
| Coloring the branch breaks its right-justify width math | Low | Med | Width computed on the plain string (`branch.len()`), as today; render test asserts alignment holds |
| Detached-HEAD row misreads | Low | Low | `HEAD@<sha>` -> `WorktreeDiverged`, branch shown; unit-tested against the real `git.rs:190` value |

## Open Questions

- [ ] None. All review-panel findings folded; rendering, scope, colors,
      branch-suppression, detached-HEAD, and non-UTF-8 handling are settled.

## References
- Scott's starship config: `~/.config/starship.toml` (`custom.path`,
  `custom.branch`) - the authoritative rendering + color spec.
- Bare-container support: commits `77092ea`, `ce72d10`.
- `~/repos/.claude/rules/taste.md`, `~/repos/.claude/refs/design-exemplars.md`
  (#7 derived fields, #18 gx bare "infection").
- Memory: `[[clone-flat-default-decision]]`.
```
