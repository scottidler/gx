# Shakedown Report: gx + gx-mcp v0.5.0

Date: 2026-07-12. Binaries: installed `~/.cargo/bin/gx` (reports `gx v0.5.0`)
and `target/release/gx-mcp` (v0.5.0). All fixture work ran under a throwaway
scratch dir with bare-file remotes, a deterministic fake-agent script, and a
`gh` shim. No live LLM, no real GitHub org, nothing under `~/repos` touched.

Verdict: both binaries are solid. Every exercised path behaved as designed.
The `llm` propose/apply round trip, the confirm gates, the failure matrix, and
the full MCP campaign (including the token gate) all passed. Zero product
defects. Three minor cosmetic/UX observations, none blocking.

## Summary

| Metric | Count |
|--------|-------|
| Subcommands discovered | 10 (`status checkout clone create apply review rollback undo cleanup doctor`) + `help` |
| CLI paths exercised | 14 invocation shapes across 7 subcommands |
| CLI paths passed | 14 |
| CLI paths failed (product bug) | 0 |
| CLI paths skipped (need live GitHub) | `checkout`, `clone`, `review {clone,approve,delete,sync,purge}`, `rollback {execute,validate,cleanup}`, `cleanup --all`, live `--pr` |
| Failure-matrix cases | 6 (empty diff, agent nonzero, agent timeout, symlink payload, drift-then-refuse, re-apply idempotency) -- all pass, worktree byte-identical after each |
| Edge cases | 7 (bad change-id, missing manifest, undo missing id, nonexistent pattern, both confirm gates, no-subcommand dry-run) |
| MCP checks | 27 assertions across 2 sessions (default gating + read-only, enabled mutating campaign) -- all pass |
| MCP gating proof | PASS (read-only present, mutating absent by default; enabled config lists all 4) |
| MCP token gate | PASS (wrong AND missing token refused, nothing applied) |
| MCP stdout hygiene | PASS (every stdout line JSON-RPC 2.0; logs file-only, zero stderr) |

## CLI: command tree

| Subcommand | Class | Subcommands / notable flags |
|------------|-------|------------------------------|
| `status` | read-only | `--detailed --no-emoji --no-color -p --fetch-first --no-remote` |
| `checkout` | mutating (local) | `[BRANCH] -b -f -s -p` |
| `clone` | mutating (network) | `<USER\|ORG> --include-archived -p` |
| `create` | mutating | `add delete sub regex llm`; `-f -x -p -c --pr -y` |
| `apply` | mutating | `<CHANGE_ID> --pr -y` (NEW v0.5.0) |
| `review` | mutating (network) | `ls clone approve delete sync purge` |
| `rollback` | recovery | `list execute validate cleanup` |
| `undo` | mutating (network) | `<CHANGE_ID> -o -y` |
| `cleanup` | mutating | `[CHANGE_ID] --all --list --include-remote --force` |
| `doctor` | read-only | `--purge` |

The NEW v0.5.0 surface is `gx create ... llm` (the `llm` create subcommand) and
`gx apply`. Both were exercised thoroughly below.

## `gx create ... llm` -- PASS

One-shot (`gx create -p app --change-id GX-oneshot --yes llm "<prompt>"`)
proposed the fake agent's edit, printed the colored diff, applied in the same
invocation, and pushed the branch to the bare remote with the agent's content:

```
Targeting 1 repositories:
  fleet/app

=== fleet/app ===
diff --git c/data.md i/data.md
index 286bb20..be3c124 100644
--- c/data.md
+++ i/data.md
@@ -1 +1 @@
-old value
+new value

📊 1 repositories: 1 proposed | 0 empty | 0 failed
GX-oneshot ------- 💾 fleet/app

📊 Applied GX-oneshot: 1 applied | 0 drifted/failed (token 7210c47ed7c6304f)
```

`git --git-dir <remote> show refs/heads/GX-oneshot:data.md` -> `new value`.

