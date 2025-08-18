# SLAM Create Command Parity Implementation Plan

## Overview

This document outlines the exact changes needed to implement SLAM-style incremental command building in the GX create command. The goal is to replicate the exact behavior where `gx create -f README.md` shows matched repositories and files without requiring a subcommand.

## Current Problem

**GX Current Behavior:**
```bash
gx create -f README.md
# ERROR: 'gx create' requires a subcommand but one was not provided
```

**SLAM Behavior (Target):**
```bash
slam create -f README.md
# Matched repositories:
#   scottidler/imap-filter
#     README.md
#   scottidler/imap-filter-py
#     README.md
#
#   4📄 | 4🔍
```

## Root Cause Analysis

The fundamental difference is architectural:

- **SLAM**: Uses `action: Option<CreateAction>` (optional subcommands)
- **GX**: Uses `action: CreateAction` (mandatory subcommands)

SLAM's dry-run logic in `process_create_command()`:
```rust
if change.is_none() {  // No subcommand provided
    // Show matches and exit - no actual changes made
    return Ok(());
}
```

## Implementation Plan

### 1. CLI Structure Change (`src/cli.rs`)

**File:** `src/cli.rs`
**Line:** 194

**Change:**
```rust
// FROM:
#[command(subcommand)]
action: CreateAction,

// TO:
#[command(subcommand)]
action: Option<CreateAction>,
```

### 2. Main Handler Logic (`src/main.rs`)

**File:** `src/main.rs`
**Lines:** 93-107

**Replace entire Commands::Create match arm:**
```rust
// FROM:
Commands::Create {
    files,
    change_id,
    patterns,
    commit,
    pr,
    action,
} => {
    let change = match action {
        cli::CreateAction::Add { path, content } => create::Change::Add(path.clone(), content.clone()),
        cli::CreateAction::Delete => create::Change::Delete,
        cli::CreateAction::Sub { pattern, replacement } => create::Change::Sub(pattern.clone(), replacement.clone()),
        cli::CreateAction::Regex { pattern, replacement } => create::Change::Regex(pattern.clone(), replacement.clone()),
    };
    create::process_create_command(cli, config, files, change_id.clone(), patterns, commit.clone(), *pr, change)
}

// TO:
Commands::Create {
    files,
    change_id,
    patterns,
    commit,
    pr,
    action,
} => {
    if action.is_none() {
        create::show_matches(cli, config, files, patterns)
    } else {
        let change = match action.unwrap() {
            cli::CreateAction::Add { path, content } => create::Change::Add(path.clone(), content.clone()),
            cli::CreateAction::Delete => create::Change::Delete,
            cli::CreateAction::Sub { pattern, replacement } => create::Change::Sub(pattern.clone(), replacement.clone()),
            cli::CreateAction::Regex { pattern, replacement } => create::Change::Regex(pattern.clone(), replacement.clone()),
        };
        create::process_create_command(cli, config, files, change_id.clone(), patterns, commit.clone(), *pr, change)
    }
}
```

### 3. New Show Matches Function (`src/create.rs`)

**File:** `src/create.rs`
**Location:** Add after existing imports

