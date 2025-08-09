# Remote Status Feature

## Overview

The `gx status` command now displays the remote tracking status for each repository, showing whether your local branch is up-to-date, ahead, behind, or diverged from its remote tracking branch.

## Remote Status Emojis

### 🟢 Up to Date
- **Meaning**: Local branch is in sync with remote
- **Example**: `gx 📝6 ❓4 🟢 (main)`
- **No-emoji**: `=`

### ⬆️N Ahead
- **Meaning**: Local branch is N commits ahead of remote
- **Example**: `repo ⬆️3 (main)` - 3 commits ahead
- **No-emoji**: `↑3`

### ⬇️N Behind
- **Meaning**: Local branch is N commits behind remote
- **Example**: `okta-yoink ⬇️7 (main)` - 7 commits behind
- **No-emoji**: `↓7`

### 🔀 Diverged
- **Meaning**: Local and remote have diverged
- **Example**: `repo 🔀3↑2↓ (main)` - 3 ahead, 2 behind
- **No-emoji**: `↑3↓2`

### 📍 No Remote
- **Meaning**: No remote tracking branch configured
- **Example**: `local-repo 📍 (main)`
- **No-emoji**: `~`

### ⚠️ Error
- **Meaning**: Error checking remote status
- **Example**: `repo ⚠️ (main)`
- **No-emoji**: `!`

## Usage Examples

### Compact View
```bash
$ gx status
gx 📝6 ❓4 🟢 (main)           # Up to date with remote
okta-yoink 📝1 ❓1 ⬇️7 (main)  # 7 commits behind remote
sieve ❓1 🟢 (main)           # Up to date with remote
```

### Detailed View
```bash
$ gx status --detailed okta-yoink
📁 okta-yoink
  Branch: main
  Remote: ⬇️  Behind by 7 commits
  📝 1 modified
  ❓1 untracked
```

### No-Emoji Mode (CI/Scripts)
```bash
$ gx status --no-emoji
gx M:6 ?:4 = (main)           # Up to date (=)
okta-yoink M:1 ?:1 ↓7 (main)  # 7 behind (↓7)
sieve ?:1 = (main)           # Up to date (=)
```

## Technical Implementation

### Remote Status Detection
1. **Check Upstream Branch**: Uses `git rev-parse --abbrev-ref branch@{upstream}`
2. **Compare Commits**: Uses `git rev-list --left-right --count branch...upstream`
3. **Parse Results**: Determines ahead/behind counts
4. **Handle Errors**: Gracefully handles missing remotes or network issues

### Performance
- **Parallel Execution**: Remote status checked concurrently for all repos
- **Efficient Git Commands**: Minimal git operations per repository
- **Error Resilience**: Continues if some repos fail remote checks

### Emoji Legend
| Status | Emoji | No-Emoji | Meaning |
|--------|-------|----------|---------|
| Up to date | 🟢 | `=` | Local = Remote |
| Ahead | ⬆️N | `↑N` | Local ahead by N |
| Behind | ⬇️N | `↓N` | Local behind by N |
| Diverged | 🔀N↑M↓ | `↑N↓M` | Ahead N, behind M |
| No remote | 📍 | `~` | No tracking branch |
| Error | ⚠️ | `!` | Check failed |

## Benefits

1. **Quick Overview**: See sync status at a glance across all repos
2. **Visual Clarity**: Emoji indicators make status immediately obvious
3. **Detailed Info**: Exact commit counts in detailed view
4. **CI Friendly**: Plain text fallback for scripts
5. **Error Handling**: Clear indication when checks fail

This feature helps maintain awareness of remote sync status across multiple repositories, making it easy to identify which repos need pulling, pushing, or conflict resolution.