Split flow (`--propose`, then a separate `gx apply <id> --yes`) produced the
identical end state: the proposal persisted (change state `Proposed`, no branch
on the remote), and apply re-rendered the same diff and pushed the same content.

## `gx apply` -- PASS

`gx apply GX-split --yes` on a persisted proposal re-presented the diff, applied
it, and pushed the branch. Identical output shape to the one-shot's apply block.
`--pr [normal|draft]` is present on the flag surface (not exercised live).

## Confirm gate #5 fails closed -- PASS (both paths)

With `-p app` matching exactly one repo, the up-front blast-radius gate
auto-proceeds under the default `confirm-threshold: 5`, leaving gate #5 as the
one under test. On non-interactive stdin (`</dev/null`) without `--yes`:

- One-shot `gx create ... llm` (no `--yes`): presents the diff, then
  `Error: Application failed: Refusing to apply 1 proposed repositories without
  confirmation on non-interactive stdin; pass --yes to proceed` -- exit 1.
- `gx apply <id>` (no `--yes`): same refusal, exit 1, branch never pushed.

Both name `--yes` and fail closed, matching every other TTY gate in the codebase.

## Failure matrix -- PASS (worktree byte-identical after each)

Each case ran through `--propose` (or `apply` for drift) and asserted the real
worktree HEAD + porcelain + `data.md` bytes were unchanged after the failure.

| Case | Agent / trigger | Result | Exit | Worktree |
|------|-----------------|--------|------|----------|
| Empty diff | `exit 0`, no edit | `0 proposed \| 1 empty \| 0 failed`; no change-state file written | 0 | identical |
| Agent nonzero | `exit 7` | `FAILED fleet/app: agent exited with status 7: boom` | 0 | identical |
| Agent timeout | `sleep 999`, timeout 2s | `FAILED fleet/app: agent timed out after 2s (process group killed)`; returned in ~2s | 0 | identical |
| Symlink payload | `ln -s data.md link.txt` | `FAILED fleet/app: rejected symlink: link.txt` | 0 | identical |
| Drift-then-refuse | commit+push past base, then apply | `❌ fleet/app ... repo drifted since proposal; re-propose (proposal base <X> != current head <Y>)`; `0 applied \| 1 drifted/failed`; state stays `Proposed` with the drift error recorded; branch never pushed | 0 | identical |
| Re-apply idempotency | apply an already-applied id | first apply commits+pushes; second re-presents the diff then `Error: ... no repositories in a Proposed state for GX-re; nothing to apply` | 1 (2nd) | n/a |

