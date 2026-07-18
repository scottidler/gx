# Design Document: gx read-only intel catalog

**Author:** Scott Idler
**Date:** 2026-07-17
**Status:** Draft
**Review Passes Completed:** 5/5 self + review-panel (Architect + Staff, both rc=0, reviewed as the combined doc 2026-07-17); all findings folded
**Track:** B1 (feature) (companions: `2026-07-17-gx-onto-mcp-io.md` Track A, and `2026-07-17-gx-lib-decomposition.md` Track B0, which BOTH land first)
**Gated on:** Track B0 (gx lib decomposition) landing. The intel tools live in a new `catalog` crate depending on `local` only; that crate cannot exist until B0 splits the lib. No open questions remain.

## Summary

Add a read-only intel layer to gx: a local SQLite catalog of every repo under a configured root (in `<org|user>/<name>` form) plus 4 MCP tools (`query` | `search` | `read` | `deps`) an agent uses to reason across the whole cloned fleet. Cross-org READS are allowed (intel). Cross-org OPERATIONS stay a permanent, closed non-goal.

Depends on Track A (`gx mcp` on `mcp-io`) already landed: the 4 tools are added to the migrated `gx mcp` handler.

## Problem Statement

### Background

- gx = CLI for automating git activities over 2+ repos, with an MCP server (`gx mcp`, after Track A) fronting its campaign cores.
- No tool lets an agent ask questions ACROSS all cloned repos. `gx status` reports on-main-vs-not per invocation, but there is no queryable catalog, no cross-repo search, no dependency lookup.

### Problem

No cross-repo read/intel surface. An agent cannot cheaply answer "which of my repos are dirty", "which use `serde`", or "where is `FooClient`" across the fleet.

### Goals

- Add a local SQLite catalog of repos under a configured root, in `<org|user>/<name>` form.
- Add 4 read-only intel tools: `query`, `search`, `read`, `deps`.
- Cross-org READS allowed, subtree-scoped by a clamped `root`.

### Non-Goals

- **Cross-org OPERATIONS in one run.** Permanent, closed non-goal (reaffirmed 2026-07-14). Never. Not parked, not a gap.
- **Symbol/definition indexing** (the heavy 5th tool). Parked. Revisit condition: an observed need the 4 tools cannot serve.
- **A live/always-fresh index service.** The catalog is a rebuildable cache with VISIBLE staleness, not a hot service.
- **Remote transports / a daemon.** Inherited from Track A: stdio only.

## Proposed Solution

### Overview

Depends only on the `local` crate (Track B0) plus the migrated `gx mcp` handler (in `remote`) from Track A. No external dependency of its own.

### Architecture

