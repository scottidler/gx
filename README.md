# gx
rust cli for automating activities over 2+ repos at the same time

## `gx status` layout indicator

`gx status` marks bare-container worktrees so a flat clone, a worktree whose
folder still agrees with its checked-out branch, and a worktree whose folder
disagrees are visually distinct - using the same connector-glyph language as
Scott's starship prompt (`custom.path`/`custom.branch`):

```
branch         sha      rmt  repo
               a1b2c3d  🟢   tatari-tv/clyde≡main       folder == branch (branch column blank)
main           e4f5a6b  🟢   scottidler/otto            flat clone (unchanged)
feature-x      9c8d7e0  ↑2   tatari-tv/clyde≈main       folder != branch (real branch shown)
```

- `≡` - the worktree's directory name matches the checked-out branch; the
  branch column is suppressed since the leaf already says it.
- `≈` - the worktree's directory name disagrees with the checked-out branch
  (including a detached `HEAD@<sha>`); the real branch is shown alongside.
- No glyph - a normal flat clone; rendering is unchanged from before.

This is `gx status` only. `gx checkout`/`create`/`review` render exactly as
before - their branch column carries a change ID, not a checked-out branch.

## `gx create ... llm` / `gx apply`

`gx create ... llm "<prompt>"` runs an agent per repo in a throwaway worktree
(never the real one), diffs what it wrote, and rides the same
branch/commit/push/PR pipeline as `sub`/`regex`/`add`/`delete`. One generation
per repo per propose; re-propose to retry.

```bash
gx create -p frontend llm "add error handling to the API client"
```

That one command runs: propose (agent-per-repo, fleet-parallel) -> present
(colored per-repo diff + a proposed/empty/failed summary) -> confirm -> apply.
Nothing lands until you confirm; a bad generation costs nothing (the real
worktree is never touched during propose).

Split the flow with `--propose` to stop after persisting proposals - review
now, apply later:

```bash
gx create -p frontend llm "add error handling to the API client" --propose
gx apply GX-2026-07-12                    # re-presents the diffs, then applies
```

`--propose` IS the dry run for `llm` - there is no apply-then-rollback dance
like a deterministic change; the proposal itself is what you review.

Non-interactive automation needs `--yes` at both steps (propose's blast-radius
confirm and the present/apply confirm are separate gates; each fails closed -
loudly naming `--yes` - rather than silently guessing on a non-interactive
terminal):

```bash
gx create -p frontend --yes llm "..." --propose
gx apply GX-2026-07-12 --yes
```

Configure the agent under `create.llm` in `~/.config/gx/gx.yml` (see the
shipped `gx.yml` for the annotated default). `--permission-mode acceptEdits`
is required: Claude Code in print mode (`-p`) won't edit files without an
edit-granting permission mode, so a bare `claude -p --output-format text`
proposes nothing.

```yaml
create:
  llm:
    agent-command: "claude -p --output-format text --permission-mode acceptEdits"
    timeout-seconds: 300   # wall-clock per repo; on expiry the agent's process group is killed
```
