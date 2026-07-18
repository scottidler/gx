# Design Document: gx onto mcp-io

**Author:** Scott Idler
**Date:** 2026-07-17
**Status:** Implemented
**Review Passes Completed:** 5/5 self + review-panel (Architect + Staff, both rc=0, reviewed as the combined doc 2026-07-17); all findings folded
**Track:** A of 3 (ships first; companions land after in order: `2026-07-17-gx-lib-decomposition.md` (B0), then `2026-07-17-gx-intel-catalog.md` (B1))

## Summary

Migrate gx's bespoke `gx-mcp` server onto `mcp-io` (the house scaffolding lib) as a `gx mcp` subcommand, and delete the standalone binary. rmcp 2.2 across the board. Pure infra migration: the 10 existing MCP tools keep their behavior; only the wiring, registration, and logging move to the house standard.

## Problem Statement

### Background

- gx = CLI for automating git activities over 2+ repos.
- gx already has an MCP server, `gx-mcp/`, fronting its campaign cores. It is wired up bespoke: standalone binary, rmcp 2.2.0, MANUAL registration, no `.mcpb` bundle.
- `mcp-io` (tatari-tv, Scott's lib) exists precisely to kill that bespoke wiring across the four in-house MCP servers. Its README names the problem: "Four in-house MCP servers exist today, all wired up differently: different rmcp versions, different registration steps, no `.mcpb` bundle anywhere."

### Problem

`gx-mcp` is one of the bespoke servers `mcp-io` was built to replace. It re-derives scaffolding by hand, needs manual registration, ships no bundle, and will drift from the fleet.

### Goals

- Migrate gx's MCP surface onto `mcp-io` as `gx mcp` (serve | register | unregister | status | bundle); delete the standalone `gx-mcp` binary.
- rmcp 2.2 everywhere (`mcp-io` bumps 2.1 -> 2.2; gx keeps its 2.2 handler).
- Preserve the 10 existing tools and their gating unchanged.

### Non-Goals

- **The intel catalog + 4 read-only tools.** Separate docs, Tracks B0 then B1. Land after this.
- **Remote transports (HTTP/SSE) / a daemon.** Excluded by `mcp-io`'s own non-goal. stdio only.
- **MCP resources or prompts.** Tools only, matching the fleet.
- **Any behavior change to the 10 existing tools.** This is a wiring migration, not a feature.

## Proposed Solution

### Overview

This track carries the cross-repo dependency: `mcp-io` must bump to rmcp 2.2 and be tagged before gx can depend on it by tag. So the first two phases live in the sibling repo.

### Architecture

- Add `Mcp(mcp_io::McpCmd)` as one arm of gx's `Commands` enum (`src/cli.rs:118`), intercepted early in `main.rs` (~`:309`, after `Config::load`, before `run_application`). Same shape as `renew`'s `Update` arm.
- `mcp-io` owns the serve loop, logging init, registration, and bundle. gx owns the `ServerHandler` and its tools.
- Move `gx-mcp/src/{server,gate,logic,schema}.rs` -> `gx/src/mcp/`. The handler keeps its `Arc<Config>` (`gx-mcp/src/server.rs:46`); `mcp-io`'s bound is only `H: ServerHandler + Send + 'static` (no `Clone`), so the build closure loads a fresh `Config` and moves it into `GxMcpServer::new(config)`.
- `get_info` gains `.with_server_info(Implementation::new("gx", ...))` (`gx-mcp/src/server.rs:247` currently omits it, which would report the server name as `"rmcp"`). `gx mcp status` warns if this is missed.
- gx stays fully sync (rayon, `std::process`) for every other command; `mcp-io` builds its own tokio runtime only when the `Mcp` arm runs.

### Data Model

N/A. This track adds no persistent state.

### API Design

- **MCP subcommand (from `mcp-io`):** `gx mcp serve | register | unregister | status | bundle`.
- **Existing 10 tools:** preserved unchanged, gated via the `McpTool` enum (`src/config.rs:87`).

### Implementation Plan

#### Phase 0: prove rmcp 2.2 on mcp-io's client-side surface
**Model:** opus
- In `mcp-io`: `cargo add rmcp@2.2` (dep + dev-dep), `otto ci`. Zero gx changes.
- The unproven surface is `bundle.rs`'s client path (`RunningService<RoleClient, ()>`, `model::Tool`) and `error.rs`'s boxed `ClientInitializeError`/`ServiceError` (gx-mcp already proves the server path on 2.2).
- **Success criteria:** (1) ALL rmcp-typed surfaces in `mcp-io` compile on 2.2 (serve.rs, bundle.rs, register/mod.rs, error.rs, tests, examples), not just bundle.rs/error.rs; (2) a real stdio MCP initialize handshake succeeds against a trivial handler; (3) `mcp-io` `otto ci` green.

#### Phase 1: release mcp-io on 2.2
**Model:** sonnet
- Commit the 2.2 bump on a feature branch; land via PR; operator tags + pushes per `git.md` `bump` flow.
- **Operator step, not a code bullet.** Do NOT pre-name the version in this doc.
- **Success criteria:** (1) a new `mcp-io` tag exists on `origin/main`; (2) `cargo fetch` of gx against that tag resolves.

#### Phase 2: embed `gx mcp`
**Model:** opus
- Add `mcp-io` (git dep by tag) + `rmcp` 2.2 + `tokio` + `schemars` to `gx/Cargo.toml`.
- Move `gx-mcp/src/{server,gate,logic,schema}.rs` -> `gx/src/mcp/`; add `pub mod mcp` to `lib.rs`.
- Add `Mcp(mcp_io::McpCmd)` arm (`cli.rs:118`); intercept early in `main.rs` (~`:309`); `std::process::exit(cmd.run(&io, || Ok::<_, Infallible>(GxMcpServer::new(config))))`.
- Add `.with_server_info(Implementation::new("gx", ...))` to `get_info`.
- **Success criteria:** (1) `gx mcp serve` completes an MCP initialize handshake; (2) `gx mcp status` reports name `gx` with no mismatch warning; (3) all 10 tools list under the gating rules.

#### Phase 3: retire gx-mcp
**Model:** sonnet
- Delete `gx-mcp/` crate + its workspace `members` entry (`Cargo.toml:2`).
- Rewrite the 3 test files (`gx-mcp/tests/*`) to spawn `CARGO_BIN_EXE_gx` with `["mcp","serve"]`; update the asserted log path to `mcp-io`'s convention (`$XDG_DATA_HOME/gx/logs/gx.log`); delete gx-mcp's own `setup_logging`.
- **Success criteria:** (1) no `gx-mcp` binary target exists; (2) `otto ci` green with migrated handshake/tools/e2e tests; (3) breaking one migrated test proves it bites.

## Acceptance Criteria

- [ ] `gx mcp serve` completes an MCP initialize handshake AND `gx mcp status` reports server name `gx` with no mismatch warning.
- [ ] All 10 existing tools list under the gating rules, behavior unchanged from the standalone `gx-mcp`.
- [ ] The `gx-mcp` binary target no longer exists; `otto ci` is green with the 3 migrated MCP test files, and breaking one proves it bites.
- [ ] `mcp-io` builds and passes `otto ci` on rmcp 2.2 (Phase 0), and is tagged on `origin/main` before gx depends on it (Phase 1).

## Resolved Decisions

- 2026-07-17 (Scott): rmcp **2.2 across the board** (`mcp-io` bumps 2.1 -> 2.2, gx keeps 2.2).
- 2026-07-17 (author, from research): `Config` stays non-`Clone`; the handler owns `Arc<Config>`; `mcp-io`'s `H` bound is `Send + 'static` only.
- 2026-07-17 (author): the standalone `gx-mcp` binary collapses into `gx mcp serve` (house pattern; self-registration + `.mcpb` come for free).
- 2026-07-18 (Scott): split from the intel catalog into separate docs; ship this migration first, open the B tracks (B0 decomposition, then B1 catalog) after it lands.

## Alternatives Considered

### Alternative 1: keep the bespoke gx-mcp binary
- **Description:** leave `gx-mcp/` as a standalone rmcp server.
- **Cons:** re-derives scaffolding, manual registration, no `.mcpb`, drifts from the fleet.
- **Why not chosen:** `mcp-io` exists precisely to kill this. Converge on the in-house standard.

### Alternative 2: make `Config: Clone`
- **Description:** derive `Clone` so the build closure clones config.
- **Cons:** forces `Clone` onto ~8 nested config structs; unnecessary.
- **Why not chosen:** `Arc<Config>` already works; the closure moves a fresh load.

## Technical Considerations

### Dependencies
- gx gains (direct): `rmcp` 2.2, `tokio`, `schemars`, `mcp-io` (git dep by tag).
- `gx-mcp/Cargo.toml` deleted; removed from workspace `members`.
- Consume `mcp-io` by tag with the `CARGO_NET_GIT_FETCH_WITH_CLI` + `insteadOf` recipe (same as `okta-auth-rs`).

### Performance
- tokio spins up only on the `mcp` arm; every other command stays sync.

### Testing Strategy
- Migrate the 3 MCP tests to `CARGO_BIN_EXE_gx` + `["mcp","serve"]`, new log path.
- Break-a-test-to-prove-it-bites on at least one migrated MCP test.

### Rollout Plan
- `mcp-io` 2.2 bump tagged + pushed FIRST (operator). Then gx depends by tag. This track lands. Tracks B0 then B1 follow.
- Existing external `gx-mcp` registrations must be re-registered against `gx mcp serve` (one-time; `gx mcp register` does it). Call out in release notes.

### Version reporting divergence
- `mcp_io!()` registers/bundles under `CARGO_PKG_VERSION` while gx `--version` uses `GIT_DESCRIBE`. Cosmetic. Accepted.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| rmcp 2.1->2.2 client surface in mcp-io breaks | Med | High | Phase 0 spike proves it before the tag order commits |
| Collapsing the binary breaks external registrations | Low | Med | `gx mcp register` re-registers; document in release notes |

## Open Questions

- (none: all findings dispositioned; ready to build)

## References

- `mcp-io` README + `mcp-io-rs/docs/design/2026-07-09-mcp-io-rs.md`
- gx `docs/design/2026-07-12-llm-propose-apply-and-mcp-server.md` (gx-mcp origin)
- Companion: `docs/design/2026-07-17-gx-intel-catalog.md` (Track B1, depends on this)
- Research brief (2026-07-17): file:line anchors for gx-mcp handler, mcp-io host contract, blast radius
