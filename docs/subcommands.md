# gx Subcommands

## clone

**Purpose**: Clone multiple repositories from a GitHub organization/user

**Usage**:
```
gx clone <org> [repo_patterns...]
```

**Behavior**:
- Requires GitHub organization as first positional argument (e.g., `tatari-tv`)
- Optional repo patterns filter which repos to clone from the org (defaults to `['*']` if omitted)
- Uses `gh repo list <org>` to discover available repositories
- Clones repos in parallel to current directory
- Skips repos that already exist locally

**Examples**:
```bash
gx clone tatari-tv              # Clone all tatari-tv repos
gx clone tatari-tv frontend     # Clone only repos matching "frontend"
gx clone tatari-tv api ui web   # Clone repos matching any of the patterns
```

**Output**: Final summary with success/failure per repo, error emojis for failures

---

## checkout

**Purpose**: Checkout branches across multiple repositories

**Usage**:
```
gx checkout <branch> [repo_patterns...]
gx checkout -b <new_branch> [--from <base_branch>] [repo_patterns...]
```

**Behavior**:
- Discovers git repositories from current directory downward (like slam)
- Filters repositories using provided patterns
- Attempts to checkout specified branch in each repo
- For new branches (`-b`), creates from main/master (whatever HEAD points at) by default
- Use `--from <branch>` to specify different base branch for all repos
- Never stops operation if some repos fail - continues with all others

**Examples**:
```bash
gx checkout main                           # Checkout main in all repos
gx checkout feature/auth frontend          # Checkout branch in repos matching "frontend"
gx checkout -b feature/new                 # Create and checkout new branch from HEAD
gx checkout -b feature/new --from develop  # Create branch from develop in all repos
```

**Output**: Final summary with per-repo status, error emojis for failures

---

## status

**Purpose**: Show git status across multiple repositories

**Usage**:
```
gx status [repo_patterns...]
gx status --detailed [repo_patterns...]
```

**Behavior**:
- Discovers git repositories from current directory downward (like slam)
- Filters repositories using provided patterns
- Runs `git status --porcelain` in parallel
- Compact format by default: one line per repo with reposlug and status emojis
- Use `--detailed` flag for full file-by-file status
- Shows only repos with changes by default, use `--all` to show clean repos too

**Examples**:
```bash
gx status                    # Compact status for repos with changes
gx status --detailed         # Detailed status for repos with changes
gx status --all              # Compact status for all repos
gx status frontend api       # Status for matching repos only
```

**Output**:
- **Compact**: `repo-slug 📝3 ➕2 ❌1` (3 modified, 2 added, 1 deleted)
- **Detailed**: Full git status output per repo

---

## Common Patterns

### Repository Filtering
All commands support filtering repositories using these patterns (in order of precedence):
1. Exact match on repository name (part after `/`)
2. Starts-with match on repository name
3. Exact match on full repo slug (`org/repo`)
4. Starts-with match on full repo slug

### Error Handling
- **Never stop**: Continue processing all repos even if some fail
- Show error emojis (❌) in output for failed operations
- Log detailed errors to log file
- **Exit code**: Number of failed repositories (0 = all success, N = N failures)

### Parallel Execution
- All repo operations run concurrently using rayon
- Final summaries only (no real-time progress bars for now)
- Results aggregated and displayed coherently with heavy emoji usage