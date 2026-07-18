# Implementation Notes: gx read-only intel catalog

Design doc: `docs/design/2026-07-17-gx-intel-catalog.md`

## Phase 1: catalog schema

### Design decisions
- Created the `catalog` crate now (Phase 1), not Phase 3 as the doc's
  Implementation Plan literally schedules it — `catalog/Cargo.toml`,
  `catalog/src/lib.rs`, `catalog/src/db.rs` — because the schema/migration
  code needed a coherent home from the start rather than living transiently
  in `local` and being moved later. `catalog` depends on `local` ONLY (see
  `catalog/Cargo.toml`); `cargo tree -p catalog` shows no `remote`.
- `catalog::db::catalog_db_path()` (`catalog/src/db.rs`) resolves
  `$XDG_CACHE_HOME/gx/catalog.db`, mirroring the `xdg_data_dir()`/
  `xdg_config_dir()` shape already in `local::config`: the new
  `local::config::xdg_cache_dir()` (`local/src/config.rs`) returns the bare
  cache-home dir (not `gx`-suffixed), and callers append `gx/catalog.db`
  themselves — same convention `main.rs` uses for `xdg_data_dir().join("gx")`.
- `catalog::db::open()` applies `busy_timeout` (named `BUSY_TIMEOUT_MS`
  const), `journal_mode=WAL`, `synchronous=NORMAL`, and `foreign_keys=ON` on
  EVERY connection open (SQLite does not persist `foreign_keys` in the file),
  then runs `migrate()`. `migrate()` short-circuits on `PRAGMA user_version`
  already at `SCHEMA_VERSION`, and otherwise runs the full DDL + version bump
  inside one `BEGIN;...;COMMIT;` batch — matches rust.md's rusqlite house
  rules (busy_timeout const, one-txn migration, idempotent DDL).
- `repos`/`deps` DDL is copied verbatim from the design doc's Data Model SQL
  (`catalog/src/db.rs::migrate`), including both indexes on `deps`.
- `local::config::CatalogConfig` (`local/src/config.rs`) follows the existing
  `Config` idiom exactly: every field `Option<T>`, a hand-written
  `impl Default`, `deny_unknown_fields` + `rename_all = "kebab-case"`, and
  effective-value accessors on `Config` (`catalog_root()`,
  `catalog_staleness_secs()`) rather than reading the raw `Option` fields at
  call sites — consistent with `confirm_threshold()`, `pr_body_template()`,
  etc.
- Added a private `expand_tilde()` helper in `local/src/config.rs` (no
  existing tilde-expansion helper in the codebase before this phase, despite
  `logging.file`'s default containing a literal `~` that is never expanded).
  It expands a bare `~` or `~/...` against `dirs::home_dir()`; a path with no
  leading `~` passes through unchanged; if `$HOME` cannot be resolved the
  literal path is returned rather than fabricating a partially-expanded one.
  Only `catalog_root()` calls it in this phase — `~` expansion for
  `logging.file` was out of scope and untouched.
- `./gx.yml` gained an annotated, commented-out `catalog:` block (default
  behavior needs no config, so it stays commented like `review`/`cleanup`).

### Deviations
- The `catalog` crate exists starting Phase 1, not Phase 3, per the task
  instructions' explicit call-out. Same effect as the doc intends (a
  `local`-only intel crate), just built one phase sooner so the schema has a
  home from day one instead of temporarily living in `local` and moving
  later.
- The design doc's Phase 1 description doesn't literally name a
  `catalog::db` module path; I placed it at `catalog/src/db.rs` (the natural
  single-word module name for "DB open + migrate") rather than inlining it
  into `lib.rs`.

### Tradeoffs
- `migrate()` re-checks `user_version` and short-circuits, AND every
  individual `CREATE TABLE`/`CREATE INDEX` statement is itself `IF NOT
  EXISTS`. Belt-and-suspenders: the version check alone would suffice for
  idempotence, but keeping the DDL idempotent too costs nothing and guards
  against a future migration bump that forgets to raise `SCHEMA_VERSION`.
- `expand_tilde` is a private, hand-rolled helper rather than pulling in a
  crate like `shellexpand`. The expansion surface needed (`~` and `~/...`
  only, no `~user` support) is small enough that a dependency wasn't
  justified; revisit if a second config field needs richer expansion.
