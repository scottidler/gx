# CLI Design

## Help Output Enhancement

The `--help` output should include tool validation status, similar to slam's approach of advertising required tools.

### Enhanced Help Format
```
gx - git operations across multiple repositories

REQUIRED TOOLS:
  ✓ git 2.34.1    (required: >= 2.30)
  ✓ gh 2.40.1     (required: >= 2.0)
  ✗ jq            (required: >= 1.6) - install with: apt install jq

USAGE:
    gx [OPTIONS] <SUBCOMMAND>

SUBCOMMANDS:
    clone       Clone repositories from organization/user
    checkout    Checkout branches across repositories
    status      Show git status across repositories

OPTIONS:
    -c, --config <FILE>    Path to config file
    -v, --verbose          Enable verbose output
    -h, --help             Print help information
    -V, --version          Print version information

EXAMPLES:
    gx status                          # Show status of all repos with changes
    gx clone tatari-tv                 # Clone all Tatari repositories
    gx checkout main frontend          # Checkout main branch in frontend repos

Logs are written to: ~/.local/share/gx/logs/gx.log
```

## Tool Validation

### Required Tools
1. **git** - Core git operations
   - Minimum version: 2.30
   - Check: `git --version`
   - Install instructions per platform

2. **gh** - GitHub CLI for repository discovery and operations
   - Minimum version: 2.0
   - Check: `gh --version`
   - Install instructions per platform

### Validation Implementation
- Check tool availability and versions on startup
- Display status in help output with checkmarks/X marks
- Provide installation instructions for missing tools
- Gracefully handle missing tools with helpful error messages

## Global Options

### Configuration
- `--config, -c`: Path to configuration file
- Default locations: `~/.config/gx/gx.yml`, `./gx.yml`
- Supports configuration via: `gx.yml`, environment variables, and CLI args
- CLI args override env vars, env vars override config file

### Verbosity
- `--verbose, -v`: Enable verbose output
- Controls logging level and progress detail
- Shows individual git command executions in verbose mode

### Standard Options
- `--help, -h`: Enhanced help with tool status
- `--version, -V`: Version from git describe

## Subcommand Structure

### Common Options
All subcommands support:
- Repository filtering via positional arguments
- `--dry-run`: Show what would be done without executing
- `--parallel <N>`: Control parallelism level (default: CPU cores)

### Error Handling
- **Exit code**: Number of failed repositories (0 = success, N = N failures)
- Show error emojis in output for immediate visual feedback
- Detailed error messages logged to file
- **Never stop**: Continue-on-error behavior with final summary
- Heavy use of emojis for status indication (✓❌📝➕❓etc.)