- A SEPARATE SQLite catalog at `$XDG_CACHE_HOME/gx/catalog.db` (rebuildable index -> cache dir, not the data dir where `changes/` lives). Not a change to `StateManager`'s JSON store.
- Built by two operations:
  - **walk** (local, no network, no persona): `gx catalog`. Reuses `repo::discover_repos` + `local::git::get_repo_status_local(repo)` (branch/dirty AND ahead/behind from local tracking refs, no fetch; the NEW local-only status fn from Track B0, since the old `get_repo_status_with_options` is fetch-capable and lives in `remote`), `git log` for last-commit, `FETCH_HEAD` mtime for last-fetch, and a manifest parse for lang + deps. Upserts into SQLite, and prunes rows for repos no longer on disk.
  - **fetch refresh** (network, per repo): `gx catalog --fetch`. For each repo, `git::fetch_origin(path)` (unchanged; it fetches the repo's existing `origin` remote), then re-walk that repo. Per-org auth rides each repo's `origin` URL + `~/.ssh/config` host aliases set at clone time; gx does NOT select a key per org (`get_ssh_command` reads a single global `core.sshCommand`, src/ssh.rs:71). A repo whose fetch fails (auth/network) is reported loudly and skipped; the run continues.
- The 4 intel tools live in a new `catalog` crate that depends on `local` ONLY (Track B0 produced `local` = credential-free; `remote` = credential-bound). Because `catalog`'s Cargo.toml has no path to `remote`, an intel tool CANNOT compile a call to `persona`/`github`/`ssh`/remote-git. The boundary is compiler-structural. A tiny CI guard asserts `catalog` never gains a `remote` dependency.
- The one network path (`--fetch`) is CLI-only and iterates one repo at a time, each fetching its own `origin`. That is N independent single-org fetches, never one cross-org operation.

### Data Model

Two SQLite tables. Open with WAL, a named `BUSY_TIMEOUT_MS` const, `synchronous=NORMAL`, and `PRAGMA foreign_keys=ON` per connection (else `ON DELETE CASCADE` is inert); migration DDL idempotent inside one transaction guarded by `user_version`. Idempotence of a re-walk: a repo's `deps` rows are replaced per repo (`DELETE FROM deps WHERE repo_slug=?` then insert) inside the same txn as its `repos` upsert.

```sql
CREATE TABLE IF NOT EXISTS repos (
  slug             TEXT PRIMARY KEY,   -- <org|user>/<name>
  org              TEXT NOT NULL,
  name             TEXT NOT NULL,
  path             TEXT NOT NULL,      -- canonical absolute; scope clamp filters on this
  branch           TEXT,
  dirty            INTEGER NOT NULL,   -- 0/1
  ahead            INTEGER,            -- from local tracking ref, may be stale
  behind           INTEGER,            -- from local tracking ref, may be stale
  lang             TEXT,               -- primary language guess
  last_commit_sha  TEXT,
  last_commit_time INTEGER,            -- unix
  last_walk        INTEGER NOT NULL,   -- unix; when local state was read
  last_fetch       INTEGER             -- unix; FETCH_HEAD mtime, NULL if never
);

CREATE TABLE IF NOT EXISTS deps (
  repo_slug   TEXT NOT NULL REFERENCES repos(slug) ON DELETE CASCADE,
  ecosystem   TEXT NOT NULL,          -- cargo | npm
  name        TEXT NOT NULL,
  version_req TEXT,
  kind        TEXT NOT NULL,          -- normal | dev | build
  PRIMARY KEY (repo_slug, ecosystem, name, kind)
);
CREATE INDEX IF NOT EXISTS deps_name ON deps(name);
CREATE INDEX IF NOT EXISTS deps_repo ON deps(repo_slug);
```

- `search` and `read` need NO table: they read the working tree live (never stale). Only `query` and `deps` are index-backed, and both surface `last_walk`/`last_fetch` so an agent sees staleness.

### API Design

**4 new tools**, added to `McpTool` (`src/config.rs:87`, default ENABLED, read-only category):

- `query(root?, where{ dirty?, branch?, org?, lang?, behind_gt? })` -> rows of repo metadata. Indexed SELECT.
- `search(root?, pattern, glob?)` -> `[{slug, path, line_no, line}]`. Shells out to `rg` over the scoped repo paths (live).
- `read(slug, path)` -> file contents. Reuses `file::read_utf8_or_skip`; path clamped inside the repo dir via the `file::validate_new_file_path` pattern.
- `deps(dependency?)` -> repos using it; `deps(slug?)` -> that repo's deps. Indexed SELECT both directions.

**Scope clamp (all 4):** canonicalize `catalog.root`, the requested `root` (default caller CWD), and stored repo paths (the walk stores canonical paths). Match `path = :root OR path LIKE :root || '/%'` (the trailing `/%` avoids the sibling-prefix bug where `/repos/foo` wrongly matches `/repos/foobar`). A `root` that canonicalizes outside `catalog.root`, or does not exist, is REJECTED loudly (fail closed), never widened or emptied. Same canonicalize-and-compare pattern as `file::validate_new_file_path` (src/file.rs:187), so `..` and symlink escapes are rejected.

**Output bounds (all 4):** each tool caps result count and total bytes (config-defaulted) and returns `truncated: true` when a cap trips; `search` sets an `rg` subprocess timeout; `read` fails loud on an oversized file unless a bounded line range is requested. Rationale: an MCP response serializes one JSON content block (gx-mcp/src/server.rs:96), so fleet-sized payloads blow the protocol limit (the prior gx-mcp design avoided exactly this).

**Catalog-building surface (CLI):** `gx catalog` (local walk) and `gx catalog --fetch` (walk + per-repo fetch). One verb, a flag for the network step (not a subcommand split). Fetch is NEVER an MCP tool: an MCP `query` may trigger only a local walk when rows are stale beyond a TTL; it never spawns fetches.

**Config:** a new `catalog:` section, kebab-case, under `deny_unknown_fields`:

```yaml
catalog:
  root: ~/repos          # ceiling for the scope clamp; catalog covers this subtree
  staleness-secs: 3600   # a query older than this triggers a local walk of the scoped subtree
```

Plus the 4 new `mcp.tools` keys, default enabled (read-only category). The scope clamp's "configured repos root" is `catalog.root`.

### Implementation Plan

Depends on Track A landed (the migrated `gx mcp` handler) AND Track B0 landed (`local`/`remote` crates; the migrated handler lives in `remote`).

#### Phase 1: catalog schema
**Model:** sonnet
- Add `rusqlite`; add a `xdg_cache_dir` helper mirroring `config::xdg_data_dir`; open + migrate `catalog.db` (WAL, `BUSY_TIMEOUT_MS` const, `synchronous=NORMAL`, `foreign_keys=ON`, idempotent DDL in one txn, `user_version`).
- Wire config: add `CatalogConfig` to `Config` (src/config.rs:38) + defaults (src/config.rs:312), `~` expansion for `catalog.root`, and update the annotated `gx.yml` example. Kebab-case under `deny_unknown_fields`.
- **Success criteria:** (1) DB opens under `$XDG_CACHE_HOME/gx/`; (2) re-running migration is a no-op; (3) `repos` carries every column in the Data Model; (4) a `gx.yml` with a `catalog:` block parses and one with an unknown `catalog.*` key fails loudly (parse test).

#### Phase 2: walk indexer
**Model:** opus
- Reuse `discover_repos` + `local::git::get_repo_status_local(repo)` (the Track B0 local-only, no-fetch status fn), `git log` for last-commit, `FETCH_HEAD` mtime for last-fetch; parse `Cargo.toml`/`package.json` for lang + deps; store CANONICAL paths; upsert `repos` and replace `deps` per repo in one txn; prune rows for repos gone from disk.
- **Success criteria:** (1) walk populates N repos with ZERO `git fetch` calls (network asserted absent); (2) re-walk is idempotent and pruning a removed repo clears BOTH its `repos` and `deps` rows; (3) dep rows link repo <-> dependency both directions.

#### Phase 3: read-only intel tools
**Model:** opus
- Create the `catalog` crate (depends on `local` + `rusqlite`, NOT `remote`); implement the 4 tools there with the subtree clamp + output bounds; `search` shells `rg` (with timeout); `read` reuses `local::file::read_utf8_or_skip` + path clamp.
- Wire the 4 tools into the `gx mcp` handler (in `remote`) and add `Query`/`Search`/`Read`/`Deps` to `McpTool` (default enabled).
- Add an `rg` presence check to `gx doctor`; add a CI guard asserting `catalog` has no `remote` dependency.
- **Success criteria:** (1) all 4 tools return only rows/hits under a valid clamped root; (2) fail-closed clamp tests BITE: sibling-prefix (`foo` vs `foobar`), `..`, symlink escape, out-of-root root; (3) an oversized result is truncated with `truncated: true`; (4) `read` on a non-utf8 file errors clearly (not empty-as-success); (5) a cross-org read succeeds; (6) `gx doctor` reports missing `rg`; (7) `cargo tree -p catalog` shows no `remote`, and adding such a dep fails the CI guard.

#### Phase 4: fetch refresh
**Model:** opus
- `gx catalog --fetch`: iterate repos calling `git::fetch_origin(path)` (unchanged; auth rides each repo's `origin` remote), then re-walk each. A per-repo fetch failure is reported and skipped, not fatal.
- **Success criteria:** (1) fetch updates `last_fetch` + ahead/behind for a reachable repo; (2) fetch succeeds for a repo in each of two orgs via their existing `origin` remotes; (3) a repo whose fetch fails is reported loudly and does NOT abort the run; (4) the boundary enforcement confirms the fetch path is unreachable from the intel tools.

## Acceptance Criteria

- [ ] `gx catalog` populates the SQLite catalog with zero `git fetch` calls (network asserted absent); re-run is idempotent, and removing a repo prunes both its `repos` and `deps` rows.
- [ ] `query` returns only rows under a canonicalized, clamped root; sibling-prefix, `..`, and symlink-escape roots are rejected loudly (fail-closed test bites).
- [ ] All 4 tools cap output and return `truncated: true` past the cap; `read` on a non-utf8 file errors clearly.
- [ ] `catalog` depends on `local` only (`cargo tree -p catalog` shows no `remote`); the CI guard bites when a `remote` dep is added; cross-org reads succeed.

## Resolved Decisions

- 2026-07-17 (Scott): cross-org **READS** allowed for intel; cross-org **OPERATIONS** never (reaffirms the permanent non-goal).
- 2026-07-17 (Scott): subtree scope via a clamped `root`.
- 2026-07-17 (Scott): first intel tool set = `query`, `search`, `read`, `deps`; `symbols` deferred.
- 2026-07-17 (author): catalog is a rebuildable cache -> `$XDG_CACHE_HOME/gx` (per taste.md "cache in ~/.cache not ~/.config").
- 2026-07-17 (author): `search` shells out to `rg` (matches gx's git/gh shell-out pattern); `gx doctor` gains an `rg` presence check.
- 2026-07-17 (author): `--fetch` is CLI-only, never an MCP tool; MCP queries may trigger only a local walk.
- 2026-07-17 (author, from review): per-org fetch auth rides each repo's `origin` remote + `~/.ssh/config` host aliases; gx does NOT select a key per org and `fetch_origin` keeps its path-only signature. `resolve_token_env` is gh-CLI only and is NOT used for `git fetch`.
- 2026-07-17 (author, from review): intel tools cap result count + bytes and `search` sets an `rg` timeout, to stay under the MCP protocol payload limit.
- 2026-07-18 (Scott): split from the migration into separate docs; this track opens after Track A lands.
- 2026-07-18 (Scott): cross-org boundary enforced by a separate `catalog` crate depending on `local` only (option b), made possible by the Track B0 decomposition; chosen over the import-lint test and `IntelConfig`.

## Alternatives Considered

### Alternative 1: daemon + HTTP/SSE transport holding a hot index
- **Description:** long-lived process, background refresh, multi-client.
- **Cons:** `mcp-io` non-goal; daemon lifecycle (auth env at spawn, PID/logs, restart, one-process concurrency); a single-user local tool does not need it.
- **Why not chosen:** stdio + on-demand walk suffices at N=1 user.

### Alternative 2: SQLite FTS5 content index for `search`
- **Description:** index repo file contents for fast text search.
- **Cons:** stale-prone cache, invalidation logic; `rg` over the tree is already fast.
- **Why not chosen:** live `rg` is simpler and never stale.

### Alternative 3: `grep`/`ignore` crates instead of shelling `rg`
- **Description:** in-process search via crates.
- **Cons:** heavier direct-dep add; diverges from gx's shell-out pattern.
- **Why not chosen:** shell-out matches the house pattern; recorded here so it is not re-litigated.

## Technical Considerations

### Dependencies
- New crate `catalog` depends on `local` + `rusqlite`, NOT `remote`. Requires Track B0 (decomposition) landed.
- Runtime: `rg` binary (checked by `gx doctor`).

### Performance
- Walk is parallel (existing rayon `par_iter`). `rg` is fast. `query`/`deps` are indexed SELECTs.

### Security
- Intel tools are read-only, no persona, no network.
- `read` clamps the path inside the repo dir; the scope `root` is clamped inside `catalog.root` (fail closed).
- `--fetch` fetches each repo's existing `origin` remote; per-org auth rides the origin URL + `~/.ssh/config`, not a gx-selected token.
- The catalog holds only local metadata (paths, branches, dep names). Nothing secret.

### Edge Cases
- **Empty or fully-stale catalog on an MCP query:** the query auto-walks the scoped subtree first (local, no network); the first such query may be slow. A missing DB file is created on first walk. It never returns empty-as-success on an unbuilt catalog.
- **Non-utf8 / binary file on `read`:** `read_utf8_or_skip` yields `None`; the tool returns a clear "not utf-8" error, never empty-as-success.
- **Writer concurrency:** a query's auto-walk can coexist with a CLI `gx catalog`/`--fetch`; WAL + `BUSY_TIMEOUT_MS` serialize writers rather than erroring. Reads of already-fresh rows use a short read txn and are not blocked by a concurrent stale-triggered walk (WAL readers do not block on the writer).
- **Worktrees / bare containers:** the catalog defers to `discover_repos` slug/path semantics (the same flat vs bare-container-worktree handling `gx status` already uses). No new repo-shape logic.
- **Repo removed since last walk:** re-walk prunes its rows (ON DELETE CASCADE clears its deps).

### Testing Strategy
- Add: walk idempotency + deps-prune; no-network assertion on walk; clamp fail-closed variants (sibling-prefix, `..`, symlink); output-cap/truncation; non-utf8 `read`; `catalog:` config parse (+ unknown-key rejection); fetch-failure isolation (one repo fails, run continues); the boundary-enforcement test.
- Break-a-test-to-prove-it-bites on the clamp test and the boundary-enforcement test.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `rg` not installed | Low | Low | `gx doctor` flags it; `search` errors loudly, never returns empty-as-success |
| Catalog staleness misleads the agent | Low | Med | `query`/`deps` return `last_walk`/`last_fetch`; auto-walk on stale (local only) |
| Cross-org operation sneaks in via the intel path | Low | High | `catalog` crate cannot name `remote` (compiler-structural); CI guard blocks re-adding the dep; `--fetch` is CLI-only and per-repo single-org |

## Open Questions

- (none: cross-org boundary resolved via the `catalog`/`local` crate split, Track B0. B1 is gated on B0 landing, not on any open question.)

## References

- Companions: `docs/design/2026-07-17-gx-onto-mcp-io.md` (Track A) and `docs/design/2026-07-17-gx-lib-decomposition.md` (Track B0); BOTH land first
- memory: `no-cross-org-boundary-ever`
- Research brief (2026-07-17): file:line anchors for the intel reuse map (discovery, local status, fetch, file read)
