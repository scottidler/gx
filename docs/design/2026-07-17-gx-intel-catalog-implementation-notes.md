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
