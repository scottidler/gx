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

## create

**Purpose**: Apply file changes across multiple repositories, optionally commit and open PRs

**Usage**:
```
gx create --files <pattern> [--commit <msg>] [--pr [--draft]] [--yes] [--report <path>] <action>
```

**Behavior**:
- Discovers matching files per repo, applies the requested action (`add`/`delete`/`sub`/`regex`), diffs, and (with `--commit`) commits + optionally opens a PR
- `--pr` is a boolean flag (no value) that opens a PR per repo after committing; `--pr --draft` opens draft PRs instead of normal ones. `--draft` REQUIRES `--pr` -- a bare `--draft` with no `--pr` is a clap error, not a silent no-op. (The old `--pr=normal`/`--pr=draft`/`--pr <value>` form is gone -- it let clap swallow the next token as the flag's value, which misparsed `--pr regex ...` as an unrecognized subcommand.)
- **Exits non-zero when any repo fails** (matches `status`/`checkout`/`clone`): the exit code is the failed-repo count, so a script can gate on it directly
- `--yes` skips the confirm prompt above `create.confirm-threshold` (default 5); required on non-interactive stdin or the run fails closed naming `--yes`
- `--report <path>` writes a JSON array of `{repo, phase, error}` for every FAILED repo to that file; on-screen output is unchanged (human summary stays human, the file is the scriptable surface). An all-success run writes `[]`
- Every `git`/`gh` subprocess this command spawns is wall-clock-bounded by `subprocess-timeout-secs` (default 300s); a hung op is killed and reported as that repo's error, the run still reaches its summary

**Examples**:
```bash
gx create --files '*.md' --commit 'Update docs' sub 'old-text' 'new-text'
gx create --files '*.json' --commit 'Bump version' --pr regex '"version": "[^"]+"' '"version": "1.2.3"'
gx create --files '*.md' --commit 'Draft update' --pr --draft sub 'old-text' 'new-text'
gx create --files '*.txt' --commit 'Remove old files' --yes --report /tmp/failures.json delete
```

---

## apply

**Purpose**: Apply a persisted `llm` proposal (from `gx create ... llm "<prompt>" --propose`), re-presenting the diffs and running the same confirm gate as the one-shot `create ... llm` flow

**Usage**:
```
gx apply <change-id> [--pr [--draft]] [--yes]
```

**Behavior**:
- Re-presents the diffs recorded for `<change-id>`'s persisted proposal, then applies them across the targeted repos
- `--pr` is a boolean flag (no value) that opens a PR per repo after committing; `--pr --draft` opens draft PRs instead of normal ones. `--draft` REQUIRES `--pr` -- a bare `--draft` with no `--pr` is a clap error, not a silent no-op
- `--yes` skips the confirmation prompt before applying; required on non-interactive stdin or the run fails closed naming `--yes`

**Examples**:
```bash
gx apply GX-2026-07-12              # re-present diffs, confirm, then apply
gx apply GX-2026-07-12 --yes        # apply without the confirmation prompt
gx apply GX-2026-07-12 --pr         # apply and open a PR per repo
gx apply GX-2026-07-12 --pr --draft # apply and open a DRAFT PR per repo
```

---

## review

**Purpose**: Manage PRs opened by `gx create` across multiple repositories, by change ID

**Usage**:
```
gx review approve <change-id> [--admin] [--auto] [--yes]
gx review delete <change-id> [--yes]
gx review sync <change-id>
```

**Behavior**:
- `approve`/`delete` resolve every targeted org's PRs FIRST; if ANY org's discovery errors, the WHOLE batch aborts (naming the failed org) before a single GitHub write -- no partial merge/delete on a partial view
- `approve` only merges a PR GitHub reports `mergeable: MERGEABLE`; a `CONFLICTING` or lazily-computed `UNKNOWN` PR is SKIPPED (recorded distinctly, not merged) with a re-run hint in the summary -- gx never merges on uncertainty
- `--admin` bypasses branch protection on merge. Since GitHub always rejects self-approval, `--admin` SKIPS the `gh pr review --approve` step entirely and merges straight through; without `--admin`, a failed approve aborts that PR's merge
- `delete` CLOSES open (unmerged) PRs and DELETES their branches -- its consent prompt states that destruction explicitly ("will CLOSE N open (unmerged) PR(s) and DELETE their branches")
- Both `approve` and `delete` prompt for confirmation once the affected count reaches `review.confirm-threshold` (default 5); `--yes` bypasses it and is REQUIRED on non-interactive stdin (fails closed naming `--yes` otherwise, with ZERO mutations)

**Examples**:
```bash
gx review approve GX-2026-07-12 --admin --yes   # merge own campaign, bypass branch protection
gx review delete GX-2026-07-12                  # close + delete unmerged PRs (prompts above threshold)
gx review sync GX-2026-07-12                    # true-up recorded state against GitHub reality
```

---

## cleanup

**Purpose**: Force-delete local `gx`-created branches once their change has landed

**Usage**:
```
gx cleanup <change-id> [--force] [--include-remote] [--yes]
gx cleanup --all [--yes]
gx cleanup --list
```

**Behavior**:
- Before `git branch -D`, fetches `origin` and proves the branch with `git merge-base --is-ancestor <branch> origin/<base>` against the FRESHLY-fetched base ref -- the recorded `PrMerged` status is a fast-path signal only; the fetched-ancestry check is the real guard. A branch that fails the check is preserved and reported, not deleted, unless `--force`
- Prompts for confirmation once the eligible-for-deletion branch count reaches `cleanup.confirm-threshold` (default 5); `--yes` bypasses it and is REQUIRED on non-interactive stdin (fails closed naming `--yes`, ZERO branches deleted)
- `--force` also deletes branches whose PR status is unknown, bypassing the fast-path signal (the ancestry check still runs)

**Examples**:
```bash
gx cleanup GX-2026-07-12              # clean up one change's local branch
gx cleanup --all --yes                # clean up every merged change, no prompt
gx cleanup --list                     # list what's eligible, delete nothing
```

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