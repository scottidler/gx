# gx-mcp

An [MCP](https://modelcontextprotocol.io) stdio server that fronts gx's cores so
an agent can drive fleet campaigns over the protocol instead of screen-scraping
the CLI. Part of the gx product (design doc:
`docs/design/2026-07-12-llm-propose-apply-and-mcp-server.md`, Chunk B).

## Transport and logging

- **stdio only.** The client spawns the process; stdin/stdout carry JSON-RPC.
- **Logging is file-only** at `<XDG_DATA_HOME>/gx/logs/gx-mcp.log` (the shared
  `gx` XDG segment, alongside `gx.log`). Nothing is ever written to stdout or
  stderr outside the transport -- a stray byte would corrupt the protocol.

## Tool surface

Read-only (default **enabled**):

| tool | input | returns |
|------|-------|---------|
| `status` | `{ patterns[], fetch_remote? }` | per-repo git status (local unless `fetch_remote`) |
| `repo-discover` | `{ patterns[] }` | matched repo slugs + paths |
| `change-list` | `{}` | every persisted change + aggregate status |
| `change-get` | `{ change_id, slug? }` | one change's per-repo state + **full** proposal diffs |
| `review-status` | `{}` | PR-bearing changes and each repo's PR state |
| `doctor` | `{}` | tool versions, orphaned/stuck artifacts |

Mutating (default **disabled**):

| tool | input | returns |
|------|-------|---------|
| `create-propose` | `{ prompt, patterns[] }` | `{ change_id, token, repos:[{slug, outcome, files, diff-stat}] }` -- per-repo **summaries**, never full diffs (fleet-sized diffs blow the protocol limit; use `change-get` for one repo's full diff) |
| `create-apply` | `{ change_id, token }` | per-repo apply status + PR url |
| `undo-plan` | `{ change_id }` | `{ token, plan:[...] }` |
| `undo-execute` | `{ change_id, token }` | per-repo undo outcomes |

Deliberately **absent**: rollback execute (recovery repair stays a human
surface), review purge, cleanup.

## Config gating

Every tool has an `enabled:` flag under `mcp.tools` in `~/.config/gx/gx.yml`
(shared with the gx CLI). Read-only tools default `true`, mutating tools default
`false`. A **disabled** tool is absent from `tools/list` and its call is rejected
-- writes are impossible by default.

```yaml
mcp:
  tools:
    status: true
    create-propose: false   # flip to true only for a client you trust
```

## Confirm-token protocol

Every mutation is two steps. A plan/propose step returns a **token** bound to
the exact bytes it produced; the execute step must send that token back:

- `create-propose` mints the token over the canonical `manifest.json` (which
  carries every blob's sha256). `create-apply` demands it and refuses on
  mismatch; apply also re-verifies each blob's hash under lock before writing.
- The undo plan is not persisted: `undo-plan` computes a token over the
  reconciled plan; `undo-execute` recomputes it and refuses if the plan changed
  (state moved between planning and executing).

## Trust model (read this before enabling a mutating tool)

The confirm token proves the caller **received** the exact plan/proposal bytes.
It does **not** prove a human reviewed them, and stdio-local (the client spawned
this process) is the only caller authentication there is.

**Enabling a mutating MCP tool grants that client the same authority as a shell
running `gx ... --yes`.** An agent can call `create-propose` then `create-apply`
back to back with no human in the loop. The token prevents executing a **stale**
plan (one whose bytes changed since it was shown), not an **unreviewed** one.

That is an accepted property of a local, single-user tool -- not a bug -- but it
is the reason mutating tools are disabled by default. Enable one only for a
client you would hand a `--yes` shell.

## Registering with a client

Registration is a manual operator step. Point your MCP client's config at the
`gx-mcp` binary (built/installed from this workspace) as a stdio server; it
inherits your ambient environment (git/gh credentials, `~/.config/gx/gx.yml`).
