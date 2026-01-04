# Bug: `gx review ls` Returns 0 Repos When PRs Exist

## Summary

`gx review ls <change-id>` returns 0 repositories even when PRs with matching branch names exist on GitHub.

## Environment

- **Date observed**: 2026-01-03
- **OS**: Linux

## Steps to Reproduce

1. Create PRs using gx:
   ```bash
   cd ~/repos
   gx create --files '.otto.yml' --commit 'chore: remove jobs config' --pr=normal sub 'jobs: 4' ''
   ```

2. Note the change-id from output (e.g., `GX-2026-01-03T20-50-31`)

3. Verify PRs were created:
   ```bash
   gh search prs --owner tatari-tv --head GX-2026-01-03T20-50-31 --state open
   ```
   Output shows 4 PRs exist.

4. Try to list with gx:
   ```bash
   gx review ls GX-2026-01-03T20-50-31
   ```

## Expected Behavior

```
📊 4 repositories processed:
   📋 tatari-tv/auth-svc PR #287
   📋 tatari-tv/media-planning-service PR #769
   📋 tatari-tv/philo-fe PR #8251
   📋 tatari-tv/pre-commit-hooks PR #32
```

## Actual Behavior

```
📊 0 repositories processed:
```

## Evidence PRs Exist

```bash
$ gh search prs --owner tatari-tv --head GX-2026-01-03T20-50-31 --state open --json number,repository,url

[
  {"number":287,"repository":{"nameWithOwner":"tatari-tv/auth-svc"},"url":"https://github.com/tatari-tv/auth-svc/pull/287"},
  {"number":8251,"repository":{"nameWithOwner":"tatari-tv/philo-fe"},"url":"https://github.com/tatari-tv/philo-fe/pull/8251"},
  {"number":32,"repository":{"nameWithOwner":"tatari-tv/pre-commit-hooks"},"url":"https://github.com/tatari-tv/pre-commit-hooks/pull/32"},
  {"number":769,"repository":{"nameWithOwner":"tatari-tv/media-planning-service"},"url":"https://github.com/tatari-tv/media-planning-service/pull/769"}
]
```

## Attempted Workarounds

Tried various invocations, all return 0:

```bash
gx review ls GX-2026-01-03T20-50-31                           # From ~/repos
gx review ls GX-2026-01-03T20-50-31                           # From ~/repos/tatari-tv
gx review --org tatari-tv ls GX-2026-01-03T20-50-31           # Explicit org
```

## Possible Causes

1. **Directory scanning issue**: `gx review ls` may not be scanning the correct directories to find repos
2. **GitHub API query issue**: The query to find PRs by branch name may be malformed
3. **State file missing**: gx may rely on local state files that weren't created properly
4. **Org detection failure**: Auto-detection of organization may be failing silently

## Notes

- `~/.local/share/gx/` directory only contains `logs/`, no `changes/` directory
- PRs were successfully created (confirmed via GitHub)
- Branch names are correct (`GX-2026-01-03T20-50-31`)
- The create command worked, only review ls is broken

