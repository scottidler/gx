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

CLI for git operations across multiple repositories simultaneously. Designed for progressive filtering — start broad, narrow down with each parameter.

## Core Concept: Progressive Filtering

GX operations follow a funnel pattern:

```
All repos in cwd
    ↓ -p pattern      (filter repos by name)
    ↓ --files         (filter files within repos)
    ↓ sub/regex       (filter by content match)
    ↓ --commit        (actually make changes)
    ↓ --pr            (create PRs)
```

**Omit later stages to preview what would be affected.**

## Discovery & Search (Dry-Run Mode)

To find where something exists without making changes, omit `--commit`:

```bash
# Step 1: See which repos have a file
gx create --files 'pyproject.toml'

# Step 2: Narrow to repos matching a pattern
gx create --files 'pyproject.toml' -p python

# Step 3: See which files contain a string (dry-run substitution)
gx create --files 'pyproject.toml' -p python sub '3.9' 'PLACEHOLDER'
# Output shows: "Files scanned: N, Files changed: M" (M = files with matches)

# Step 4: Different file types
gx create --files '.python-version' sub '3.9' 'X'
gx create --files 'Dockerfile' sub 'python3.9' 'X'
gx create --files '*.yaml' sub 'python:3.9' 'X'
```

The Pattern Analysis in output shows:
- `Files scanned` = files matching the --files pattern
- `Files changed` = files that contain the search string
- `Files with no matches` = files scanned but string not found

## Making Changes

Add `--commit` to actually modify files:

```bash
# Substitution across repos
gx create --files 'pyproject.toml' sub '^3.9' '^3.11' --commit "Upgrade to Python 3.11"

# With PR creation
gx create --files 'pyproject.toml' sub '^3.9' '^3.11' \
  --commit "Upgrade to Python 3.11" --pr

# Draft PR
gx create --files '*.md' sub 'old-url' 'new-url' \
  --commit "Update URLs" --pr=draft
```

## Commands Reference

### Status
```bash
gx status                     # All repos
gx status -p frontend         # Repos matching "frontend"
gx status --detailed          # File-by-file details
```

### Clone
```bash
gx clone tatari-tv            # Clone all org repos
gx clone tatari-tv -p python  # Only repos matching "python"
```

### Checkout
```bash
gx checkout main              # Checkout main everywhere
gx checkout -b feature-x      # Create new branch everywhere
gx checkout main -p frontend  # Only in matching repos
```

### Create (Changes & PRs)
```bash
# Preview matches (no action = show repos/files)
gx create --files '*.json'

# Preview substitution (action but no --commit = dry-run)
gx create --files '*.json' sub 'old' 'new'

# Execute (--commit = make changes)
gx create --files '*.json' sub 'old' 'new' --commit "Update config"

# With PR
gx create --files '*.json' sub 'old' 'new' --commit "Update config" --pr
```

Actions: `add <path> <content>`, `delete`, `sub <find> <replace>`, `regex <pattern> <replace>`

### Review (PR Management)
```bash
gx review ls GX-2024-01-15        # List PRs for change-id
gx review approve GX-2024-01-15   # Approve and merge
gx review delete GX-2024-01-15    # Close PRs, delete branches
gx review purge                   # Clean up all GX-* branches
```

### Cleanup
```bash
gx cleanup --list             # Show what needs cleanup
gx cleanup GX-2024-01-15      # Clean specific change
gx cleanup --all              # Clean all merged changes
```

## Key Flags

| Flag | Purpose |
|------|---------|
| `-p, --patterns` | Filter repos by name (can repeat: `-p foo -p bar`) |
| `-f, --files` | File glob pattern within repos |
| `-c, --commit` | Commit message (triggers actual changes) |
| `--pr` | Create PR after commit |
| `-j, --jobs` | Parallel workers |
| `-m, --depth` | Max directory depth to scan |

## Change IDs

GX auto-generates IDs like `GX-2026-01-03T12-30-00`. All branches/PRs for one operation share the same ID for tracking and cleanup.

## Requirements

- `git` >= 2.30
- `gh` CLI (authenticated)
