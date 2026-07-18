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

## Phase 2: split git.rs into local + remote

### Design decisions
- **The split follows the Phase-0 authoritative table verbatim.** `src/git.rs` (2592 lines) was
  `git mv`'d to `local/src/git.rs` (history-preserving on the larger LOCAL half) and rewritten to contain
  ONLY the 44 LOCAL functions/helpers + the 4 status types (`RepoStatus`/`StatusChanges`/`RemoteStatus`/
  `BranchTrackingInfo`) + the NEW `get_repo_status_local`. A fresh `src/git.rs` (gx) holds the 16 REMOTE
  functions + the 4 remote result types (`CheckoutResult`/`CheckoutAction`/`CloneResult`/`CloneAction`),
  importing the LOCAL helpers/types it needs from `local::git`. `pub mod git;` added to `local/src/lib.rs`.
- **`get_repo_status_local` (`local/src/git.rs`)** built exactly to the Phase-0 sketch: `get_current_branch`
  + `get_current_commit_sha` + `get_remote_status_native` (zero-fetch, local tracking ref) +
  `get_status_changes(..).is_empty()` -> `RepoStatus`. Its call graph never reaches
  `get_remote_status_with_fetch`/`fetch_origin`; the boundary grep enforces this structurally. It is unused
  by production code today (it is B1's entry point) but is legitimate public API of a lib crate, so
  `dead_code` does not fire.
- **`file` moved into `local`** (`git mv src/file.rs local/src/file.rs` + `src/file/` -> `local/src/file/`);
  `pub mod file;` added to `local/src/lib.rs`. Inside `local`, `file.rs`'s `use local::diff;` became
  `use crate::diff;` and its `use crate::git;` (for `git::list_index_files`, a LOCAL fn) resolves cleanly to
  the new `local::git`; `file/tests.rs`'s `local::{diff,test_utils}::` became `crate::{diff,test_utils}::`.
- **Boundary guard `bin/check-local-boundary.sh`** greps `local/src/**/*.rs` and exits non-zero on
  (a) `Command::new("gh")` or a `\b(ssh|github|persona)::` path (all files), and (b) a quoted network verb
  `"fetch"|"pull"|"ls-remote"|"clone"|"push"`. Wired into `.otto.yml` as task `local-boundary` in `ci`'s
  `before:` list, so it runs on every `otto ci`. Design: `cargo tree` misses source-level shell-outs, so the
  guard is a source grep per the doc's Resolved Decision on the two-part boundary.
- **Function visibility:** nine helpers that were private in the monolith and are now called by the REMOTE
  half across the crate boundary were promoted to `pub` in `local::git`: `get_current_branch`,
  `get_current_commit_sha`, `get_remote_status_native`, `get_status_changes`, `get_status_changes_for_path`,
  `get_remote_origin`, `is_same_repo`, `resolve_update_work_tree`, and `branch_changes_in_base`. Helpers used
  only within `local` stayed private (`get_detached_head_info`, `run_status_porcelain`,
  `parse_branch_tracking_info`, `bytes_to_path`).

### Deviations
- **`branch_changes_in_base` promoted from private `fn` to `pub fn` (same seam, wider visibility).** It is a
  purely local `git cherry` patch-identity proof (LOCAL per the Phase-0 corrections), but its only caller,
  the REMOTE `branch_merged_into_base` (which fetches `origin` first), now lives in a different crate. Making
  it `pub` in `local::git` is the minimal change that keeps the local primitive local while letting the
  fetch-owning remote caller reach it. Its fail-closed test moved to `local::git`'s test module unchanged.
- **Guard excludes `test_utils.rs` from the network-verb check.** `local/src/test_utils.rs`
  (`create_bare_container`) runs a LOCAL `git clone --bare <temp-source> <temp-bare>` from a temp path -- no
  network -- across a multi-line args array, so a line-level exclusion could not target it the way the
  `stash push` line is excluded by matching `stash`. `test_utils` is test-only scaffolding gated behind
  `cfg(any(test, feature = "testutil"))` and is never part of the credential-free runtime surface B1 depends
  on, so excluding it from the verb check does not weaken the production boundary. The credential-import
  check (ssh/github/persona/gh) still applies to `test_utils.rs`.
- **Rewiring style is mixed by necessity, not uniform.** Files whose git references are ALL local
  (`create/core/propose.rs`) just repointed `use crate::git;` -> `use local::git;`. Files with BOTH halves
  (`checkout`, `cleanup`, `create/core`, `status`, `transaction`, `undo/core`, `output`, plus their test
  modules and the `tests/*` integration tests) kept `use crate::git;` for the remote refs and
  fully-qualified each LOCAL ref to `local::git::<name>` (types `RepoStatus`/`RemoteStatus`/`StatusChanges`
  -> `local::git`, remote result types stay `crate::git`). `file` importers (`create`, `create/core`,
  `create/core/manifest`, `state`, `transaction`, and test modules) repointed to `local::file`. 23 importer
  files rewired; the compiler + `-D warnings` (unused-import) confirmed no stale `use crate::git;` remained.

### Bite proof (required)
- Planted `let _planted = std::process::Command::new("git").args(["fetch", "origin"]);` at the top of
  `local::git::get_repo_status_local` (a production local module). `bash bin/check-local-boundary.sh` printed
  `BOUNDARY VIOLATION -- remote git network verb in local/src: .../local/src/git.rs:68: ... ["fetch",
  "origin"]` and exited **1** (RED). Reverted the line; the guard returned to `local/src is clean` and exit
  **0**. The `local-boundary` task sits in `ci.before`, so this same RED would fail `otto ci`.

### Tradeoffs
- **`tempfile` is now a plain `[dependencies]` entry of `local`, not optional.** `file::atomic_write` uses
  `tempfile::NamedTempFile` in PRODUCTION, so once `file` moved into `local`, `tempfile` had to be a normal
  dependency. `local/Cargo.toml` dropped the `optional = true` marker and the redundant `[dev-dependencies]
  tempfile` line, and simplified `[features]` to `testutil = []` (test_utils stays gated by
  `#[cfg(any(test, feature = "testutil"))]` but no longer needs `dep:tempfile`, since tempfile is always
  present now). Chosen over keeping the optional/dev double-listing, which no longer models reality.
- **Kept the git.rs inline `#[cfg(test)] mod tests` style in both halves** rather than extracting to
  `git/tests.rs` per the rust.md test-placement rule. This is a pure move refactor with a "no behavior
  change / no test changes except paths" acceptance criterion; extracting the test module is an orthogonal
  cleanup that would enlarge the diff and risk the split. Left as-is deliberately.

### Success criteria
1. `local::git` compiles credential-free (no `ssh`/`github`/`persona`, no network verb) -- PASS (guard clean
   + workspace builds).
2. Boundary guard BITES -- PASS (planted `git fetch` -> guard exit 1, reverted, proof above).
3. `otto ci` GREEN, all existing git tests pass -- PASS (`otto ci` exit 0; 21 `git::tests::*` cases run
   green under `local`, remote git tests green under `gx`).
4. `file` lives in `local`; `local::git::get_repo_status_local` exists and is zero-fetch -- PASS.

### Open questions
- None. The three cross-repo/deferred bullets in this design (`remote` crate formation, bin shim, status flip
  to Implemented) belong to Phase 3 / the parent, not this phase.

## Phase 3: form remote, finalize the bin

### Design decisions
- **Repo layout note.** The design doc's steps refer to `gx/Cargo.toml` and `gx/src/`, but in this repo the
  `gx` package IS the workspace root (root `Cargo.toml` + root `src/`); there is no `gx/` subdirectory holding
  the bin (a `gx/` dir exists but only holds an unrelated legacy doc, `gx/docs/remote-status-feature.md`).
  Read the doc's intent (move the 21 lib modules out of the `gx` package into a new `remote` crate, thin the
  `gx` bin) and executed it at the correct seam: `remote/` created as a new top-level workspace member
  alongside `local/`, the 21 modules `git mv`'d from root `src/` into `remote/src/`, root `src/lib.rs` deleted
  (gx has no lib now), root `src/main.rs` thinned.
