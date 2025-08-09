# gx Architecture

## Overview

`gx` is a git operations tool designed to execute git commands across multiple repositories concurrently. It is the spiritual successor to `slam`, inheriting its parallel execution patterns and repository discovery mechanisms.

## Key Design Principles

1. **Multi-repo Operations**: All commands operate on multiple repositories simultaneously
2. **Repository Discovery**: Automatically discover git repositories from current working directory downward
3. **Smart Filtering**: Filter repositories using patterns (exact match → starts-with → full slug matching)
4. **Parallel Execution**: Use rayon for concurrent operations across repositories
5. **Concise Output**: Provide clear, actionable output across many repositories
6. **Tool Validation**: Check for required CLI tools and their versions

## Core Architecture Components

### CLI Structure
```
gx [GLOBAL_OPTIONS] <SUBCOMMAND> [SUBCOMMAND_OPTIONS] [REPO_FILTERS...]
```

### Repository Discovery
- Scan current directory and subdirectories for `.git` folders
- Build list of repository paths and derive repo slugs
- Apply filtering based on user-provided patterns

### Parallel Execution
- Use `rayon::par_iter()` for concurrent operations
- Each repository operation runs in parallel thread
- Collect and aggregate results for display

### Required Tools
- `git` (version check)
- `gh` (GitHub CLI, version check)
- Display tool status with checkmarks in `--help`

## Subcommands

### Initial Implementation
1. `clone` - Clone repositories from organization/user
2. `checkout` - Checkout branches across repositories
3. `status` - Show git status across repositories

### Command Pattern
All commands follow the pattern:
1. Discover repositories (or use provided repo list for clone)
2. Filter repositories based on patterns
3. Execute git operations in parallel
4. Aggregate and display results

## Configuration
- YAML configuration file support
- Tool version requirements
- Default organization/user settings