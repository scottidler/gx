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
default_user_org: "tatari-tv"  # Can be GitHub user or organization
parallelism: "nproc"  # Number of concurrent operations (default: nproc)

# Tool requirements and paths
tools:
  git:
    min_version: "2.30.0"
  gh:
    min_version: "2.0.0"

# Output preferences
output:
  emoji: true          # Use emojis in output
  compact: true        # Compact status format by default
  show_clean: false    # Don't show clean repos by default
  colors: true         # Colorized output

# Repository discovery
repo_discovery:
  max_depth: 10        # Max directory depth to scan
  ignore_patterns:     # Patterns to ignore during discovery
    - "node_modules"
    - ".git"
    - "target"
    - "build"

# Branch operations
branches:
  default_base: "main"  # Default base branch for new branches
  fallback_base: "master"  # Fallback if main doesn't exist

# Logging
logging:
  level: "info"        # debug, info, warn, error
  file: "~/.local/share/gx/logs/gx.log"
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
export GX_PARALLELISM=16  # Override default (nproc)
export GX_OUTPUT_EMOJI=false
export GX_REPO_DEPTH=5
export GX_LOGGING_LEVEL=debug
```

Nested config uses underscore separation:
- `output.emoji` → `GX_OUTPUT_EMOJI`

- `default_user_org` → `GX_USER_ORG`
- `repo_discovery.max_depth` → `GX_REPO_DEPTH`

## CLI Override Examples

```bash
# Override user/org
gx --user-org scottidler clone    # Clone from user scottidler
gx --user-org tatari-tv clone     # Clone from org tatari-tv

# Override parallelism
gx --parallel 16 status

# Override output format
gx --no-emoji --detailed status

# Override config file
gx --config ./custom-gx.yml clone tatari-tv
```

## Configuration Validation

- Validate tool versions on startup
- Check file paths exist and are executable
- Validate numeric ranges (parallelism > 0, max_depth > 0)
- Warn about unknown configuration keys
- Provide helpful error messages for invalid values