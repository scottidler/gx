# Streaming Status Output with Column Alignment

## Problem Statement

Currently, `gx status` uses a batch output pattern that waits for all repositories to complete before displaying any results, while `gx checkout` uses streaming output that shows results immediately as they complete. Users want both **immediate feedback** (streaming) and **perfect column alignment** (batch).

## Solution: Pre-calculated Alignment with Streaming Display

### Architecture Overview

The solution uses a **two-phase approach**:
1. **Fast Pre-scan Phase**: Quickly determine column widths without expensive git operations
2. **Streaming Display Phase**: Use pre-calculated widths for immediate, perfectly aligned output

### Implementation Design

#### Phase 1: Fast Pre-scan for Column Widths

```rust
fn calculate_alignment_widths_fast(repos: &[Repo]) -> AlignmentWidths {
    // Branch width: Fast git command, no network calls
    let branch_width = repos.par_iter()
        .map(|repo| get_current_branch_name_fast(repo).len())
        .max()
        .unwrap_or(7)
        .max(7); // Minimum readable width

    // SHA width: Always fixed
    let sha_width = 7;

    // Emoji width: Conservative fixed width for all possible combinations
    let emoji_width = 6; // Handles "🔀2↑3↓" (worst case)

    AlignmentWidths { branch_width, sha_width, emoji_width }
}

fn get_current_branch_name_fast(repo: &Repo) -> String {
    // Use existing fast git command (no network, no status parsing)
    Command::new("git")
        .args(["-C", &repo.path.to_string_lossy(), "branch", "--show-current"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}
```

#### Phase 2: Streaming Display with Fixed Widths

```rust
pub fn process_status_command_streaming(
    cli: &Cli,
    config: &Config,
    detailed: bool,
    use_emoji: bool,
    use_colors: bool,
    patterns: &[String],
) -> Result<()> {
    // ... existing discovery and filtering logic ...

    // NEW: Fast pre-scan for alignment widths
    let widths = calculate_alignment_widths_fast(&filtered_repos);

    // NEW: Streaming execution with immediate display
    let results = Mutex::new(Vec::new());

    filtered_repos.par_iter().for_each(|repo| {
        let result = git::get_repo_status(repo);

        // Store for final summary
        if let Ok(mut results_vec) = results.lock() {
            results_vec.push(result.clone());
        }

        // Display immediately with pre-calculated alignment
        if let Err(e) = display_status_result_immediate(&result, &status_opts, &widths) {
            log::error!("Failed to display status result: {}", e);
        }
    });

    // Final summary (existing logic)
    let results_vec = results.into_inner().unwrap_or_default();
    let (clean_count, dirty_count, error_count) = categorize_status_results(&results_vec);
    output::display_unified_summary(clean_count, dirty_count, error_count, &status_opts);

    Ok(())
}
```

#### New Immediate Display Function

```rust
/// Display a single status result immediately with pre-calculated alignment
pub fn display_status_result_immediate(
    result: &RepoStatus,
    opts: &StatusOptions,
    widths: &AlignmentWidths,
) -> Result<()> {
    // Use existing unified formatting with fixed widths
    display_unified_format(result, opts, widths);

    // Ensure immediate visibility
    io::stdout().flush().context("Failed to flush stdout")?;
    Ok(())
}
```

### Performance Analysis

#### Pre-scan Overhead
- **Operation**: `git branch --show-current` per repository
- **Network calls**: None (local git command only)
- **Estimated time**: ~0.5ms per repo = ~100ms for 200 repos
- **User perception**: Effectively instant

#### Streaming Benefits
- **First result visible**: As soon as first repo completes (~50-200ms)
- **Progress feedback**: Continuous visual progress
- **Total time**: Same as batch (no performance penalty)

### Column Width Strategy

#### Branch Width
- **Source**: Fast `git branch --show-current` command
- **Accuracy**: 100% accurate for current branch names
- **Fallback**: "unknown" for detached HEAD or errors

#### SHA Width
- **Source**: Fixed at 7 characters (existing pattern)
- **Accuracy**: 100% (always consistent)