- Chose infallible `PathBuf` return from `expand_tilde` (falls back to the
  literal path when `$HOME` is unresolved) rather than `Option`/`Result`,
  since `catalog_root()` needs a concrete `PathBuf` either way and an
  unresolvable `$HOME` already breaks `xdg_data_dir()`/`xdg_config_dir()`
  identically elsewhere in this crate.

### Open questions
- The companion Track A (`2026-07-17-gx-onto-mcp-io.md`) and Track B0
  (`2026-07-17-gx-lib-decomposition.md`) design docs are still untracked in
  git (never committed as files, even though the doc text says B0 has
  "landed" and Track A is a stated dependency). Not this phase's file to fix,
  but worth confirming with Scott whether those docs should be committed
  before Phase 2/3 land, since the intel-catalog doc's "Gated on" line points
  at them.

## Phase 2: walk indexer

### Design decisions
- The walk lives in `catalog/src/walk.rs` (`catalog::walk::walk`), a pure
  `local`-only module: it reuses `local::repo::discover_repos`,
  `local::git::get_repo_status_local` (LOCAL branch/dirty/ahead-behind, never
  a fetch), and `local::subprocess::{run_checked, subprocess_timeout}` for the
  `git log` / `git rev-parse` shell-outs. No `remote` path exists, so the
  zero-network guarantee is compiler-structural. `cargo tree -p catalog`
  shows no `remote`.
- Two-stage pipeline (`walk`): gather read-only `RepoRecord`s in parallel
  (`repos.par_iter().map(build_record)`), then serialize ALL writes through
  the single `&mut Connection` — one `conn.transaction()` per repo, each doing
  the `repos` upsert + `DELETE FROM deps WHERE repo_slug=?` + re-insert. This
  is the Data Model idempotence rule and keeps SQLite single-writer (WAL +
  `BUSY_TIMEOUT_MS` already set in `catalog::db::open`). Every bound value goes
  through `params![]`.
- ahead/behind mapping (`ahead_behind`, walk.rs): `RemoteStatus::UpToDate →
  (0,0)`, `Ahead(a) → (a,0)`, `Behind(b) → (0,b)`, `Diverged(a,b) → (a,b)`;
  every state with NO local tracking ref (`NoUpstream`/`NoRemote`/
  `DetachedHead`/`Error`) → `(NULL, NULL)`, because the columns exist precisely
  to say "no local tracking ref to read." Values come from
  `get_remote_status_native` (reads the LOCAL tracking ref via `git status
  --porcelain --branch`), never a fetch.
- last-commit (`last_commit`, walk.rs): `git log -1 --format=%H%x09%ct` →
  `(full_sha, committer_unix)`; `(None, None)` for an empty repo or any git
  error. last-fetch (`last_fetch_mtime`): resolves the real FETCH_HEAD path via
  `git rev-parse --git-path FETCH_HEAD` (so flat repos, worktrees, and bare
  containers all work), then reads its mtime; `None` when the file is absent
  (never fetched). No fetch is ever issued.
- Manifest parsing (`parse_manifests`/`parse_cargo_deps`/`parse_npm_deps`):
  `Cargo.toml` → ecosystem `cargo`, sections `[dependencies]`/`[dev-*]`/
  `[build-*]` → kinds `normal`/`dev`/`build`; a git/path/workspace dep with no
  `version` stores `version_req = NULL`. `package.json` → ecosystem `npm`,
  `dependencies`/`devDependencies` → `normal`/`dev` (npm has no build kind).
  Language guess prefers `rust`, then `typescript` (a `tsconfig.json` beside
  `package.json`), then `javascript`. Added deps `toml`, `serde_json`, `serde`,
  `rayon` to `catalog/Cargo.toml` via `cargo add`.
- Pruning (`prune_missing`) is SCOPED to the canonical walked `root`
  (`path = :root OR path LIKE :root || '/%'`, the trailing `/%` avoiding the
  sibling-prefix bug) and deletes only slugs not seen this walk. A subtree walk
  therefore never wipes out-of-scope repos. Deps cascade via `ON DELETE
  CASCADE` (foreign_keys pragma set per-connection in Phase 1). Paths are
  stored CANONICAL (`std::fs::canonicalize`, falling back to the raw path with
  a `warn!` if a repo vanished mid-walk).
