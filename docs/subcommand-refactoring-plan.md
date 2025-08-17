# Subcommand Refactoring Plan

## Overview
Refactor gx subcommands from `main.rs` into dedicated `.rs` files for better code organization and maintainability.

## Current State
All subcommand implementations are in `main.rs`:
- `process_status_command()` (lines 91-170)
- `process_checkout_command()` (lines 172-304)
- `process_clone_command()` (lines 306-412)

## Target Structure
```
src/
├── commands/
│   ├── mod.rs         # Module declarations + shared utilities
│   ├── status.rs      # Status subcommand implementation
│   ├── checkout.rs    # Checkout subcommand implementation
│   └── clone.rs       # Clone subcommand implementation
├── main.rs            # Simplified - setup and routing only
├── lib.rs             # Updated - add commands module
└── output.rs          # Unchanged - unified display system
```

## Implementation Steps

### 1. Create Commands Module Structure
- Create `src/commands/mod.rs` with:
  - Module declarations (`pub mod status;`, etc.)
  - Shared utilities (`get_jobs_from_config`, `get_max_depth_from_config`, `get_nproc`)

### 2. Create Individual Subcommand Modules

#### `src/commands/status.rs`
- Move `process_status_command()` function
- Keep all existing logic and error handling
- Use `output::display_status_results()` for terminal output
- Add private helper functions as needed

#### `src/commands/checkout.rs`
- Move `process_checkout_command()` function
- Keep streaming output via `output::display_checkout_result_immediate()`
- Add `display_checkout_summary()` helper function
- Preserve parallel processing and atomic counters

#### `src/commands/clone.rs`
- Move `process_clone_command()` function
- Keep streaming output via `output::display_clone_result_immediate()`
- Add `filter_repository_slugs()` and `display_clone_summary()` helpers
- Preserve GitHub API integration

### 3. Update Module System
- Update `src/lib.rs`: Add `pub mod commands;`
- Update `src/main.rs`:
  - Add `mod commands;`
  - Simplify `run_application()` to route to `commands::*::process_*_command()`
  - Remove all `process_*_command()` functions
  - Remove helper functions (moved to `commands/mod.rs`)

## Key Principles

### Preserve Existing Functionality
- No changes to CLI interface (`cli.rs`)
- No changes to output formatting (`output.rs`)
- No changes to core logic (git operations, config handling)
- All error handling and exit codes remain identical

### Maintain Output Consistency
- All subcommands continue using unified `output.rs` system
- Status: `output::display_status_results()`
- Checkout: `output::display_checkout_result_immediate()`
- Clone: `output::display_clone_result_immediate()`

### Code Organization
- Shared utilities in `commands/mod.rs`
- Each subcommand is self-contained
- Clear module boundaries and dependencies

## Benefits
- **Maintainability**: Isolated subcommand logic
- **Scalability**: Easy to add new subcommands
- **Testing**: Dedicated unit tests per subcommand
- **Readability**: Clean, focused `main.rs`
- **Standards**: Follows Rust project conventions

## Migration Checklist
- [ ] Create `src/commands/mod.rs`
- [ ] Create `src/commands/status.rs`
- [ ] Create `src/commands/checkout.rs`
- [ ] Create `src/commands/clone.rs`
- [ ] Update `src/lib.rs`
- [ ] Simplify `src/main.rs`
- [ ] Run tests to verify functionality
- [ ] Run `whitespace -r` for cleanup
- [ ] Update documentation if needed

## Testing Strategy
1. Run existing test suite after each step
2. Verify all subcommands produce identical output
3. Test error conditions and exit codes
4. Confirm parallel processing still works
5. Validate configuration and CLI parsing

This refactoring maintains 100% backward compatibility while achieving better code organization.
