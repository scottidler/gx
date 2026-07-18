# Implementation Notes: gx lib decomposition (Track B0)

Running, append-only record for `2026-07-17-gx-lib-decomposition.md`.
Per `/how-to-execute-a-plan`.

## Phase 0: call-graph analysis + Track-A preflight (zero-commit spike)

### Track-A preflight (gate) — PASS
`members = []` (gx-mcp gone), `src/mcp.rs` + `src/mcp/` exist, MCP deps
(mcp-io/rmcp/tokio/schemars) + `pub mod mcp;` in the gx lib, no `gx-mcp` binary
target. B0 is cleared to start.

### Authoritative git.rs LOCAL/REMOTE table (supersedes the doc's provisional lists)

**REMOTE** (transitively runs a network verb or calls ssh/github/persona — stays in `remote::git`):
- `get_repo_status_with_options` (:94) — `get_remote_status_with_fetch` → `git fetch` when `fetch_first`
- `get_remote_status_with_fetch` (:405) — `git fetch --quiet`
- `checkout_branch` (:434) — `git pull --ff-only`
- `clone_or_update_repo` (:655) — → `clone_repo` / `update_existing_repo`
- `clone_repo` (:725) — `ssh::...test_github_ssh_connection` + `git clone`
- `update_existing_repo` (:840) — `github::get_default_branch` + `git fetch origin` + `git pull --ff-only`
- `branch_merged_into_base` (:1083) — `fetch_origin` + `get_head_branch`
- `push_branch` (:1262) — `ssh::...get_ssh_command` + `git push --set-upstream`
- `pull_latest` (:1408) — `git pull`
- `clone_repository` (:1425) — `git clone`
- `get_head_branch` (:1458) — local `symbolic-ref` primary, but `ls-remote` fallback via `branch_exists_remotely`
- `branch_exists_remotely` (:1488) — `git ls-remote --heads origin`
- `remote_branch_exists_probe` (:1511) — `git ls-remote --exit-code`
- `delete_remote_branch` (:1544) — probe (ls-remote) + `git push origin --delete`
- `fetch_origin` (:1618) — `ssh::...get_ssh_command` + `git fetch origin`
- `pull_latest_changes` (:1724) — `git pull --ff-only`

The only `ssh::`/`github::` call sites are `clone_repo`, `update_existing_repo`, `push_branch`, `fetch_origin`. No `persona::` reference exists in git.rs.

**LOCAL** (local repo/worktree/index/config only, zero network — → `local::git`):
`StatusChanges::is_empty` (:83), `get_current_commit_sha` (:134), `get_current_branch` (:156),
`get_detached_head_info` (:182), `parse_porcelain_status` (:210), `run_status_porcelain` (:243),
`get_status_changes` (:265), `parse_branch_tracking_info` (:272), `get_remote_status_native` (:333,
`git status --porcelain --branch`, reads LOCAL tracking ref), `resolve_branch_name` (:564),
`get_default_branch_local` (:573), `resolve_update_work_tree` (:832), `get_remote_origin` (:951,
`git remote get-url`, local config), `is_same_repo` (:974), `get_status_changes_for_path` (:990),
`switch_branch` (:1041), `branch_changes_in_base` (:1135, `git cherry`), `delete_local_branch` (:1173),
`add_files` (:1208), `commit_changes` (:1241), `has_uncommitted_changes` (:1299),
`get_current_branch_name` (:1317), `branch_exists_locally` (:1342), `commit_parent_count` (:1585),
`create_branch_at` (:1645), `revert_commit` (:1686), `get_head_sha` (:1746), `stash_save_with_untracked`
(:1769), `stash_sha_by_message` (:1816), `stash_apply_sha` (:1851), `stash_drop_by_sha` (:1881),
`reset_hard_to_sha` (:1949), `force_switch_branch` (:1975), `bytes_to_path` (:2003), `list_index_files`
(:2022), `worktree_add_detached` (:2062), `worktree_remove` (:2096), `stage_all` (:2126),
`resolve_worktree_repo` (:2154), `diff_cached_patch` (:2193), `diff_cached_raw_z` (:2221).

Types → `local::git`: `RepoStatus`, `RemoteStatus`, `StatusChanges`, `BranchTrackingInfo`.

### CORRECTIONS vs the design doc (3 fns the doc mislabeled remote; they are LOCAL)
1. **`create_branch` (:997)** — LOCAL. Callees are `branch_exists_locally`/`switch_branch`/
   `branch_exists_on_remote`/`checkout_remote_branch` + `git checkout -b`; no network, no ssh/github.