- **All 21 modules moved via `git mv`** (files + subdirs): `checkout`, `cleanup`, `cli`, `clone`, `confirm`,
  `crash`, `create`, `doctor`, `git` (the REMOTE half, already split out in Phase 2), `github`, `lock`, `mcp`,
  `output`, `persona`, `review`, `rollback`, `ssh`, `state`, `status`, `transaction`, `undo` -> `remote/src/`.
  Internal `crate::` references between these modules were left untouched (they resolve inside `remote` now,
  same as Phase 1's treatment of `local`'s internal refs).
- **`run_application` moved into `remote::app`** (a new `remote/src/app.rs`, `pub mod app;` in
  `remote/src/lib.rs`), per the design doc's stated preference for a genuinely thin bin over leaving the full
  command-dispatch match in `main.rs` with `remote::` prefixes sprinkled through it. `main.rs` now does: parse
  args (`remote::cli::{Cli, Commands}` + `clap`'s `CommandFactory`/`FromArgMatches` traits), `--cwd` chdir, the
  `mcp` interception (`local::config::Config::load` + `mcp_io::mcp_io!()` + `remote::mcp::server::GxMcpServer`),
  `setup_logging`/`install_panic_hook` (kept in `main.rs` - they are bin-lifecycle concerns, not command
  dispatch), `Config::load` + `local::subprocess::init_subprocess_timeout`, then one call:
  `remote::app::run_application(&cli, &config)`. `main.rs` dropped from 343 lines (21 `mod` decls + the full
  match) to a genuinely thin shell; `run_application`'s body is otherwise byte-identical to before the move
  (only `crate::` -> local `crate::` refs inside `remote::app`, since it now lives in `remote` and calls
  sibling `remote` modules directly).
- **`build.rs` (`GIT_DESCRIBE` for clap's `version = env!("GIT_DESCRIBE")`) moved with `cli.rs`.** `cargo:rustc-
  env` only applies to the package whose build script sets it, not workspace-wide, so once `cli.rs` (the only
  consumer of `env!("GIT_DESCRIBE")`) moved into `remote`, the build script had to move there too:
  `git mv build.rs remote/build.rs`, `build = "build.rs"` added to `remote/Cargo.toml`. Its
  `cargo:rerun-if-changed` paths were relative to the OLD manifest dir (repo root); rewritten to `../.git/HEAD`,
  `../.git/refs/`, `../.git/packed-refs` since they are now resolved relative to `remote/`'s manifest dir. The
  companion regression test (`tests/build_script_test.rs`, which reads `build.rs` via
  `concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs")`) moved with it to `remote/tests/build_script_test.rs` (its
  `CARGO_MANIFEST_DIR` is now `remote/`, matching where `build.rs` lives), and its three string assertions were
  updated to expect the `../.git/...` paths.
- **gx-tests `gx::` -> `remote::` rewire.** Five integration-test files under (root) `tests/` imported the
  former `gx` lib directly: `alignment_test.rs`, `lock_contention_test.rs`, `unified_formatting_tests.rs`,
  `mcp_tools_test.rs`, `ssh_integration_tests.rs`. Since `gx` has no `[lib]` anymore, every `gx::` path became
  `remote::` (mechanical `sed -i 's/\bgx::/remote::/g'`); `cargo fmt` re-wrapped one resulting long line in
  `unified_formatting_tests.rs`. The binary-spawning tests (`mcp_tools_test.rs`, `mcp_handshake_test.rs`, etc.)
  kept `env!("CARGO_BIN_EXE_gx")` unchanged - the compiled bin's name and behavior are untouched.
- **`gx`'s `[dev-dependencies]` gained `serde_json`, `serde_yaml`, `tempfile`, `remote = { path = "remote" }`.**
  Before Phase 3 these were `gx`'s own `[dependencies]` (the lib used them); once the lib's code moved to
  `remote`, the root package's ONLY remaining consumers of these three crates are its `tests/*.rs` integration
  tests, so they moved to `[dev-dependencies]`. `remote = { path = "remote" }` (no feature flags - see
  Tradeoffs) lets those same tests reach `remote::create::manifest`, `remote::state`, `remote::output`, etc.
- **Root `Cargo.toml` dependency list reduced to what `main.rs` itself imports directly:** `clap` (trait
  methods on the derived `Cli`), `env_logger` + `log` (bin-owned logging setup), `eyre` (error handling),
  `local` (`Config`, `xdg_data_dir`, `subprocess::init_subprocess_timeout`), `mcp-io` (the `mcp_io!()` macro),
  `remote` (everything else). Every other former root dependency (`chrono`, `colored`, `dirs`, `glob`,
  `num_cpus`, `rayon`, `regex`, `rmcp`, `schemars`, `serde`, `serde_json`, `serde_yaml`, `similar`, `tempfile`,
  `tokio`, `unicode-display-width`, `walkdir`) is now exclusively consumed by the moved modules and lives in
  `remote/Cargo.toml` instead (built by compiling and letting the compiler name every missing one, then
  `cargo add`-equivalent version-matching against the prior root `Cargo.toml`).
- **`similar` dropped entirely from the workspace's non-`local` crates.** It was a root dependency before this
  phase but no moved module (`remote/src/**`) references it directly; grepping confirmed zero remaining
  `similar::` call sites outside `local` (which already depends on it from Phase 1). Not re-added to `remote`.

### Deviations
- **`tempfile` is a normal (non-optional) `[dependencies]` entry in `remote/Cargo.toml`, not dev-only.**
  `remote/src/create/core/propose.rs`'s `tempfile::Builder::new().prefix("wt-").tempdir_in(tmp_root)` is
  PRODUCTION code (the propose worktree's temp checkout dir), not test scaffolding - every other `tempfile::`
  call site in the moved modules is inside a `#[cfg(test)] mod tests` (git.rs, cleanup.rs, github.rs, lock.rs,
  state.rs, transaction.rs, and several `create`/`rollback`/`undo` test submodules), but this one production
  call site forces `tempfile` to be a plain dependency of `remote`, matching how `local/Cargo.toml` already
  handles it after Phase 2 (`file::atomic_write`'s analogous production use).
- **Declared, then removed, an unused `testutil` feature on `remote`.** The design doc's step 1 said to add
  `[features] testutil = []` "if any remote test helper must cross to the gx bin's tests." Checked: `remote`
  has no `#[cfg(test)]`-gated pub exports that gx's `tests/*.rs` need (unlike `local::test_utils`, everything
  the gx integration tests import from `remote` - `create::manifest`, `state`, `output`, `ssh`, `git::
  {CheckoutAction, CheckoutResult}`, `transaction` - is ordinary production `pub`). Declared the feature and
  the corresponding `features = ["testutil"]` on both dependency edges first, then removed both once the build
  proved they gated nothing, per "config drives behavior or it doesn't exist" - an inert feature flag is dead
  configuration.
- **`main.rs`'s `install_panic_hook`/`setup_logging` stayed in the bin rather than moving to `remote::app`.**
  The design doc's Phase 3 bullet lists `output`/`cli`/`mcp` etc. as moving, and separately says "keep
  `setup_logging`/`install_panic_hook` + mcp interception" in the bin if `run_application` moves - matching
  that explicit guidance; these two functions own the bin's own lifecycle (process-wide logger init, panic
  hook), not command dispatch, so they are correctly bin-owned, not `remote`-owned.

### Tradeoffs
- **`remote = { path = "remote" }` in `gx`'s `[dev-dependencies]` carries no feature flags,** unlike `local`'s
  `features = ["testutil"]` dev-dependency edge. This is intentional asymmetry, not an oversight: `local`
  genuinely gates test-only code behind a feature (`test_utils`), `remote` does not, so adding an unused
  feature flag to the `remote` edge would be cargo-cult symmetry with no effect. Chosen over "match `local`'s
  shape for consistency" because the shapes are legitimately different for a concrete reason (see Deviations).
- **Cargo.lock churned on this phase** (new `remote` package entry, transitive version bumps picked up by the
  fresh resolve - e.g. `clap` 4.5.47 -> 4.6.2, `tempfile` 3.22 -> 3.27, `serde` 1.0.225 -> 1.0.228 - all within
  each dependency's declared semver range in the various `Cargo.toml`s). Left as the resolver produced it
  rather than pinning back to the exact prior lock, since `otto ci` is green with the new lock and the design
  doc explicitly rules out any external/behavior change, not any exact dependency-version freeze.

### Success criteria
1. Workspace is `local` + `remote` + `gx` (bin), single flat version -- PASS. `cargo tree -p remote --depth 1`
   shows `local` as its only workspace dependency; `cargo tree -p gx --depth 1` shows `local` + `remote`;
   `grep -c "gx v0" <(cargo tree -p remote)` is 0 (no cycle back to `gx`). All three crates use
   `version.workspace = true` against the single `[workspace.package] version = "0.6.3"`.
2. `otto ci` GREEN -- PASS (exit 0; lint, `local-boundary`, check/clippy/fmt, full test suite - 92 `local`
   tests + 252 `remote` tests + the root `gx` integration-test targets - all green).
3. Full `gx --help` command matrix behaves identically -- PASS. Built `./target/debug/gx --help`: lists
   status/checkout/clone/create/apply/review/rollback/undo/cleanup/doctor/mcp verbatim (plus `help`); `gx mcp
   --help` lists serve/register/unregister/status/bundle (plus `help`). No behavior change - `run_application`'s
   body is a verbatim move.
4. `cargo tree -p remote` depends on `local`; `cargo tree -p gx` depends on `remote` -- PASS, shown above.

### Open questions
- None. All ambiguities in this phase (repo-layout mismatch between the doc's `gx/` paths and this repo's
  root-package layout, the `GIT_DESCRIBE` build-script seam, the unused `testutil` feature) were resolved
  during implementation per "open questions are the author's to close." This closes Track B0; Track B1 (the
  intel catalog, `2026-07-17-gx-intel-catalog.md`) can now build its read-only tools against `local` only,
  with the crate boundary enforced by the compiler and the biting CI guard from Phase 2.
