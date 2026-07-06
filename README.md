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
