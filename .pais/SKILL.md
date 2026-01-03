---
name: gx
description: Multi-repo Git operations - bulk changes, PRs, and cleanup across many repositories
tier: 1
triggers:
  - multi-repo
  - bulk change
  - mass update
  - N repos
  - many repos
  - across repos
  - gx
---

# GX

CLI for git operations across multiple repositories simultaneously.

## When to Use

Use `gx` when the user needs to:
- Make the same change across multiple repos (file updates, substitutions)
- Create PRs in bulk with a single change-id
- Review/approve/merge PRs across repos
- Clean up branches after bulk operations
- Check status of many repos at once

## Commands

### Status & Discovery

```bash
gx status                          # Status across all discovered repos
gx status -p frontend              # Filter by pattern
gx clone tatari-tv                 # Clone all repos from an org
```

### Bulk Changes

```bash
# Dry-run (preview matches)
gx create --files '*.json' -p myrepo

# String substitution + commit + PR
gx create --files 'Cargo.toml' sub 'version = "1.0"' 'version = "1.1"' \
  --commit "Bump version" --pr

# Regex substitution
gx create --files '*.md' regex 'v\d+\.\d+' 'v2.0' --commit "Update versions" --pr

# Add new file to repos
gx create --files '*.toml' add .github/CODEOWNERS 'content here' --commit "Add CODEOWNERS" --pr

# Delete files
gx create --files 'legacy.txt' delete --commit "Remove legacy" --pr
```

### PR Management

```bash
gx review ls GX-2024-01-15              # List PRs for a change-id
gx review approve GX-2024-01-15         # Approve and merge all
gx review approve GX-2024-01-15 --admin # Admin merge (bypass checks)
gx review delete GX-2024-01-15          # Close PRs, delete branches
gx review purge                         # Clean up all GX-* branches
```

### Cleanup

```bash
gx cleanup --list                  # Show what needs cleanup
gx cleanup GX-2024-01-15           # Clean up specific change
gx cleanup --all                   # Clean up all merged changes
```

## Change IDs

GX auto-generates change IDs like `GX-2024-01-15T10-30-00`. All branches and PRs for a bulk operation share the same change-id, making them easy to track and clean up.

## State Tracking

GX tracks operations in `~/.gx/changes/`. This enables:
- Knowing which repos were modified
- Which PRs are open/merged/closed
- Automatic cleanup of local branches after merge

## Important Notes

- Always do a dry-run first (omit `--commit`) to preview changes
- The `--pr` flag requires `--commit`
- Use `--pr=draft` for draft PRs
- GX requires `gh` CLI to be authenticated