Every per-repo failure is a loud per-repo message with process exit 0 (the
design's "loud per-repo error, not a process failure" contract). The timeout's
process-group kill landed right at the 2s deadline.

## `gx undo` -- PASS

Applied campaign (`gx undo GX-undo`, with the `gh` shim on PATH):

- No `--yes`, non-TTY: prints the plan
  (`fleet/app  pushed, no PR  delete remote branch -> delete local branch`) then
  fails closed, exit 1.
- With `--yes`: deletes the branch from the bare remote AND locally, state trued
  up to `Abandoned`. `1 undone, 0 reverted, 0 failed`.

Bare unapplied proposal (`gx undo GX-undobare --yes`, run with `PATH=/usr/bin:/bin`
so no `gh` is reachable): took the local-only arm --
`bare proposal; delete proposal artifacts (local only, no remote)` -- removed
the proposal dir, marked state `Abandoned`, made zero remote/gh calls. This
proves the local-only undo path touches no network.

## `gx doctor` -- PASS (STUCK PROPOSALS section)

After the run, doctor cleanly separated the two proposal-health sections:

```
ORPHANED PROPOSALS:
  GX-empty (no change state)
  GX-nz (no change state)
  GX-to (no change state)
  (run `gx doctor --purge` to remove these via rkvr)

STUCK PROPOSALS (proposed, never applied or undone):
  GX-drift (1 repo(s), updated 2026-07-12T21:39:31...)
  GX-gate1 (1 repo(s), updated 2026-07-12T21:38:48...)
  GX-gate2 (1 repo(s), updated 2026-07-12T21:38:48...)
  (run `gx apply <change-id>` to apply, or `gx undo <change-id>` to discard)
```

ORPHANED = a proposal directory with no change state (the empty/failed/symlink
runs, which never write change state). STUCK = a change in `Proposed` status
never applied or undone (the drift refusal and the two confirm-gate refusals).
A fresh data dir shows `none` for both. `git`/`gh` version checks pass.

## Edge cases -- PASS

| Case | Result | Exit |
|------|--------|------|
| `gx apply bogus-id` (non-`GX-` prefix) | Rejected at clap parse: `change-id must start with 'GX-' (got 'bogus-id')` | 2 |
| `gx apply GX-does-not-exist` | `no proposal to apply ...: expected manifest at <path>/GX-does-not-exist/manifest.json` | 1 |
| `gx undo GX-does-not-exist` | `Nothing to undo for GX-does-not-exist.` | 0 |
| `gx status -p zzzznope` | `🔍 No repositories found matching the criteria` | 0 |
| `gx create --files '*.md' -p app` (no subcommand) | dry-run match listing `data.md`, `1📄 \| 1📦 \| 1🔍` | 0 |
| `gx rollback list` | `📋 No recovery states found` | 0 |
| `gx cleanup --list` | `No changes need cleanup.` | 0 |
| `gx review ls GX-oneshot` (local fixtures, no GitHub) | `📊 0 repositories processed:` | 0 |

## Output-format matrix

The CLI has no `--json`/`--format` flag on any subcommand. Human output is the
only CLI surface, tuned via `status --detailed --no-emoji --no-color`. Machine
JSON is the MCP server's job (see below), where every tool returns a JSON
payload as `result.content[0].text`.

| Surface | Format | Verified |
|---------|--------|----------|
| `gx status` | text; emoji/color toggles | `--no-emoji --no-color` flips glyphs to `=` and strips color |
| `gx <verb>` | text | all exercised verbs |
| `gx-mcp` tools | JSON string in `content[0].text` | parsed with `json.loads` on every call |

## MCP shakedown

Driven by a scripted Python stdio JSON-RPC 2.0 client
(`scratchpad/shakedown-run/mcp_client.py` + `mcp_run.py` + `mcp_campaign.py`),
mirroring `gx-mcp/tests/mcp_tools_test.rs`: `initialize` ->
`notifications/initialized` -> `tools/list` / `tools/call`, reading responses
from stdout. Two sessions: default config (gating + read-only) and a
mutating-enabled config (full campaign).

### Handshake

`initialize` returned `serverInfo {"name":"rmcp","version":"2.2.0"}` and
`capabilities: {tools}`. Both fields present.

### Tool-list gating -- PASS

Default config (no `mcp.tools` block -> category defaults):

```
tools: [change-get, change-list, doctor, repo-discover, review-status, status]
```

All 6 read-only tools present; all 4 mutating tools
(`create-propose create-apply undo-plan undo-execute`) ABSENT. A direct call to
the disabled `create-propose` was refused with `tool not found`.

Enabled config (`mcp.tools` flips all 4 mutating to true): `tools/list` returns
all 10, including the 4 mutating tools.

### Read-only calls -- PASS

Against the 3-repo fixture fleet:

- `repo-discover {patterns: []}` -> `[fleet/api, fleet/app, fleet/web]`.
- `status {patterns: []}` -> per-repo `{slug, branch, clean, remote, error}`
  array, e.g. `{"slug":"fleet/app","branch":"main","clean":true,...}`.
- `change-list {}` -> `[]` (fresh data dir).
- `doctor {}` -> object with `{tools, log_path, failed_recovery,
  orphaned_artifacts, orphaned_proposals, stuck_proposals}`.
- `review-status {change_id}` -> succeeds.

### Token-gated mutating campaign -- PASS

Full round trip against a fresh fixture + `gh` shim, all 5 steps succeeding at
both the protocol level (no error/isError) and the git level:

1. `create-propose {prompt, patterns:[]}` -> `proposed:1 failed:0`, minted
   `change_id` + confirm `token`, per-repo `outcome:"proposed"` with a files
   summary (no full diff).
2. `change-get {change_id}` -> the full per-repo unified diff (showed the
   `new value` hunk that the propose summary omits).
3. `create-apply {change_id, token}` -> `applied:1 drifted_or_failed:0`,
   repo `status:"Committed"`, `pr_url:null` (MCP create-apply never opens a PR),
   branch landed on the bare remote with `data.md` = `new value`.
4. `undo-plan {change_id}` -> reconciled against the shim, minted an undo token,
   `actionable:1`, plan action `DeleteRemoteAndLocal`.
5. `undo-execute {change_id, token}` -> `outcome:"Undone"`, branch gone from the
   remote, state `Abandoned`, proposal artifacts removed.

### Token gate -- PASS

Before the correct-token apply, two refusals proved the gate fails closed:

- Wrong token (`0000000000000000`): refused with
  `confirm token mismatch for <id>: the proposal changed since it was presented
  (expected <token>); re-present and apply`. No branch pushed.
- Missing token (empty string): same refusal, cites the token.

The gate prevents executing a STALE plan; the token travels
create-propose -> create-apply.

### stdout hygiene -- PASS

Every non-empty stdout line across both full sessions parsed as JSON-RPC 2.0
(the harness asserts this on every `readline`). The server produced ZERO stderr
output. Logs went to the file only:
`<XDG_DATA_HOME>/gx/logs/gx-mcp.log` (21 lines: init, per-tool INFO lines, the
`gx::github` remote-branch-delete, shutdown). File-only logging confirmed.

## Failures & bugs

None (product). All 14 CLI paths, 6 failure-matrix cases, 7 edge cases, and 27
MCP assertions passed.

## Observations / UX notes

1. **Applied proposals retain their proposal directory.** After a successful
   `apply`/one-shot (`GX-oneshot`, `GX-split`, `GX-re`), the proposal dir (blobs
   + manifest) stays under `<data>/gx/proposals/<id>`. Doctor correctly does NOT
   flag these (they have valid `Committed` state, so they are neither orphaned
   nor stuck), and `undo` cleans them. This is retention-by-design so the patch
   survives for undo, but the blobs linger until an undo or a manual cleanup.
   Worth confirming there is a reap path for applied-and-merged campaigns
   (`review sync` + `cleanup`), or they accumulate.

2. **`undo` uses the ❌ glyph for a successfully-undone repo.** The success line
   reads `UNDO ------- ❌ fleet/app` above `1 undone, 0 reverted, 0 failed`. Per
   the legend ❌ means "PR closed and branch deleted", so it is the documented
   glyph, but next to a success summary it reads as failure. Minor dissonance.

3. **`--propose` on an all-failed/all-empty manifest still prints
   `Run gx apply <id> to apply`.** For `GX-nz`, `GX-to`, `GX-empty`,
   `GX-symlink` nothing was proposed, yet the trailer suggests applying.
   Harmless (a later `apply` reports "nothing to apply"), but slightly
   misleading. Already noted in the prior `shakedown-llm-mcp.md`.

## Not exercised (needs live infra)

Live GitHub verbs: `clone`, `checkout` (local but not part of the llm surface),
`review {clone,approve,delete,sync,purge}`, `cleanup --all/--include-remote`,
and live `--pr` PR creation on both `create` and `apply`. The `rollback
execute/validate/cleanup` recovery paths (no interrupted transactions to
recover in this pass). A real registered MCP client (Claude Desktop / Claude
Code) driving `gx-mcp` interactively -- this pass used a scripted JSON-RPC
client, not a live client session.
