# Testing Infrastructure

## Overview

The `gx` testing infrastructure is designed to support comprehensive multi-repository testing scenarios using isolated temporary environments.

## Key Components

### 1. Isolated Test Environments
- All tests use `tempfile::TempDir` to create isolated temporary directories
- No test pollution or bleed-over between tests
- Automatic cleanup when tests complete

### 2. Test Utilities (`src/test_utils.rs`)

#### Basic Functions
- `get_gx_binary_path()` - Gets path to compiled gx binary for testing
- `run_gx_command(args, working_dir)` - Executes gx commands in test environment
- `run_git_command(args, working_dir)` - Executes git commands in test repos
- `get_current_branch(repo_path)` - Gets current branch name for a repository

#### Repository Creation
- `create_test_repo(base_path, name, with_remote)` - Creates basic test repository
- `create_test_workspace()` - Creates simple workspace with 5 repositories
- `create_comprehensive_test_workspace()` - Creates complex workspace with diverse scenarios
- `create_test_repo_with_branches(base_path, name, remote_slug, branches)` - Creates repo with multiple branches
- `create_test_repo_with_commits(base_path, name, remote_slug, commits)` - Creates repo with commit history

#### GitHub Integration
- `should_run_github_tests()` - Checks if GitHub integration tests should run
- `get_test_github_token()` - Gets GitHub API token for testing
- `create_gx_testing_workspace()` - Creates workspace configured for gx-testing organization

### 3. Test Repository Scenarios

#### Simple Workspace (`create_test_workspace`)
- `frontend` - Basic repository with remote
- `backend` - Basic repository with remote
- `api` - Basic repository with remote
- `docs` - Basic repository with remote
- `dirty-repo` - Repository with uncommitted changes

#### Comprehensive Workspace (`create_comprehensive_test_workspace`)
- `frontend` - Multiple branches (main, develop, feature/auth)
- `backend` - Multiple branches (main, staging)
- `mobile-app` - Multiple branches + untracked files
- `infrastructure` - Multiple branches + staged changes
- `documentation` - Multiple commits with history

All repositories in comprehensive workspace use `gx-testing/*` remote URLs.

## GitHub Integration Testing

gx resolves its GitHub token per-org from environment variables, not a token file: built-in
`tatari-tv` -> `$GITHUB_PAT_WORK`, everything else -> `$GITHUB_PAT_HOME` (override the
whole run with `GH_PERSONA=work|home`, or override a specific org via the optional
`github.token-env` config block naming an env var). The per-org token-FILE scheme this doc
used to describe is retired. See `docs/clone-feature.md#authentication-requirements` for
the full precedence.

### Setup
1. Set `$GITHUB_PAT_HOME` in the shell (decrypted via `manifest age`, per
   `~/repos/.claude/rules/secrets.md`). The `gx-testing` org classifies as home.
2. Authenticate gh CLI, if needed for manual testing: `gh auth login --with-token <<< "$GITHUB_PAT_HOME"`
3. GitHub integration tests are gated on `should_run_github_tests()`, which now checks
   that `$GITHUB_PAT_HOME` is set and non-empty - no token file is read.

### Required Scopes
- `repo` - Full repository access
- `read:org` - Read organization data
- `read:user` - Read user profile data

### Test Configuration
Use `tests/fixtures/gx-testing-config.yml` for GitHub-specific test configuration.

## Test Organization

### Unit Tests (`src/main.rs`, `src/*.rs`)
- Test individual functions and modules
- Use `#[cfg(test)]` modules within source files

### Integration Tests (`tests/*.rs`)
- Test complete workflows and command interactions
- Use `gx::test_utils::*` for shared utilities

### Test Files
- `tests/status_tests.rs` - Tests for `gx status` command
- `tests/checkout_tests.rs` - Tests for `gx checkout` command
- `tests/integration_tests.rs` - General integration and workflow tests
- `tests/comprehensive_tests.rs` - Tests using comprehensive multi-repo scenarios

## Best Practices

### Test Isolation
- Always use `tempfile::TempDir` for test workspaces
- Never create files in the project root during tests
- Each test should be completely independent

### Multi-Repo Testing
- Use `create_comprehensive_test_workspace()` for complex scenarios
- Test filtering, parallel execution, and error handling
- Verify repository state changes after operations

### GitHub Integration
- Guard GitHub tests with `should_run_github_tests()`
- Use `gx-testing` organization for controlled testing
- Test both local git operations and remote GitHub API interactions

## Running Tests

```bash
# Run all tests
cargo test

# Run specific test file
cargo test --test status_tests

# Run with output visible
cargo test -- --nocapture

# Run GitHub integration tests (requires $GITHUB_PAT_HOME set, no token file)
cargo test github_integration_tests

# Run comprehensive multi-repo tests
cargo test --test comprehensive_tests

# Authenticate gh CLI for testing
gh auth login --with-token <<< "$GITHUB_PAT_HOME"
```

## Environment Variables

- `GITHUB_PAT_HOME` - GitHub token for the home persona; required to run GitHub
  integration tests (`should_run_github_tests()` gates on it being set and non-empty).
  `gx-testing` and other non-`tatari-tv` orgs classify as home.
- `GITHUB_PAT_WORK` - GitHub token for the work (`tatari-tv`) persona; not needed for
  the `gx-testing` scenarios above, but resolved the same way for any `tatari-tv/*` repo.
- `GH_PERSONA` - `work`|`home`, forces the persona for the whole test run, overriding the
  per-org classification.
- `RUST_BACKTRACE=1` - Show detailed error traces
- `RUST_LOG=debug` - Enable debug logging during tests
