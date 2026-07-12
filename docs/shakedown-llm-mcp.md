# CLI Shakedown Report: gx `llm` propose/apply + `gx-mcp` server

Date: 2026-07-12. Binaries: `target/release/gx` and `target/release/gx-mcp`,
built from the `llm-propose-apply-mcp` branch (workspace version `0.4.1`, not
yet bumped for this feature -- see Not Exercised). All fixtures built under a
throwaway scratch directory with bare-file remotes and a deterministic
fake-agent script; no live LLM, no real GitHub org, nothing under `~/repos`
touched.

## Summary

| Metric | Count |
|--------|-------|
| New CLI surface | `gx create ... llm`, `gx apply` |
| New protocol surface | `gx-mcp` (10 tools over stdio) |
| Commands/flags exercised | `llm` (one-shot + `--propose`), `apply` (`--yes`, missing/malformed id), all 10 MCP tools |
| Passed | every exercised path behaved correctly |
| Failed (product bug) | 0 |
| Cosmetic issues | 0 new (see `shakedown-v0.4.0.md` for the pre-existing eyre-trailer nit) |
| Edge cases tested | 7 (empty diff, agent nonzero exit, drift-then-refuse, re-apply idempotency, bad change-id, fail-closed confirm x2, stuck-proposal doctor report) |
| Focus | the full propose -> present -> apply -> undo round trip, both CLI and MCP |

Verdict: the `llm` change type and `gx-mcp` are solid on this pass. Every
failure mode reported loudly and left the real worktree untouched; the
confirm gates fail closed on non-TTY without `--yes`; the MCP server enforces
config gating and the confirm-token protocol exactly as designed. No product
defects found.

## `gx create ... llm` -- PASS

One-shot (`gx create -p app --yes llm "<prompt>"`): proposed the fake agent's
edit, printed the colored diff, and applied in the same invocation --
identical shape to a `sub`/`regex` create, just with an agent-generated
patchset:

```
Targeting 1 repositories:
  unknown/app

=== unknown/app ===
diff --git c/data.md i/data.md
index 286bb20..be3c124 100644
--- c/data.md
+++ i/data.md
@@ -1 +1 @@
-old value
+new value

📊 1 repositories: 1 proposed | 0 empty | 0 failed
GX-2026-07-12T12-42-58 ------- 💾 unknown/app

📊 Applied GX-2026-07-12T12-42-58: 1 applied | 0 drifted/failed (token 6012196dedf6b762)
```

Split flow (`--propose` then a separate `gx apply <change-id>`): the proposal
persisted, present-gate re-rendered the SAME diff at apply time, and the
resulting branch/push was identical to the one-shot path.

- **Empty diff** (agent makes no changes) is a valid per-repo outcome, not an
  error: `0 proposed | 1 empty | 0 failed`, "Nothing to apply", exit 0.
- **Agent nonzero exit** is a loud per-repo `failed` outcome, not a process
  failure: `FAILED unknown/app: agent exited with status 7: boom`, exit 0
  (`--propose` still reports `Run `gx apply <id>`` even though nothing was
  proposed -- harmless, since apply on an all-failed manifest reports
  "nothing to apply").
- **Drift-then-refuse**: proposed against `base_sha` X, then an unrelated
  commit landed on `main` before apply. Apply refused per-repo with the exact
  base_sha mismatch named, `0 applied | 1 drifted/failed`, and the real
  worktree was byte-identical after (`data.md` still "old value", no stray
  commits).
- **Re-apply idempotency**: applying an already-applied change-id again
  re-presents the diff (from the persisted patch, correctly) then refuses:
  "no repositories in a Proposed state for `<id>`; nothing to apply".

## `gx apply` -- PASS, incl. fail-closed prompt