- CLI: added `Commands::Catalog { fetch: bool }` to `remote::cli`, dispatched
  in `remote::app` to the new `remote/src/catalog.rs`
  (`process_catalog_command`). The handler resolves `config.catalog_root()` +
  effective `max_depth` + `ignore_patterns`, opens `catalog::db::open_default()`,
  runs `catalog::walk::walk`, and prints a one-line summary. Added
  `catalog = { path = "../catalog" }` to `remote`'s deps (remote→catalog→local;
  catalog still has NO remote dep — verified).

### Deviations
- The design's Phase 2 bullet names only the walk; I placed the walk at
  `catalog::walk` (single-word module) rather than inlining it, matching the
  `catalog::db` precedent from Phase 1. Same effect, correct seam.
- `--fetch` in `gx catalog` is a Phase-4 stub: rather than a silent no-op, the
  handler `bail!`s with an explicit "not yet implemented (Phase 4)" error
  (fail-loud, fail-closed per taste.md). Phase 4 replaces that early return
  with the per-repo `git::fetch_origin` + re-walk loop. The `fetch` flag,
  its clap help, and the dispatch arm are all wired so Phase 4 only fills the
  body.

### Tradeoffs
- Parse manifests into `toml::Table` / `serde_json::Value` and walk the tree
  by hand, rather than deriving strict typed manifest structs. A real
  `Cargo.toml`/`package.json` carries many fields the catalog does not care
  about; a `deny_unknown_fields` typed parse would reject perfectly valid
  manifests. The catalog wants a best-effort dep list, so a lenient
  value-walk that skips unknown shapes is the right call here (this is NOT
  config parsing, where strict schema is law).
- `last_walk` is stamped once per walk (a single `now_unix()` shared across all
  repos in the run) so every row from one walk sorts together; the per-record
  placeholder set in `build_record` is overwritten at write time. Costs a
  redundant clock read in the struct; keeps `RepoRecord` self-contained for
  unit tests.
- Deps use `INSERT OR IGNORE` so a manifest naming the same dep twice under one
  kind collapses to the single PK row instead of erroring the whole repo txn.

### Open questions
- None.

### Environment note (not a code issue)
- The shared cargo `target/` (a symlink to an external SSD) held ~45 GiB of
  PRE-DECOMPOSITION incremental artifacts — a stale `gx v0.1.9` LIBRARY crate
  with `src/state.rs` (gx is now a bin-only 0.6.3 crate). Under `otto ci`'s
  parallel `check`+`test`, those stale units resurfaced as a phantom
  `unnecessary_sort_by` clippy error against a source file that no longer
  exists. A full `cargo clean` cleared it; direct per-crate builds never hit it
  because they used up-to-date incremental state. Worth a periodic `cargo clean`
  after big structural refactors (like B0). No production code involved.

## Phase 3: read-only intel tools

### Design decisions
- The four tools live in a new `catalog::tools` module tree
  (`catalog/src/tools.rs` + `tools/{query,search,read,deps}.rs`), depending on
  `local` + `rusqlite` ONLY. The two cross-cutting invariants live ONCE in
  `tools.rs`: `clamp_root` (the scope clamp) and `Bounds`/`bound_items` (the
  output bounds). Every tool calls both, so neither can drift per-tool.
- **Scope clamp** (`tools::clamp_root`): canonicalize the `catalog.root` ceiling
  and the requested `root` (default = caller CWD), then require
  `root == ceiling || root.starts_with(ceiling)`. `starts_with` compares whole
  path COMPONENTS, so `/repos/foobar` is not "inside" `/repos/foo` (sibling-
  prefix closed at the Rust layer). Because `canonicalize` resolves symlinks and
  `..`, both escape classes resolve to a real path outside the ceiling and are
  rejected; a non-existent root fails `canonicalize` outright. All four rejects
  are fail-closed (loud `Err`, never widened/emptied). The SQL layer
  (`tools::scope_sql`) then filters `path = :root OR path LIKE :root || '/%'` --
  the trailing `/%` (not bare `%`) is the second sibling-prefix guard.
- **Output bounds** (`tools::bound_items`): caps BOTH result count and total
  serialized bytes, sets `truncated: true` when either trips, and always keeps
  at least one item if the input is non-empty (a single oversized item returns
  WITH `truncated`, never empty-as-success). Caps are module consts
  (`DEFAULT_MAX_RESULTS = 500`, `DEFAULT_MAX_BYTES = 1 MiB`) -- see Deviations
  re: no config field.