2. **`branch_exists_on_remote` (:1359)** — LOCAL. `git rev-parse --verify refs/remotes/origin/<b>`
   reads the LOCAL tracking ref (contrast `branch_exists_remotely` :1488 which really does `ls-remote`).
3. **`checkout_remote_branch` (:1376)** — LOCAL. `git checkout -b <b> origin/<b>` from an already-present
   tracking ref, no network.
   These three are the `create` command's branch-setup path; they are credential-free and go to `local::git`.

### `get_repo_status_local` (new, LOCAL, zero-fetch)
Reuses existing local helpers verbatim (no new git commands):
`branch = get_current_branch`, `commit_sha = get_current_commit_sha`,
`remote_status = get_remote_status_native` (zero-fetch, local tracking ref),
`is_clean = get_status_changes(..).is_empty()`. Returns `RepoStatus`. Its call graph reaches
`get_remote_status_native` but NEVER `get_remote_status_with_fetch`/`fetch_origin` — the Phase 2
biting grep over `local/src` enforces this structurally.

### Straddling helpers — NO duplication needed
Because every shared helper is LOCAL and lands in `local::git`, and `remote` depends on `local`, every
remote-side caller reaches them through the crate dependency. Nothing needs to move to
`subprocess`/`utils` and nothing is duplicated. (`run_checked`/`subprocess_timeout` are already in
`subprocess`, which is a Phase-1 `local` module, so both git halves import them cleanly.)

### file.rs → local: confirmed safe
`file.rs`'s only `git::` use is `git::list_index_files` (LOCAL); its other dep is `crate::diff`
(also → local). No ssh/github/persona in file.rs.

## Phase 1: create local, move the credential-free modules

### Design decisions
- New `local` lib crate at `local/`, added to `[workspace] members` (root `Cargo.toml`); `local/Cargo.toml`
  is `edition = "2021"` (matches gx; edition 2024 would force `unsafe` around the `env::set_var`/`remove_var`
  calls in `config/tests.rs` and `test_utils.rs`, per the rust.md platform-path test pattern).
- `git mv` (not rewrite) for the 9 modules, so history follows: `config.rs`+`config/`, `repo.rs`,
  `subprocess.rs`+`subprocess/`, `hash.rs`+`hash/`, `utils.rs`, `bare.rs`+`bare/`, `diff.rs`, `user_org.rs`,
  `test_utils.rs` all moved into `local/src/`.
- Internal `crate::` references between the 9 moved modules were left untouched (they resolve inside `local`
  now); only the 43 staying `gx` source files (plus 9 integration-test files under `tests/`) had their
  `crate::<name>`/`gx::<name>` paths mechanically rewritten to `local::<name>` via a word-boundary `sed` pass.
  Four files used brace-list imports (`use crate::{git, output, repo};` etc, `src/checkout.rs`, `src/status.rs`,
  `src/clone.rs`, `src/create/core/propose.rs`) that the regex could not split; those were hand-edited to pull
  the moved name into its own `use local::<name>;` line, then `cargo fmt` re-sorted imports.
- `local`'s own `[dependencies]` list was built from what the 9 modules actually import: `chrono`, `colored`,
  `dirs`, `eyre`, `glob`, `log`, `num_cpus`, `rayon`, `regex`, `serde`, `serde_json`, `serde_yaml`, `similar`,
  `unicode-display-width`, `walkdir`, matching gx's existing versions. `gx`'s own `[dependencies]`/
  `[dev-dependencies]` gained `local = { path = "local" }` and `local = { path = "local", features =
  ["testutil"] }` respectively; gx's other now-unused-by-the-moved-code deps (`tempfile`, etc.) were left in
  place because gx's *staying* modules still use them directly.
- `src/lib.rs` and `src/main.rs` had their 9 `pub mod`/`mod` declarations for the moved names removed; the
  bin's `mod config`/`mod repo`/etc. duplicate compilation units are gone, so `main.rs` now imports
  `local::config::{xdg_data_dir, Config}` and calls `local::subprocess::init_subprocess_timeout(..)` directly.

