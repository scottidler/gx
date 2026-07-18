# Implementation Notes: gx onto mcp-io (Track A)

Running, append-only record of how the implementation diverges from or interprets
`2026-07-17-gx-onto-mcp-io.md`. Per `/how-to-execute-a-plan`.

## Phase 0: prove rmcp 2.2 on mcp-io's client-side surface

Executed in `tatari-tv/mcp-io-rs` (the sibling repo), not gx. Zero gx changes, per the doc.

### Design decisions
- Used `cargo add rmcp@2.2 --features server,macros,client` (dep) and `--dev ... server,macros`
  (dev-dep) per rust.md (never hand-edit versions). Resolved to rmcp **2.2.0** in `Cargo.lock`.
- Kept the `version = "2.2"` spec (not pinned `2.2.0`) — mirrors the prior `2.1.0` style and
  lets patch releases float, matching the rest of the manifest.

### Deviations
- None. The doc's Phase 0 predicted the risk surface was `bundle.rs`'s client path
  (`RunningService<RoleClient, ()>`, `model::Tool`) and `error.rs`'s boxed
  `ClientInitializeError`/`ServiceError`. In practice the 2.1→2.2 bump was **source-compatible**
  on every rmcp-typed surface: `cargo check --all-targets` compiled with ZERO code edits.

### Tradeoffs
- Ran Phase 0 inline (not via a phase-implementer agent) because it is the load-bearing spike
  that gates the whole tag-order commit; full visibility was worth more than delegation here.

### Open questions
- None for Phase 0. The mcp-io release version (Phase 1) is deliberately unnamed per the doc
  ("Do NOT pre-name the version") and is the operator's call.

### Success criteria — verified
1. ALL rmcp-typed surfaces compile on 2.2 (serve.rs, bundle.rs, register/mod.rs, error.rs, tests,
   examples): `cargo check --all-targets` finished clean. ✓
2. A real stdio MCP initialize handshake succeeds against a trivial handler: the `serve` tests feed
   real `initialize`/`notifications/initialized` JSON-RPC frames and assert the response frame;
   `tests/stdout.rs::serve_writes_only_jsonrpc_frames_to_stdout` passes; the client-side handshake
   (`bundle::tests::test_advertised_tools_lists_handler_tools`, a real in-process
   `RunningService<RoleClient, ()>` round trip) passes. ✓
3. `mcp-io` `otto ci` green: 45 unit tests + integration + doctests, exit 0. ✓

## Phase 1: release mcp-io on 2.2

Operator step (not a code bullet). Scott chose "I drive the PR" + a **minor** bump.

### Design decisions
- Minor bump 0.1.4 -> **0.2.0**: mcp-io's public API re-exports rmcp types, so
  requiring rmcp 2.2 is a breaking change for consumers under 0.x semver.
- The rmcp-floor change and the version bump landed as ONE amended commit (`bump
  --no-tag -m` amends the manifest bump into the feature commit).

### Deviations / Tradeoffs / Open questions
- None. Standard gated-repo flow (feature branch -> PR #12 -> approve+merge ->
  `bump --tag-only` on merged main -> push `v0.2.0`).

### Success criteria — verified
1. `v0.2.0` tag on `origin/main` (annotated, points at merged commit `30a7af5`). ✓
2. gx resolves against the tag: `mcp-io v0.2.0` locked from
   `git+https://github.com/tatari-tv/mcp-io-rs?tag=v0.2.0#30a7af5`. ✓

## Phase 2: embed `gx mcp`

### Design decisions
- **mcp lives in the gx LIB** (`pub mod mcp`, all submodules `pub`), and the gx
  BIN consumes `gx::mcp::server::GxMcpServer` via the library. This resolves a
  type-identity hazard: gx compiles every module into BOTH the lib and the bin,
  so `main.rs`'s bin-local `config::Config` is a DISTINCT type from the lib's
  `gx::config::Config` that the handler holds. The mcp arm loads
  `gx::config::Config` explicitly and feeds the library handler. — `src/main.rs::run`
- **mcp arm intercepts before gx's `env_logger` init.** gx's `setup_logging`
  uses `env_logger::init()` (panics on double-init); mcp-io's `init_logging`
  owns the mcp process's file logging (via `try_init`). The `Commands::Mcp` arm
  is handled in `run()` after `--cwd`/`Config::load` but BEFORE `setup_logging`,
  and `std::process::exit`s mcp-io's return code (renew's contract). `--cwd` was
  hoisted above `setup_logging` so config/mcp/repo-discovery all see it; the
  "Changed working directory" info log stays after logging is live. — `src/main.rs::run`
- **Server name** set via `.with_server_info(Implementation::new("gx",
  env!("CARGO_PKG_VERSION")))` in `get_info` — else rmcp reports "rmcp" and `gx
  mcp status` warns. Version rides gx's `CARGO_PKG_VERSION` (0.6.3), matching
  `mcp_io!()`'s captured version. — `src/mcp/server.rs`
- `run_application`'s match gains `Commands::Mcp(_) => unreachable!(...)` (the
  arm is intercepted in `run()` and never reaches dispatch). — `src/main.rs`