#### Emoji Width
- **Source**: Conservative fixed width
- **Value**: 6 characters (handles worst case: "🔀2↑3↓")
- **Trade-off**: Slight over-allocation vs. perfect efficiency

### Implementation Changes

#### Files to Modify

1. **`src/status.rs`**
   - Replace batch pattern with streaming pattern
   - Add pre-scan phase
   - Use `par_iter().for_each()` instead of `par_iter().map().collect()`

2. **`src/output.rs`**
   - Add `display_status_result_immediate()` function
   - Add `calculate_alignment_widths_fast()` function
   - Add `get_current_branch_name_fast()` helper

3. **`src/git.rs`** (minimal changes)
   - Optionally extract branch name logic for reuse

#### Backward Compatibility
- ✅ Same CLI interface
- ✅ Same output format and alignment
- ✅ Same emoji system
- ✅ Same error handling
- ✅ Only timing of display changes

### Error Handling

#### Pre-scan Failures
- **Branch name errors**: Use "unknown" fallback
- **Width calculation errors**: Use conservative defaults
- **Never block**: Pre-scan failures don't prevent streaming

#### Streaming Display Failures
- **Display errors**: Log but continue processing other repos
- **Flush failures**: Log but don't crash
- **Result collection**: Still works for final summary

### User Experience Impact

#### Before (Batch)
```bash
$ gx status
[2-5 second wait with no output]
   main 7f33680 🟢  tatari-tv/airflow-dags
   main d5f9d7d 🟢  tatari-tv/alertmanager-webhook-logger
   [... all results at once ...]
📊 187 clean, 0 dirty, 0 errors
```

#### After (Streaming)
```bash
$ gx status
[~100ms pre-scan, then immediate results as they complete]
   main 7f33680 🟢  tatari-tv/airflow-dags
   main d5f9d7d 🟢  tatari-tv/alertmanager-webhook-logger
   main 899c3a8 🟢  tatari-tv/apigw-lambdas
   [... results appear in completion order ...]
📊 187 clean, 0 dirty, 0 errors
```

### Configuration Options

#### Future Enhancements
```yaml
# gx.yml
status:
  output_mode: "streaming"  # "streaming" | "batch"
  pre_scan: true           # Enable/disable pre-scan optimization
  emoji_width: 6           # Override conservative emoji width
```

#### CLI Flags
```bash
gx status --batch        # Force batch mode (existing behavior)
gx status --stream       # Force streaming mode (new behavior)
```

### Testing Strategy

#### Unit Tests
- Test `calculate_alignment_widths_fast()` with various repo configurations
- Test `display_status_result_immediate()` output formatting
- Test error handling for pre-scan failures

#### Integration Tests
- Compare streaming vs. batch output for identical repo sets
- Verify alignment consistency across different terminal widths
- Test performance with large repository counts (100+ repos)

#### Manual Testing
- Test with slow repositories (network delays)
- Test with repositories having very long branch names
- Test with mixed emoji combinations

### Migration Plan

#### Phase 1: Implementation
- [ ] Add pre-scan width calculation functions
- [ ] Add immediate display function
- [ ] Modify `process_status_command()` to use streaming pattern
- [ ] Add comprehensive tests

#### Phase 2: Validation
- [ ] Performance testing with large repo sets
- [ ] Visual alignment verification
- [ ] Error handling validation
- [ ] User acceptance testing

#### Phase 3: Rollout
- [ ] Default to streaming behavior
- [ ] Add configuration options
- [ ] Update documentation
- [ ] Monitor for issues

### Success Criteria

- ✅ **Immediate feedback**: First result visible within 200ms
- ✅ **Perfect alignment**: All columns align identically to batch mode
- ✅ **No performance regression**: Total execution time unchanged
- ✅ **Robust error handling**: Graceful degradation on failures
- ✅ **Consistent behavior**: Matches checkout/clone streaming patterns

## Conclusion

This design provides the best of both worlds: immediate user feedback through streaming output while maintaining perfect column alignment through fast pre-scanning. The solution is performant, robust, and maintains full backward compatibility while significantly improving user experience.