`gx apply <id>` on non-interactive stdin without `--yes` presented the diff
and **failed closed**: "Refusing to apply 1 proposed repositories without
confirmation on non-interactive stdin; pass --yes to proceed" (exit 1) --
matching every other TTY gate in this codebase. `gx apply <id> --yes`
applied cleanly; `gx apply bogus-id` was rejected at clap parse time
("change-id must start with 'GX-'"); `gx apply GX-does-not-exist` named the
expected manifest path it couldn't find.

## `gx undo` on an llm campaign -- PASS

`gx undo <id>` (with a `gh` shim on `PATH`, since undo's reconcile shells out
to `gh`) on non-interactive stdin without `--yes` printed the plan
("pushed, no PR -> delete remote branch -> delete local branch") and failed
closed identically to `gx apply`. With `--yes`: the branch was deleted from
the bare remote AND locally, `gx doctor` came up clean afterward.

`gx undo` on a **bare, unapplied proposal** printed a different plan line --
"bare proposal; delete proposal artifacts (local only, no remote)" -- the
local-only undo arm; no branch/PR ever existed to reverse.

## `gx doctor` -- PASS (new STUCK PROPOSALS section)

After leaving a drifted proposal un-applied and un-undone:

```
STUCK PROPOSALS (proposed, never applied or undone):
  GX-2026-07-12T12-43-29 (1 repo(s), updated 2026-07-12T19:43:29Z)
  (run `gx apply <change-id>` to apply, or `gx undo <change-id>` to discard)
```

Distinct from the pre-existing `ORPHANED PROPOSALS` section (a proposal
directory with no change state at all). A clean fleet shows `none` for both.

## `gx-mcp` tool surface -- PASS (scripted-client round trip)

The full round trip -- `create-propose` -> `change-get` -> `create-apply` ->
`undo-plan` -> `undo-execute` -- was driven by a scripted JSON-RPC client
(`gx-mcp/tests/e2e_campaign_test.rs`) against a fixture fleet (never
`~/repos`):

1. `create-propose {prompt, patterns: []}` -> minted change-id + confirm
   token; per-repo summary (`outcome: "proposed"`, files list), no full diff.
2. `change-get {change_id}` -> the full per-repo unified diff (the fleet-sized
   full-diff fetch the propose summary deliberately omits).
3. `create-apply {change_id, token}` -> `applied: 1, drifted_or_failed: 0`,
   `status: "Committed"` (no PR: MCP `create-apply` never opens one -- a
   driver opens PRs out-of-band), and the branch landed on the bare remote.
4. `undo-plan {change_id}` -> reconciled against the `gh` shim, minted an undo
   token.
5. `undo-execute {change_id, token}` -> `outcome: "Undone"`; the branch was
   gone from the remote and the change state was `Abandoned`, proposal
   artifacts removed.

Every step asserted BOTH altitudes: no JSON-RPC `error` / tool `isError`, and
the actual git-level effect (branch present/absent, file content on the
pushed branch, state file contents). The test was proven to bite: reverting
the real remote-branch-delete call in `src/undo/core.rs` made the
`undo-execute` step's remote-branch assertion fail as expected, then the
revert was undone.

Config gating and the confirm-token refusals (missing/stale/tampered token,
manifest changed since plan, blob tampered since plan, state changed between
undo-plan and undo-execute) were already proven in Phase 9's
`gx-mcp/tests/mcp_tools_test.rs`; not re-exercised here to avoid duplication.

## Operator step: registering `gx-mcp` with a client (MANUAL)

**Registering `gx-mcp` in an MCP client's config is a manual step; gx never
does this for you.** The server is a plain stdio binary the client spawns --
there is no discovery, no install hook, no CLI command that edits a client's
config on gx's behalf.

