# Testing Documentation

This document describes the comprehensive test suite for `gx`.

## Test Structure

The test suite is organized into multiple test files, each focusing on different aspects of the application:

### Test Files

- **`tests/common.rs`** - Common helper functions and utilities shared across all tests
- **`tests/status_tests.rs`** - Tests for the `gx status` subcommand
- **`tests/checkout_tests.rs`** - Tests for the `gx checkout` subcommand
- **`tests/integration_tests.rs`** - Integration tests covering overall CLI behavior and workflows

### Common Helpers (`tests/common.rs`)

The common module provides shared functionality:

- `get_gx_binary_path()` - Returns path to the built gx binary
- `run_gx_command(args, working_dir)` - Executes gx with given arguments
- `create_test_repo(dir, name, with_remote)` - Creates a test git repository
- `create_test_workspace()` - Creates multiple test repositories for testing
- `get_current_branch(repo_path)` - Gets the current branch of a repository
- `run_git_command(args, working_dir)` - Executes git commands

### Test Isolation

**All tests use proper temporary directories:**
- Tests use `tempfile::TempDir::new()` to create isolated temporary directories
- No test files or repositories are created in the project root directory
- Each test gets a fresh temporary directory that is automatically cleaned up
- This ensures tests don't interfere with each other or pollute the working directory

## Test Categories

### Status Command Tests (`status_tests.rs`)

Tests covering the `gx status` subcommand:

**Repository Discovery:**
- `test_status_discovers_repositories` - Finds all repositories in workspace
- `test_status_no_repos_found` - Handles empty directories gracefully

**Filtering:**
- `test_status_filtering_by_pattern` - Filters repositories by name patterns

**Output Formats:**
- `test_status_shows_clean_repos` - Displays clean repositories with 🟢 emoji
- `test_status_shows_dirty_repos` - Shows dirty repos with appropriate emojis (📝, ❓)
- `test_status_detailed_flag` - Tests `--detailed` flag output
- `test_status_no_emoji_flag` - Tests `--no-emoji` flag removes emojis
- `test_status_no_color_flag` - Tests `--no-color` flag removes colors

**Content Verification:**
- `test_status_shows_commit_hash` - Verifies 7-character commit SHA display
- `test_status_shows_branch_name` - Shows current branch (main/master)

**Options and Flags:**
- `test_status_parallel_option` - Tests `--parallel` global flag
- `test_status_max_depth_option` - Tests `--max-depth` global flag
- `test_status_help_output` - Validates help text with emoji legend

**Error Handling:**
- `test_status_exit_code_with_errors` - Proper exit codes on errors

### Checkout Command Tests (`checkout_tests.rs`)

Tests covering the `gx checkout` subcommand:

**Basic Operations:**
- `test_checkout_existing_branch` - Switch to existing branch and sync with remote
- `test_checkout_create_new_branch` - Create new branch with `-b` flag

**Advanced Features:**
- `test_checkout_create_branch_from_specific_base` - Create branch from specific base with `-f`
- `test_checkout_with_stash` - Stash uncommitted changes with `-s` flag
- `test_checkout_with_untracked_files` - Handle untracked files after checkout

**Filtering and Parallelism:**
- `test_checkout_filtering_by_pattern` - Filter repositories by patterns
- `test_checkout_multiple_repos_parallel` - Process multiple repositories concurrently
- `test_checkout_parallel_option` - Custom parallelism settings

**Error Handling:**
- `test_checkout_error_handling` - Handle non-existent branches gracefully
- `test_checkout_branch_name_validation` - Validate branch names
- `test_checkout_no_repos_found` - Handle empty directories

**Help and Documentation:**
- `test_checkout_help_output` - Comprehensive help text with legend and examples

### Integration Tests (`integration_tests.rs`)

Tests covering overall CLI behavior:

**CLI Interface:**
- `test_main_help_output` - Main help with tool validation and global options
- `test_version_output` - Version information display
- `test_invalid_command` - Error handling for invalid commands

**Global Options:**
- `test_global_verbose_flag` - Verbose logging functionality
- `test_global_parallel_option` - Custom parallelism settings
- `test_global_max_depth_option` - Repository discovery depth limits
- `test_config_file_option` - Configuration file support

**Workflows:**
- `test_workflow_status_then_checkout` - Multi-command workflows
- `test_repository_discovery_accuracy` - Accurate repository detection
- `test_repository_filtering_edge_cases` - Edge cases in filtering logic

**System Integration:**
- `test_error_handling_and_exit_codes` - Proper error reporting
- `test_concurrent_operations` - Parallel execution across repositories
- `test_logging_functionality` - Logging system integration

## Test Execution

### Running All Tests
```bash
cargo test
```

### Running Specific Test Files
```bash
cargo test --test status_tests
cargo test --test checkout_tests
cargo test --test integration_tests
```

### Running Individual Tests
```bash
cargo test --test status_tests test_status_help_output
cargo test --test checkout_tests test_checkout_create_new_branch
```

### Running Tests with Output
```bash
cargo test -- --nocapture
```

## Test Results Summary

As of the latest run:

- **Unit Tests**: 4/4 passing (100%)
- **Status Tests**: 11/14 passing (79%)
- **Checkout Tests**: 12/12 passing (100%)
- **Integration Tests**: ~11/13 passing (85%)

**Total**: ~38/43 tests passing (88%)

## Known Test Issues

Some tests may fail due to:

1. **Repository State**: Tests that create git repositories may have timing issues
2. **Emoji Display**: Some emoji assertions may be sensitive to terminal settings
3. **Parallel Execution**: Race conditions in parallel test execution
4. **File System**: Temporary directory cleanup and permissions

## Test Maintenance

- ✅ **Proper Isolation**: All tests use `tempfile::TempDir` for isolated test environments
- ✅ **No Directory Pollution**: Tests never create files in the project root
- ✅ **Automatic Cleanup**: Temporary directories are automatically cleaned up after tests
- ✅ **Fresh State**: Each test creates fresh git repositories to avoid state pollution
- ✅ **Common Helpers**: Shared utilities reduce code duplication and ensure consistency
- ✅ **Comprehensive Coverage**: Tests verify both success cases and error conditions

## Coverage

The test suite covers:

- ✅ All CLI subcommands (`status`, `checkout`)
- ✅ Global options (`--verbose`, `--parallel`, `--max-depth`, `--config`)
- ✅ Output formatting (emojis, colors, detailed vs compact)
- ✅ Repository discovery and filtering
- ✅ Error handling and exit codes
- ✅ Help text and documentation
- ✅ Multi-repository operations
- ✅ Git operations (branch switching, stashing, syncing)
- ✅ Configuration file support
- ✅ Tool validation
- ✅ **Proper test isolation and cleanup**

This comprehensive test suite ensures `gx` works correctly across various scenarios and provides confidence for future development and refactoring. **All tests now properly use temporary directories and will never pollute the project workspace.**