### Deviations
- **The doc's "move" is realized as copy-in-P2 + delete-in-P3.** `otto ci` builds
  `--workspace --all-targets`, so gx-mcp is compiled by CI until Phase 3 removes
  it. Physically moving the source out of `gx-mcp/src` in Phase 2 would break
  gx-mcp's build and turn CI red mid-phase. So Phase 2 COPIES the four modules
  into `gx/src/mcp/` (path-rewritten `gx::`->`crate::`, sibling refs
  ->`crate::mcp::*`) and leaves gx-mcp intact; Phase 3 deletes gx-mcp. Both
  phases stay green. Net effect is the doc's move, split across two green commits.

### Tradeoffs
- tokio features `["rt-multi-thread", "macros"]` (minimal: the handler only needs
  `spawn_blocking`) vs gx-mcp's `["full"]`. Compiles + serves correctly.
- Internal log strings and the `instructions` text still say "gx-mcp" (e.g.
  `"gx-mcp tool: status"`, `"gx-mcp: MCP surface for gx fleet campaigns"`). Left
  unchanged deliberately: the AC is "behavior UNCHANGED from standalone gx-mcp",
  and the migrated tool-behavior tests assert on the tool set/instructions.
  A cosmetic rename is deferred to avoid scope creep.

### Open questions
- None.

### Success criteria — verified live (debug binary)
1. `gx mcp serve` completes a real MCP initialize handshake: piped
   initialize/initialized/tools-list frames returned a well-formed initialize
   result. ✓
2. `serverInfo.name == "gx"` (version 0.6.3) and `gx mcp status` shows the `gx`
   server key with NO handshake-name mismatch warning. ✓
3. `tools/list` returns the 6 read-only tools (change-get, change-list, doctor,
   repo-discover, review-status, status); the 4 mutating tools are gated off by
   default — identical tool set + instructions to the standalone gx-mcp. ✓

## Phase 3: retire gx-mcp

### Design decisions
- The 3 integration tests moved to `gx/tests/` (they are now gx-package
  integration tests, so `env!("CARGO_BIN_EXE_gx")` + `.args(["mcp", "serve"])`
  reach the same server; `mcp_tools_test` already `use gx::{create,state}`,
  which resolves against the gx lib). Helper renamed `gx_mcp_binary` ->
  `gx_binary`; asserted log path `gx-mcp.log` -> `gx.log` (mcp-io writes
  `<XDG_DATA_HOME>/gx/logs/gx.log`).
- `gate.rs`'s module doc was corrected: it claimed the policy "lives in gx-mcp"
  and "would be dead code in the gx bin target" — false now that it lives in the
  gx lib's `mcp` module. — `src/mcp/gate.rs`
- Workspace `members = []` (gx is the sole member) rather than deleting the
  `[workspace]` table, because gx's `[package]` uses `version.workspace = true`.

### Deviations
- The handshake test's log-content assert changed from
  `contains("gx-mcp starting") || contains("GxMcpServer")` to
  `contains("GxMcpServer")`: the old gx-mcp `main`'s "gx-mcp starting" line is
  deleted, so that disjunct was dead. The gx handler's own entry log
  (`GxMcpServer::new`/`get_info`, DEBUG) proves logging landed in mcp-io's file.
- Cosmetic "gx-mcp" strings left unchanged in the migrated tests (client-info
  labels like `"gx-mcp-handshake-test"`, header comments) and in the server
  module's internal log/instruction text — not behavior, and touching the
  instruction text would fight the AC's "behavior unchanged" requirement.

### Tradeoffs
- `git rm -r gx-mcp` (recoverable from history) rather than a filesystem delete;
  empty dirs cleaned with `rmdir` (only removes empty dirs). No `rm -rf`.

### Open questions
- None.

### Success criteria — verified
1. No `gx-mcp` binary target exists (crate deleted, dropped from workspace
   members). ✓
2. `otto ci` green with the 3 migrated test files (mcp_handshake: 2, mcp_tools:
   11, e2e_campaign: 1), full suite green. ✓
3. A migrated test bites: inverting the gating default (`!is_mutating` ->
   `is_mutating`) turned `mcp_handshake_test` RED at the read-only-tool assertion
   (`default surface must include read-only tool status: [create-apply, ...]`),
   then reverted to green. ✓

## Track A acceptance criteria — final verification

- [x] `gx mcp serve` completes an MCP initialize handshake AND `gx mcp status`
  reports server name `gx` with no mismatch warning. (Phase 2, live.)
- [x] All 10 tools list under the gating rules, behavior unchanged (6 read-only
  enabled, 4 mutating gated off by default; migrated tools test passes).
- [x] The `gx-mcp` binary target no longer exists; `otto ci` green with the 3
  migrated MCP test files; breaking one proves it bites. (Phase 3.)
- [x] `mcp-io` builds + passes `otto ci` on rmcp 2.2 (Phase 0) and is tagged
  `v0.2.0` on `origin/main` before gx depends on it (Phase 1).
