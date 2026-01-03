---
name: gx
description: Multi-repo Git operations - bulk changes, PRs, and cleanup across many repositories
allowed-tools: Bash(gx:*), Bash(gh:*)
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

## Syntax Discovery

**ALWAYS run `--help` to discover exact syntax before using any command:**

```bash
gx --help                    # List all commands
gx <command> --help          # Command options (e.g., gx create --help)
gx <command> <action> --help # Action options (e.g., gx create sub --help)
```

The help output includes working EXAMPLES - use those as templates.

## Core Concept: Progressive Filtering

GX operations follow a funnel pattern - start broad, narrow down:

```
All repos in cwd
    ↓ -p pattern      (filter repos by name)
    ↓ --files         (filter files within repos)
    ↓ sub/regex       (filter by content match)
    ↓ --commit        (actually make changes)
    ↓ --pr            (create PRs)
```

**Omit later stages to preview what would be affected.**

## Workflow

### 1. Discovery (Dry-Run)

Find what exists without making changes - omit `--commit`:

```bash
gx create --files 'pyproject.toml'                    # Which repos have this file?
gx create --files 'pyproject.toml' -p python          # Narrow to repos matching "python"
gx create --files 'pyproject.toml' sub '3.9' 'X'      # Which files contain "3.9"?
```

Output shows `Files scanned` vs `Files changed` - use this to refine your scope.

### 2. Execute Changes

Add `--commit` to actually modify files. **Run `gx create --help` for exact flag order.**

### 3. Create PRs

Add `--pr` (or `--pr=draft`) after `--commit` to create pull requests.

### 4. Review & Merge

```bash
gx review ls <change-id>       # List PRs for a change
gx review approve <change-id>  # Approve and merge
```

### 5. Cleanup

```bash
gx cleanup --list              # Show what needs cleanup
gx cleanup <change-id>         # Clean up after merge
```

## Commands Overview

| Command | Purpose |
|---------|---------|
| `status` | Show git status across repos |
| `checkout` | Checkout branches across repos |
| `clone` | Clone org repos |
| `create` | Make changes, create PRs |
| `review` | Manage PRs (list, approve, delete) |
| `cleanup` | Clean up branches after merge |
| `rollback` | Recover from interrupted operations |

Run `gx <command> --help` for each command's options and examples.

## Key Concepts

- **Change IDs**: Auto-generated like `GX-2026-01-03T12-30-00`. All branches/PRs for one operation share the same ID.
- **Dry-run by default**: Without `--commit`, changes are previewed only.
- **Pattern Analysis**: Output shows files scanned, changed, and unmatched.

## Requirements

- `git` >= 2.30
- `gh` CLI (authenticated)