- `query` (`tools/query.rs`): indexed SELECT over `repos`, dynamic `where{}`
  filters bound via `params_from_iter` (`dirty`/`branch`/`org`/`lang`/
  `behind_gt`; `behind > ?` naturally excludes NULL "no tracking ref"), every
  row surfaces `last_walk`/`last_fetch`.
- `search` (`tools/search.rs`): consults the `repos` table ONLY to enumerate the
  in-scope repo paths (longest-path-first so a nested repo wins the prefix
  match), then shells `rg` ONCE over those paths for LIVE content via
  `local::subprocess::run_checked` (wall-clock timeout + process-group kill). rg
  exit 1 (no matches) is success; exit 2 is a loud error; a spawn failure ("rg
  not installed") is a loud error naming the fix, never empty-as-success. Each
  hit's absolute path maps back to its slug via the scoped paths.
- `read` (`tools/read.rs`): looks up the repo dir by slug (must be in the catalog
  AND under `catalog.root`), clamps the file path with
  `local::file::validate_new_file_path` (rejects absolute/`..`/`.git`/symlink
  escapes), refuses an oversized whole-file read loudly unless a bounded line
  range is given, and reads via `local::file::read_utf8_or_skip` -- a non-utf8
  file is a loud "not valid UTF-8" error (`None` never treated as empty success).
- `deps` (`tools/deps.rs`): a `serde`-tagged (`direction`) enum result. Exactly
  one of `dependency` (JOIN repos<-deps, repos-using-it) or `slug` (that repo's
  deps) must be given; both/neither is a loud error. Both directions are scope-
  clamped to `catalog.root` and surface `last_walk`/`last_fetch`.
- MCP wiring: 4 `#[tool]` methods (`query`/`search`/`read`/`deps`) on
  `GxMcpServer` (`remote/src/mcp/server.rs`) call thin blocking bodies in
  `mcp/logic.rs` under `run_blocking`; each opens its own `catalog::db` connection
  and resolves the ceiling from `config.catalog_root()`. Request types added to
  `mcp/schema.rs`. `Query`/`Search`/`Read`/`Deps` added to `McpTool`
  (`local::config`), to `gate::ALL` (now 14) and `gate::name` (kebab-case),
  `is_mutating = false` (read-only, default ENABLED).
- `gx doctor`: added an `rg` presence check (`check_tool_presence` +
  `extract_second_token_version`, since `rg --version` prints the version as the
  SECOND token, unlike git/gh's third). A missing `rg` reports `not found`,
  `ok = false` (fail closed).
- CI guard: new `bin/check-catalog-boundary.sh` + a `catalog-boundary` otto task
  wired into `ci.before`. Fails if `catalog/Cargo.toml` declares a `remote` dep
  (deterministic grep) or if `cargo tree -p catalog` shows a `remote` node
  (best-effort).

### Deviations
- Output caps are module consts, NOT a new `catalog.*` config field. The design's
  `catalog:` block specifies only `root`/`staleness-secs`; adding cap config keys
  would be unrequested scope. Same effect the doc intends (bounded MCP payloads);
  a future tunable defaults to these consts. Recorded so it is not mistaken for
  an oversight.
- The MCP `query` request FLATTENS the doc's `where{ ... }` object to top-level
  optional args (`dirty`/`branch`/`org`/`lang`/`behind_gt`). Same filter
  semantics; keeps the MCP input schema flat and dodges the `where` Rust keyword.
- **Auto-walk-on-stale is NOT implemented in Phase 3.** The doc's Edge Cases
  mention an MCP `query` auto-walking a stale/empty subtree, but the Phase 3
  bullet and its success criteria do not include it -- the phase specifies the 4
  tools over the existing index plus staleness surfacing (`last_walk`/
  `last_fetch`), which is what shipped. `gx catalog` (Phase 2) builds/refreshes
  the index today. Flagged as an open question below rather than silently scoped
  out.
- Placed the tools at `catalog::tools::{query,search,read,deps}` (a module dir)
  rather than a single `tools.rs`, matching the `db`/`walk` precedent. The doc
  said "e.g. `catalog/src/tools.rs` or per-tool modules" -- chose per-tool
  modules. Same effect, correct seam.

### Tradeoffs
- `search` resolves each hit's slug via the `repos` table (to enumerate scoped
  paths and prefix-match), even though the doc says "search needs no table". The
  table is used only to bound the search to scoped paths and to attach slugs to
  hits; file CONTENT is still read LIVE by `rg`, never from the index. The
  alternative (deriving slug from `<org>/<name>` path components) is fragile
  across repo shapes, so the light read wins.
- `deps` returns a `serde`-tagged enum (`ByDependency` | `BySlug`) rather than a
  struct with optional halves, so a consumer can't get a malformed both-null /
  both-populated payload -- the type encodes "exactly one direction".
- `read`'s byte cap runs even on a line-range read: a range whose text still
  exceeds the cap is truncated (`truncated: true`) rather than rejected, since
  the range was the caller's explicit opt-in to a bounded read.

### Open questions
- Auto-walk-on-stale (doc Edge Cases): should an MCP `query`/`search` trigger a
  local walk of the scoped subtree when rows are older than
  `catalog.staleness-secs`, or is the explicit `gx catalog` refresh (Phase 2)
  sufficient? Deferred out of Phase 3 per its success criteria; confirm whether a
  follow-up phase should wire it (the walk already lives in `catalog`, so it is a
  small addition when wanted).

### Bite proofs (break-a-test-to-prove-it)
- **Clamp**: changed `scope_sql`'s prefix from `{root}/%` to `{root}%`
  (reintroducing the sibling-prefix bug) -> `test_query_returns_only_in_scope_rows`
  FAILED ("only the repo under foo, not foobar", left==right: 2 != 1) because the
  sibling `foobar` repo leaked into a `foo`-scoped query. Reverted; test green.
- **Catalog boundary guard**: injected `remote = { path = "../remote" }` under
  `[dependencies]` in `catalog/Cargo.toml` -> `bin/check-catalog-boundary.sh`
  exited 1 ("catalog/Cargo.toml declares a 'remote' dependency"). Reverted;
  guard exits 0 and `cargo tree -p catalog` shows `local` only, no `remote`.

## Phase 4: fetch refresh (+ auto-walk-on-stale)

### Design decisions
- **Real `gx catalog --fetch`** replaces the Phase-2 `bail!` stub. The handler
  (`remote::catalog::process_catalog_command`) now branches on `fetch`; the
  network path is a new testable core, `remote::catalog::fetch_refresh`
  (`remote/src/catalog.rs`). It `discover_repos` under `catalog.root`, calls
  `remote::git::fetch_origin(&repo.path)` per repo (path-only, unchanged; auth
  rides each repo's `origin` URL + `~/.ssh/config`), then does ONE full
  `catalog::walk::walk` of the subtree afterward so every repo's FETCH_HEAD mtime
  (`last_fetch`) and refreshed remote-tracking ref (ahead/behind) land in the
  catalog. Chose the "fetch-all-then-one-walk" shape over per-repo re-walk: same
  effect, one write pass through the single SQLite connection.
- **Fail-loud-skip isolation** (`fetch_refresh`): a per-repo `fetch_origin` error
  is a `warn!` PLUS an `eprintln!` and a `fetch_failed` increment, never a
  `return`/`bail!` -- the loop continues, so one repo's auth/network failure can
  never abort the fleet refresh. `FetchSummary { fetched, fetch_failed, walk }`
  carries the counts so the CLI prints a truthful one-liner.
- **Auto-walk-on-stale** = new `catalog::walk::ensure_fresh(conn, catalog_root,
  requested_root, max_depth, ignore_patterns, staleness_secs) -> Result<bool>`
  (`catalog/src/walk.rs`). It clamps the requested root through the SAME
  `catalog::tools::clamp_root` the read tools use (so the freshness check and the
  query it precedes agree on scope), reads `SELECT COUNT(*), MIN(last_walk)` over
  the scoped rows, and walks when the scope is empty/unbuilt (`count == 0`) OR
  `now - MIN(last_walk) > staleness_secs`. It reuses `walk`, which is LOCAL-only
  and issues zero network calls, so the boundary and the no-fetch guarantee hold.
  Returns whether it walked (drives the tests). Wired into the MCP `query` and
  `search` handlers (`remote::mcp::logic`) as the first step before serving, so
  an unbuilt catalog is never served empty-as-success. `--fetch` remains the ONLY
  network path and stays CLI-only.
- `remote` gained a direct `rusqlite` dependency (`features = ["bundled"]`, same
  version as `catalog`) so the `fetch_refresh` core can name `Connection` and its
  tests can assert catalog rows. This does NOT touch the boundary: the rule is
  `catalog` must never depend on `remote`; `remote -> rusqlite` is fine and
  `remote` already reaches `rusqlite` transitively via `catalog`.

### Deviations
- **Auto-walk-on-stale folded into Phase 4** (disclosed). The design's
  Architecture ("MCP query may trigger a local walk when rows are stale") and
  Edge Cases ("Empty or fully-stale catalog on an MCP query") were never pinned
  to a phase bullet, and `catalog.staleness_secs` was otherwise inert. Phase 3's
  open question flagged this. Implemented here per the Phase-4 task instructions;
  recorded in the design doc's Phase 4 plan bullet + acceptance criteria.
- The staleness gate lives in `catalog::walk::ensure_fresh` (a catalog function
  called by the MCP `query`/`search` handlers), rather than being inlined into
  `catalog::tools::query`/`search` themselves. The design's suggestion was "e.g.
  `catalog::query`/`search` take the config + root and walk-if-stale before
  serving"; keeping the SELECT-only tools pure (`&Connection`) and putting the
  `&mut Connection` walk gate in one shared `ensure_fresh` avoids threading walk
  params + a `&mut` borrow through all four tools. Same effect (query/search walk
  first when stale), correct seam, testable in the `catalog` crate.

### Tradeoffs
- Staleness uses `MIN(last_walk)` (the OLDEST in-scope row) as the age, so a
  single stale row triggers a whole-subtree re-walk. Simpler and safer than a
  per-row refresh; the walk is cheap and idempotent, and a partial-freshness
  index is the exact thing the gate exists to avoid.
- `--fetch` re-walks the ENTIRE `catalog.root` subtree once after fetching,
  rather than re-walking only the repos that fetched. A full walk is idempotent
  and also refreshes local-only state (dirty flags, new commits) for free; the
  cost is re-reading unchanged repos, which the parallel walk already does fast.
- Test fetches use LOCAL bare repos as `origin` (a `git clone` of a bare
  fixture), so a real `git fetch origin` succeeds fully OFFLINE. The fail path
  points `origin` at a NON-EXISTENT local path, which fails INSTANTLY (no
  network, no hang) -- deliberately chosen over an unreachable host, since the
  subprocess timeout default is 300s and a real dead host could stall the test.

### Fetch fail-isolation proof
- `test_fetch_failure_is_isolated_and_run_continues` (`remote/src/catalog/tests.rs`):
  two repos across two orgs cloned from a shared local bare origin; one has its
  `origin` repointed at a non-existent path. `fetch_refresh` returns
  `fetch_failed == 1`, `fetched == 1`, `walk.repos_indexed == 2`. The good repo
  refreshed (`last_fetch` set, `behind == Some(1)` after the origin advanced),
  while the broken repo's failed fetch left its tracking ref UNrefreshed
  (`behind == Some(0)`) -- proving the failure was isolated and the run
  continued. The `warn!`+`eprintln!` fired (captured in test stdout).

### Auto-walk-on-stale proof
- `test_ensure_fresh_walks_unbuilt_catalog` (`catalog/src/walk/tests.rs`): an
  EMPTY DB -> `ensure_fresh` returns `true` (walked) and `repo_row_count == 2`
  (real rows, never empty-as-success).
- `test_ensure_fresh_never_fetches`: repos with an unreachable `origin`; after
  `ensure_fresh`, `last_fetch IS NULL` for every row and NO `FETCH_HEAD` file
  exists -- the auto-walk path writes zero FETCH_HEAD (LOCAL only, no fetch).
- `test_ensure_fresh_skips_when_fresh` (fresh rows -> returns `false`, no walk)
  and `test_ensure_fresh_walks_when_stale` (backdated `last_walk = 1` ->
  returns `true`, re-walk refreshes the stamp) pin the TTL boundary both ways.

### Open questions
- None. (Phase 3's auto-walk-on-stale open question is now closed: implemented
  here.)