**New function:**
```rust
/// Show matched repositories and files without performing any actions (dry-run mode)
pub fn show_matches(
    cli: &Cli,
    config: &Config,
    files: &[String],
    patterns: &[String],
) -> Result<()> {
    let start_dir = cli.cwd.as_ref().unwrap_or(&std::env::current_dir()?);
    let max_depth = cli.max_depth.or_else(|| {
        config.repo_discovery
            .as_ref()
            .and_then(|rd| rd.max_depth)
    }).unwrap_or(3);

    // Discover repositories
    let repos = crate::repo::discover_repos(start_dir, max_depth)
        .context("Failed to discover repositories")?;

    // Filter repositories by patterns
    let filtered_repos = crate::repo::filter_repos(repos, patterns);

    // Count emojis like SLAM
    let total_emoji = "🔍";
    let repos_emoji = "📦";
    let files_emoji = "📄";

    let mut status = Vec::new();
    status.push(format!("{}{}", filtered_repos.len(), total_emoji));

    // Filter repos that have matching files
    let mut matched_repos = Vec::new();
    let mut total_files = 0;

    for repo in filtered_repos {
        let mut matched_files = Vec::new();

        if !files.is_empty() {
            for file_pattern in files {
                if let Ok(files_found) = crate::file::find_files_in_repo(&repo.path, file_pattern) {
                    for file in files_found {
                        matched_files.push(file.display().to_string());
                        total_files += 1;
                    }
                }
            }
            matched_files.sort();
            matched_files.dedup();
        }

        // Include repo if it has matching files OR if no file patterns specified
        if !matched_files.is_empty() || files.is_empty() {
            matched_repos.push((repo, matched_files));
        }
    }

    if !patterns.is_empty() {
        status.push(format!("{}{}", matched_repos.len(), repos_emoji));
    }

    if !files.is_empty() {
        status.push(format!("{}{}", total_files, files_emoji));
    }

    // Display results exactly like SLAM
    if matched_repos.is_empty() {
        println!("No repositories matched your criteria.");
    } else {
        println!("Matched repositories:");
        for (repo, matched_files) in &matched_repos {
            // Show repo slug if available, otherwise repo name
            let display_name = repo.slug.as_ref().unwrap_or(&repo.name);
            println!("  {}", display_name);

            if !files.is_empty() {
                for file in matched_files {
                    println!("    {}", file);
                }
            }
        }

        status.reverse();
        println!("\n  {}", status.join(" | "));
    }

    Ok(())
}
```

### 4. Help Text Updates (`src/cli.rs`)

**File:** `src/cli.rs`
**Location:** Create command help examples

**Add to examples section:**
```rust
EXAMPLES:
  gx create -f '*.json'                                    # Show matching files (dry-run)
  gx create -f '*.json' -p frontend                       # Show matches in frontend repos only
  gx create -f '*.json' add config.json '{"debug": true}' # Actually create files
  gx create -f '*.md' sub 'old-text' 'new-text' --commit 'Update docs'
  gx create -f '*.txt' delete --commit 'Remove old files' --pr
```

### 5. Test Updates Required

**Files to update:**
- Any tests in `tests/` that call `gx create` expecting mandatory subcommands
- Update help text expectations if any tests check for subcommand requirements

## Expected Behavior After Implementation

### Dry-Run Mode (New)
```bash
gx create -f README.md
# Matched repositories:
#   scottidler/imap-filter
#     README.md
#   scottidler/imap-filter-py
#     README.md
#   scottidler/imap-filter-rs
#     README.md
#   scottidler/imap-filter-rs-v2
#     README.md
#
#   4📄 | 4🔍

gx create -f README.md -p frontend
# Shows only frontend repos with README.md files

gx create -f '*.json' -p api
# Shows only api repos with JSON files
```

### Action Mode (Existing - Unchanged)
```bash
gx create -f '*.json' add config.json '{"debug": true}'
# Actually creates files

gx create -f '*.md' sub 'old-text' 'new-text' --commit 'Update docs'
# Actually performs substitution and commits

gx create -f '*.txt' delete --commit 'Remove old files' --pr
# Actually deletes files, commits, and creates PR
```

## Files Modified

1. **`src/cli.rs`** - CLI structure change + help text updates
2. **`src/main.rs`** - Command handler logic modification
3. **`src/create.rs`** - New `show_matches()` function
4. **`tests/*`** - Update any tests expecting mandatory subcommands

## Key Features Replicated from SLAM

- ✅ **Optional subcommands** - `gx create -f pattern` works without action
- ✅ **Incremental discovery** - Shows what would be matched as you build command
- ✅ **Exact output format** - Matches SLAM's repository and file listing
- ✅ **Status emoji counts** - `4📄 | 4🔍` format
- ✅ **Repository filtering** - Works with `-p patterns` flag
- ✅ **File pattern matching** - Works with `-f files` flag
- ✅ **Dry-run first** - Safe preview before taking action
- ✅ **Backward compatibility** - Existing workflows unchanged

## Implementation Notes

- **Zero breaking changes** - All existing `gx create add|delete|sub|regex` commands continue to work exactly as before
- **Additive functionality** - Only adds new dry-run capability
- **Full SLAM parity** - Replicates exact behavior and output format
- **Performance** - Uses existing GX discovery and filtering infrastructure
- **Error handling** - Graceful degradation when repos/files don't exist

## Ready for Implementation

This plan provides exact code changes needed to achieve full SLAM create command parity in GX. All changes are precisely specified with file locations, line numbers, and complete code blocks.