1. Build/install it: `cargo install --path .` at the repo root builds the
   `gx` CLI; `gx-mcp` is a separate workspace member built with
   `cargo build --release -p gx-mcp` (its binary lands at
   `target/release/gx-mcp`; it is not installed by `cargo install --path .`,
   which is pinned to the `gx` package -- see the design doc's Phase 8 notes).
2. Point your client's MCP config at that binary as a stdio server. For a
   client using the generic `mcpServers` JSON shape (Claude Desktop and
   others):

   ```json
   {
     "mcpServers": {
       "gx": {
         "command": "/absolute/path/to/target/release/gx-mcp"
       }
     }
   }
   ```

   For Claude Code, the equivalent is:

   ```
   claude mcp add gx /absolute/path/to/target/release/gx-mcp
   ```

3. No env vars are required for a normal setup: `gx-mcp` inherits the
   spawning process's environment (ambient git/gh credentials) and reads
   `~/.config/gx/gx.yml` the same way the `gx` CLI does, so `mcp.tools`
   gating is shared, not duplicated.
4. Every mutating tool (`create-propose`, `create-apply`, `undo-plan`,
   `undo-execute`) is **disabled by default**. Flip it on in `gx.yml` only for
   a client you trust with `gx ... --yes`-equivalent authority (Security /
   Trust model, `gx-mcp/README.md`):

   ```yaml
   mcp:
     tools:
       create-propose: true
       create-apply: true
   ```

5. Restart the client (or its MCP connection) after any `gx.yml` gating
   change: gating is fixed at server construction (a stdio server the client
   respawns per session), so a config edit needs a fresh process to take
   effect.

## Edge cases

| Case | Result |
|------|--------|
| `gx apply <id>` no `--yes`, non-TTY | Fails closed, names `--yes` |
| `gx undo <id>` no `--yes`, non-TTY | Fails closed, names `--yes` |
| `gx apply bogus-id` (non-`GX-`) | Rejected at clap parse |
| `gx apply GX-does-not-exist` | Names the expected manifest path |
| Agent produces empty diff | `0 proposed \| 1 empty`, "Nothing to apply", exit 0 |
| Agent exits nonzero | Per-repo `failed` outcome, exit 0, worktree untouched |
| Drift between propose and apply | Per-repo refusal naming both SHAs, worktree untouched |
| Re-apply an already-applied change-id | "no repositories in a Proposed state...nothing to apply" |
| MCP mutating tool, config disabled (default) | Absent from `tools/list`; call refused "tool not found" |
| MCP `create-apply`, wrong/stale/tampered token | Refused, message cites the token |

## Failures & Bugs

None (product).

## Cosmetic

None new. (`shakedown-v0.4.0.md`'s pre-existing eyre-`Location:` trailer on
not-found errors is unaffected by this work.)

## Observations

- The one-shot `llm` flow now runs the SAME up-front blast-radius confirm as
  every other create action (Phase 6 closed a gap the Phase-4 propose-only
  seam had); with a single matched repo it auto-proceeds under the default
  `confirm-threshold: 5`, so gate #5 (the post-present confirm) is the one
  this shakedown's fail-closed checks exercise directly.
- `gx-mcp`'s log is file-only (`<XDG_DATA_HOME>/gx/logs/gx-mcp.log`); stdout
  never carries anything but JSON-RPC, confirmed by the harness (every line
  read during the round trip parsed as `jsonrpc: "2.0"`).
- The `gh` shim used for `undo`/`undo-plan` handles exactly three
  invocations (PR search, PR close, branch delete); a real `gh` on `PATH`
  handles everything else transparently in production.

## Not exercised (needs real infra, covered by tests)

A live `claude -p` agent generation (covered once, live, by Phase 0's spike;
CI and this shakedown both use the deterministic fake-agent fixture). A real
MCP client (Claude Desktop, Claude Code) driving `gx-mcp` interactively --
this shakedown's round trip is a scripted JSON-RPC client, not a live
registered client; the manual registration steps above are documented but not
re-verified against a live client session in this pass. `create-apply --pr`
/ MCP PR creation against a real GitHub org (covered by the gh-shim
integration suites, `tests/e2e_llm_apply.rs` et al.). The workspace version
has not been bumped for this feature yet, so no `bump`/tag/install/shakedown
of an installed binary is claimed here -- this pass exercised the freshly
built `target/release/{gx,gx-mcp}` binaries directly.
