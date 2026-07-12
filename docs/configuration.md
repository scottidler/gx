# Configuration Design

## Configuration Sources

Configuration is loaded in this precedence order (highest to lowest):
1. **CLI Arguments** - Override everything
2. **Environment Variables** - Override config file
3. **Config File** - Base configuration

## Config File Locations

Default search order:
1. `~/.config/gx/gx.yml` (primary location)
2. `./gx.yml` (project-local config)
3. Path specified by `--config` flag

## Configuration Schema

```yaml
# gx.yml
default-user-org: "tatari-tv"  # Can be GitHub user or organization
jobs: "nproc"  # Number of concurrent operations (default: nproc)

# Repository discovery
repo-discovery:
  max-depth: 10        # Max directory depth to scan
  ignore-patterns:     # Patterns to ignore during discovery
    - "node_modules"
    - ".git"
    - "target"
    - "build"

# `gx create` settings (optional)
create:
  confirm-threshold: 5   # Prompt before committing to more than this many repos
  llm:                   # `gx create ... llm "<prompt>"` (agent-per-repo propose/apply)
    # The prompt is appended as the final argument; CWD is a throwaway worktree.
    # `--permission-mode acceptEdits` is REQUIRED: in print (-p) mode Claude Code
    # will not edit files without an edit-granting permission mode, so a bare
    # `claude -p --output-format text` proposes nothing.
    agent-command: "claude -p --output-format text --permission-mode acceptEdits"
    timeout-seconds: 300  # Wall-clock per repo; on expiry the agent's process group is killed

# Output preferences (optional)
output:
  verbosity: summary   # Output verbosity: compact, summary, detailed, or full (default: summary)

# Logging
logging:
  level: "info"        # debug, info, warn, error
  file: "~/.local/share/gx/logs/gx.log"

# `gx-mcp` MCP server tool gating (optional). Read-only tools default ENABLED,
# mutating tools default DISABLED, so writes are impossible by default even with
# this block absent. Enabling a mutating tool grants that MCP client the same
# authority as a shell running `gx ... --yes`; the confirm token only prevents
# executing a STALE plan, not an unreviewed one.
mcp:
  tools:
    status: true          # read-only, default true
    repo-discover: true
    change-list: true
    change-get: true
    review-status: true
    doctor: true
    create-propose: false # mutating, default false
    create-apply: false
    undo-plan: false
    undo-execute: false
```

## User vs Organization Support

The `--user-org` flag and `default_user_org` config support both:
- **GitHub Users**: Individual user accounts (e.g., `scottidler`)
- **GitHub Organizations**: Company/team accounts (e.g., `tatari-tv`)

Examples:
```bash
gx clone scottidler        # Clone from user scottidler's repos
gx clone tatari-tv         # Clone from tatari-tv organization
gx --user-org scottidler clone frontend  # Override to user repos
```

## Environment Variables

All config options can be set via environment variables using `GX_` prefix:

```bash
export GX_USER_ORG="tatari-tv"
export GX_JOBS=16  # Override default (nproc)
export GX_OUTPUT_VERBOSITY=detailed
export GX_REPO_DEPTH=5
export GX_LOGGING_LEVEL=debug
```

Nested config uses underscore separation:
- `output.verbosity` → `GX_OUTPUT_VERBOSITY`
- `default-user-org` → `GX_USER_ORG`
- `repo-discovery.max-depth` → `GX_REPO_DEPTH`

## CLI Override Examples

```bash
# Override user/org
gx --user-org scottidler clone    # Clone from user scottidler
gx --user-org tatari-tv clone     # Clone from org tatari-tv

# Override jobs
gx --jobs 16 status

# Override output format
gx --no-emoji --detailed status

# Override config file
gx --config ./custom-gx.yml clone tatari-tv
```

## Configuration Validation

- Validate tool versions on startup
- Check file paths exist and are executable
- Validate numeric ranges (jobs > 0, max_depth > 0)
- Reject unknown configuration keys with a loud, named error
  (`#[serde(deny_unknown_fields)]`) rather than silently ignoring a typo
- Provide helpful error messages for invalid values