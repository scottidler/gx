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