### Deviations
- **test_utils feature-gating (from the design doc's `#[cfg(test)] pub mod test_utils`).** The doc's Phase 1
  bullet says `test_utils` moves in behind `#[cfg(test)]`, but `#[cfg(test)]` items never cross a crate
  boundary: gx's own tests (unit tests in `src/**` and integration tests in `tests/**`, roughly 30 call sites)
  need `local::test_utils::{run_git_command, create_test_repo, env_lock, ...}` from OUTSIDE the `local` crate,
  where `cfg(test)` is false. Implemented instead as `#[cfg(any(test, feature = "testutil"))] pub mod
  test_utils;` in `local/src/lib.rs`, with `local/Cargo.toml` declaring `tempfile` as an optional dependency
  gated by a `testutil` feature (plus a plain `tempfile` dev-dependency so `local`'s OWN `cargo test` builds
  `test_utils` under `cfg(test)` without needing the feature). `gx`'s `[dev-dependencies]` enables
  `features = ["testutil"]` so both gx's unit tests and its `tests/*.rs` integration tests can reach it. Same
  effect (test-only visibility, zero production footprint) at the correct seam for a multi-crate workspace.
  The two inner `#[cfg(test)]` attributes on `ENV_LOCK`/`env_lock` inside `test_utils.rs` were removed (the
  module-level gate already covers them; a leftover inner `cfg(test)` would have made those two items
  invisible under `--features testutil` outside test mode, defeating the point of the feature gate).
- **`Config::load`'s config-path calculation used `env!("CARGO_PKG_NAME")`, which silently changed meaning
  when the code moved crates.** This is a real, otto-ci-invisible behavior change the "no behavior change"
  acceptance criterion demands catching: before the move, `env!("CARGO_PKG_NAME")` compiled inside the `gx`
  package and evaluated to `"gx"`, so `Config::load(None)` read `$XDG_CONFIG_HOME/gx/gx.yml`. After the move
  it compiles inside the `local` package and evaluates to `"local"`, silently redirecting config lookup to
  `$XDG_CONFIG_HOME/local/local.yml` -- a path nothing ever writes to. `otto ci`'s pre-existing test suite
  did not catch this because `config/tests.rs`'s own `test_load_at_default_location_fails_loudly_on_typo`
  used the *same* `env!("CARGO_PKG_NAME")` expression for the fixture path, so production code and test
  stayed self-consistent while both silently drifted off the real product path. It surfaced instead as a
  live regression in `tests/e2e_campaign_test.rs` (the scripted MCP client's `create-propose` step started
  refusing with "tool not found": the test's config file at `gx.yml` was no longer found, so `Config`
  defaulted, and default `mcp.tools` are all disabled). Fixed by introducing a `const GX_PROJECT_NAME: &str
  = "gx"` in `local/src/config.rs` with a doc comment explaining why it is a fixed literal and not
  `env!("CARGO_PKG_NAME")` (the config path is a product-level contract users already have
  `~/.config/gx/gx.yml` on disk, not a crate-level one), and updating the test fixture to write to a literal
  `gx/gx.yml` path instead of re-deriving it from the package name. Verified: `cargo test -p gx --test
  e2e_campaign_test` failed before this fix and passes after it; full `otto ci` is green with this fix in
  place.
- 49 files matched the `crate::(config|repo|subprocess|hash|utils|bare|diff|user_org|test_utils)` grep at
  the start of Phase 1, not the doc's estimated 42; the extra count is because 8 of the 9 moved files
  themselves (git.rs's siblings aside) also matched during the initial pre-move scan (they reference each
  other via `crate::`), and those references correctly stayed as `crate::` once inside `local` (no rewrite
  needed for them). After the move, 43 staying `src/**/*.rs` files plus 9 `tests/**/*.rs` integration-test
  files needed the `local::` rewrite.

### Tradeoffs
- `tempfile` is declared BOTH as an optional dependency (for the `testutil` feature, consumed by `gx`'s
  dev-dependency) AND as a plain dev-dependency (so `local`'s own `cargo test` doesn't need
  `--features testutil` to compile its 56 unit tests) in `local/Cargo.toml`. This double-listing is the
  standard Cargo idiom for "feature-gated in normal builds, always-on in test builds" and was chosen over
  forcing every `cargo test -p local` invocation to remember `--features testutil`.
- Kept the design doc's proposed dependency list as a starting point but verified it by compiling rather
  than trusting it verbatim (the doc listed `unicode-display-width`, `chrono` with `clock,serde`, etc.,
  which all turned out correct; no deps were added beyond what the compiler required).

### Open questions
- None. All ambiguities (test_utils cfg-gating, the `CARGO_PKG_NAME` seam bug) were resolved during this
  phase per the "open questions are the author's to close" rule; nothing here needs Scott's confirmation
  before Phase 2 starts.
