//! CLI wrapper for `gx create`: discovers/filters repos, shows the blast
//! radius and prompts (or honors `--yes`), then calls into
//! [`core::execute_create`] and prints the results. All terminal output
//! (`println!`/`print!`) lives here; the core never prints or prompts (design
//! doc `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 3).

mod core;

pub use core::{generate_change_id, Change, CreateAction, CreateResult};
// Re-exported so the proposal-artifact retention callers outside `create`
// (`gx undo`'s local-only Proposed arm, `gx cleanup`, `gx doctor`) can reach
// the manifest layout/removal helpers through a stable `crate::create::manifest`
// path even though `core` itself is private.
pub use core::manifest;

use crate::cli::Cli;
use crate::config::Config;
use crate::file;
use crate::output::{display_unified_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use eyre::{Context, Result};
use log::debug;

/// Show matched repositories and files without performing any actions (dry-run mode)
pub fn show_matches(
    cli: &Cli,
    config: &Config,
    files: &[String],
    patterns: &[String],
) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_ref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    // Discover repositories
    let repos = discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    // Filter repositories by patterns
    let filtered_repos = filter_repos(repos, patterns);

    // Count emojis like SLAM
    let total_emoji = "🔍";
    let repos_emoji = "📦";
    let files_emoji = "📄";

    let mut status = Vec::new();
    status.push(format!("{}{}", filtered_repos.len(), total_emoji));

    // Filter repos that have matching files
    let mut matched_repos = Vec::new();
    let mut total_files = 0;

    for repo in filtered_repos {
        let mut matched_files = Vec::new();

        if !files.is_empty() {
            if let Ok(files_found) = file::FileSet::matching_any(&repo.path, files) {
                for file in files_found {
                    matched_files.push(file.display().to_string());
                    total_files += 1;
                }
            }
            matched_files.sort();
            matched_files.dedup();
        }

        // Include repo if it has matching files OR if no file patterns specified
        if !matched_files.is_empty() || files.is_empty() {
            matched_repos.push((repo, matched_files));
        }
    }

    if !patterns.is_empty() {
        status.push(format!("{}{}", matched_repos.len(), repos_emoji));
    }

    if !files.is_empty() {
        status.push(format!("{total_files}{files_emoji}"));
    }

    // Display results exactly like SLAM
    if matched_repos.is_empty() {
        println!("No repositories matched your criteria.");
    } else {
        println!("Matched repositories:");
        for (repo, matched_files) in &matched_repos {
            // Show repo slug if available, otherwise repo name
            let display_name = &repo.slug;
            println!("  {display_name}");

            if !files.is_empty() {
                for file in matched_files {
                    println!("    {file}");
                }
            }
        }

        status.reverse();
        println!("\n  {}", status.join(" | "));
    }

    Ok(())
}

/// Process create command across multiple repositories: discover/filter,
/// confirm the blast radius, run the core, and display the results.
#[allow(clippy::too_many_arguments)]
pub fn process_create_command(
    cli: &Cli,
    config: &Config,
    files: &[String],
    change_id: Option<String>,
    patterns: &[String],
    commit_message: Option<String>,
    pr: Option<crate::cli::PR>,
    yes: bool,
    change: Change,
) -> Result<()> {
    log::info!("Starting create command with change: {change:?}");

    // Forward hook (design doc `2026-07-12-llm-propose-apply-and-mcp-server.md`):
    // the `gx create ... llm "<prompt>"` subcommand lands in Phase 6. Until then
    // this inert-unless-set env var is the ONLY non-test site that selects
    // `Change::Llm`, giving the propose pass a real (non-test) caller so the bin
    // target's dead-code `-D warnings` passes honestly - the same established
    // pattern as `confirm::already_confirmed` (GX_TEST_CONFIRM_TOKEN) and
    // GX_CRASH_POINT / GX_TEST_LOCK_DELAY_MS.
    // Forward hook for `gx apply <change-id>` (Phase 6 wires the real clap verb
    // on top of `process_apply_command`). Inert unless `GX_LLM_APPLY_CHANGE_ID`
    // is set - the same test-drivable, dead-code-honest pattern as the propose
    // hook below. Checked first: apply and propose are mutually exclusive.
    if let Some(apply_id) = llm_apply_change_id() {
        return process_apply_command(cli, config, &apply_id);
    }

    let change = match llm_override_prompt() {
        Some(prompt) => Change::Llm(prompt),
        None => change,
    };

    // The `llm` change is a fleet-level propose pass, not the per-repo commit
    // pipeline; present + confirm + apply are Phases 5-6. Here we propose only.
    if let Change::Llm(prompt) = &change {
        return run_llm_propose(cli, config, patterns, change_id, prompt);
    }

    let change_id = change_id.unwrap_or_else(generate_change_id);
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    // Discover and filter repositories
    let repos = discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;

    log::info!("Discovered {} repositories", repos.len());

    let filtered_repos = filter_repos(repos, patterns);
    log::info!(
        "Filtered to {} repositories matching patterns",
        filtered_repos.len()
    );

    if filtered_repos.is_empty() {
        println!("No repositories found matching the specified patterns.");
        return Ok(());
    }

    // Confirmation gate: in commit mode, show the blast radius and (unless --yes)
    // prompt before mutating. Always prompt when no -p patterns were given; for
    // patterned runs, prompt only when the repo count exceeds the threshold ([A9]).
    if commit_message.is_some() {
        let threshold = config.confirm_threshold();
        let needs_prompt = patterns.is_empty() || filtered_repos.len() > threshold;
        if !confirm_blast_radius(&filtered_repos, patterns, needs_prompt, yes)? {
            println!("Aborted; no changes made.");
            return Ok(());
        }
    }

    // Determine parallelism
    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // The wrapper already confirmed (TTY prompt above, or --yes); the core
    // never prompts, so it always receives an already-satisfied confirmation.
    let results = core::execute_create(
        &filtered_repos,
        &change_id,
        files,
        &change,
        commit_message.as_deref(),
        pr.as_ref(),
        config,
        parallel_jobs,
        crate::confirm::already_confirmed(),
    )?;
    log::debug!(
        "process_create_command: {} of {} results carry a diff",
        results.iter().filter(|r| r.diff.is_some()).count(),
        results.len()
    );

    // Display results
    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };

    display_unified_results(&results, &opts);
    display_create_summary(&results, &opts);

    Ok(())
}

/// The inert forward hook that selects `Change::Llm` before the CLI `llm`
/// subcommand exists (Phase 6). Returns the prompt only when
/// `GX_LLM_PROPOSE_PROMPT` is set non-empty; unset (the normal case) leaves the
/// deterministic change untouched.
fn llm_override_prompt() -> Option<String> {
    match std::env::var("GX_LLM_PROPOSE_PROMPT") {
        Ok(p) if !p.is_empty() => Some(p),
        _ => None,
    }
}

/// The inert forward hook that selects `gx apply <change-id>` before the CLI
/// `apply` verb exists (Phase 6). Returns the change-id only when
/// `GX_LLM_APPLY_CHANGE_ID` is set non-empty.
fn llm_apply_change_id() -> Option<String> {
    match std::env::var("GX_LLM_APPLY_CHANGE_ID") {
        Ok(id) if !id.is_empty() => Some(id),
        _ => None,
    }
}

/// Apply a persisted proposal set (`gx apply <change-id>`): drive the core apply
/// pass and print a per-fleet summary. This is the CLI seam Phase 6 refines with
/// the present step (re-render each repo's diff) + confirm gate #5; the core
/// [`core::apply::execute_apply`] already owns the ChangeLock, drift/tamper
/// refusals, the unchanged branch/commit/push/PR pipeline, and the partial-apply
/// state reconciliation (drifted/failed repos stay `Proposed`).
pub fn process_apply_command(cli: &Cli, config: &Config, change_id: &str) -> Result<()> {
    log::info!("Starting apply for change ID: {change_id}");

    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // The wrapper's TTY present + confirm gate is Phase 6; the core already
    // confirmed by another means here (Token via GX_TEST_CONFIRM_TOKEN, else
    // AlreadyConfirmed), and re-verifies any supplied token against the manifest.
    let report = core::apply::execute_apply(
        change_id,
        None, // commit message: the core falls back to the recorded prompt
        None, // PR creation is a Phase 6 flag; apply pushes branches by default
        config,
        parallel_jobs,
        crate::confirm::already_confirmed(),
    )?;

    let opts = StatusOptions {
        verbosity: if cli.verbose {
            crate::config::OutputVerbosity::Detailed
        } else {
            crate::config::OutputVerbosity::Summary
        },
        use_emoji: true,
        use_colors: true,
    };
    display_unified_results(&report.results, &opts);
    println!(
        "\n📊 Applied {}: {} applied | {} drifted/failed (token {})",
        report.change_id, report.applied, report.drifted_or_failed, report.token
    );
    Ok(())
}

/// Run the `llm` PROPOSE pass over the discovered/filtered repos and print a
/// minimal per-fleet summary. The full present gate + confirm + apply are
/// Phases 5-6; this phase persists proposals and reports what landed.
fn run_llm_propose(
    cli: &Cli,
    config: &Config,
    patterns: &[String],
    change_id: Option<String>,
    prompt: &str,
) -> Result<()> {
    let change_id = change_id.unwrap_or_else(generate_change_id);
    debug!("run_llm_propose: change_id={change_id} patterns={patterns:?}");

    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli
        .max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    let repos = discover_repos(start_dir, max_depth, &config.ignore_patterns())
        .context("Failed to discover repositories")?;
    let filtered_repos = filter_repos(repos, patterns);
    if filtered_repos.is_empty() {
        println!("No repositories found matching the specified patterns.");
        return Ok(());
    }

    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    let summary =
        core::propose::execute_propose(&filtered_repos, &change_id, prompt, config, parallel_jobs)?;

    // Minimal report (present gate is Phase 6). Every ProposeSummary field is
    // consumed here so the bin target's dead-code check stays honest.
    debug!(
        "run_llm_propose: change_id={} token={}",
        summary.change_id, summary.token
    );
    println!(
        "Proposed change {}: {} proposed | {} empty | {} failed",
        summary.change_id, summary.proposed, summary.empty, summary.failed
    );
    for rp in &summary.repos {
        if rp.outcome == core::manifest::ProposalOutcome::Failed {
            println!(
                "  FAILED {}: {}",
                rp.slug,
                rp.error.as_deref().unwrap_or("unknown error")
            );
        }
    }
    if let Some(dir) = summary.manifest_path.parent() {
        println!("Proposal artifacts: {}", dir.display());
    }
    Ok(())
}

/// Show the resolved repository list and, when a prompt is required, confirm
/// before committing. Returns `Ok(true)` to proceed, `Ok(false)` if the user
/// declined. Fails closed: a required prompt on a non-interactive stdin without
/// `--yes` returns an error naming the flag rather than silently proceeding ([A9]).
fn confirm_blast_radius(
    repos: &[Repo],
    patterns: &[String],
    needs_prompt: bool,
    yes: bool,
) -> Result<bool> {
    use std::io::{IsTerminal, Write};

    println!("Targeting {} repositories:", repos.len());
    for repo in repos {
        println!("  {}", repo.slug);
    }

    if !needs_prompt {
        return Ok(true);
    }

    if yes {
        debug!("--yes supplied; skipping confirmation prompt");
        return Ok(true);
    }

    if !std::io::stdin().is_terminal() {
        return Err(eyre::eyre!(
            "Refusing to commit to {} repositories without confirmation on non-interactive stdin; pass --yes to proceed",
            repos.len()
        ));
    }

    let reason = if patterns.is_empty() {
        "no -p patterns given (all discovered repos)"
    } else {
        "repo count exceeds confirm-threshold"
    };
    print!(
        "Commit to these {} repositories? [{reason}] (y/N): ",
        repos.len()
    );
    std::io::stdout().flush().ok();

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read confirmation from stdin")?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Display pattern analysis for substitution operations
fn display_pattern_analysis(results: &[CreateResult], opts: &StatusOptions) {
    // Check if any results have substitution stats (indicating substitution operations)
    let has_substitution_stats = results.iter().any(|r| r.substitution_stats.is_some());

    if !has_substitution_stats {
        return; // No substitution operations, skip analysis
    }

    // Aggregate statistics from all results
    let total_files_scanned = results
        .iter()
        .filter_map(|r| r.substitution_stats.as_ref())
        .map(|s| s.files_scanned)
        .sum::<usize>();

    let files_changed = results
        .iter()
        .filter_map(|r| r.substitution_stats.as_ref())
        .map(|s| s.files_changed)
        .sum::<usize>();

    let files_no_matches = results
        .iter()
        .filter_map(|r| r.substitution_stats.as_ref())
        .map(|s| s.files_no_matches)
        .sum::<usize>();

    let files_no_change = results
        .iter()
        .filter_map(|r| r.substitution_stats.as_ref())
        .map(|s| s.files_no_change)
        .sum::<usize>();

    let total_matches = results
        .iter()
        .filter_map(|r| r.substitution_stats.as_ref())
        .map(|s| s.total_matches)
        .sum::<usize>();

    let files_skipped_binary = results
        .iter()
        .filter_map(|r| r.substitution_stats.as_ref())
        .map(|s| s.files_skipped_binary)
        .sum::<usize>();

    if total_files_scanned > 0 {
        if opts.use_emoji {
            println!("\n🔍 Pattern Analysis:");
            println!("   📄 Files scanned: {total_files_scanned}");
            println!("   ✅ Files changed: {files_changed}");
            if total_matches > 0 {
                println!("   🎯 Total matches: {total_matches}");
            }
            if files_no_matches > 0 {
                println!("   ❌ Files with no matches: {files_no_matches}");
            }
            if files_no_change > 0 {
                println!("   🔄 Files matched but unchanged: {files_no_change}");
            }
            if files_skipped_binary > 0 {
                println!("   ⏩  Binary files skipped: {files_skipped_binary}");
            }

            if files_changed == 0 && total_files_scanned > 0 {
                println!("   🚨  No files were modified by the pattern");
            }
        } else {
            println!("\nPattern Analysis:");
            println!("   Files scanned: {total_files_scanned}");
            println!("   Files changed: {files_changed}");
            if total_matches > 0 {
                println!("   Total matches: {total_matches}");
            }
            if files_no_matches > 0 {
                println!("   Files with no matches: {files_no_matches}");
            }
            if files_no_change > 0 {
                println!("   Files matched but unchanged: {files_no_change}");
            }
            if files_skipped_binary > 0 {
                println!("   Binary files skipped: {files_skipped_binary}");
            }

            if files_changed == 0 && total_files_scanned > 0 {
                println!("   Warning: No files were modified by the pattern");
            }
        }
    }
}

/// Display summary of create results
fn display_create_summary(results: &[CreateResult], opts: &StatusOptions) {
    let total = results.len();
    let successful = results.iter().filter(|r| r.error.is_none()).count();
    let errors = total - successful;

    // Count dry runs that would have changes vs those that wouldn't
    let dry_runs_with_changes = results
        .iter()
        .filter(|r| {
            matches!(r.action, CreateAction::DryRun)
                && (r
                    .substitution_stats
                    .as_ref()
                    .map(|s| s.files_changed > 0)
                    .unwrap_or(false)
                    || !r.files_affected.is_empty())
        })
        .count();
    let dry_runs_no_changes = results
        .iter()
        .filter(|r| {
            matches!(r.action, CreateAction::DryRun)
                && !r
                    .substitution_stats
                    .as_ref()
                    .map(|s| s.files_changed > 0)
                    .unwrap_or(false)
                && r.files_affected.is_empty()
        })
        .count();

    let committed = results
        .iter()
        .filter(|r| matches!(r.action, CreateAction::Committed))
        .count();
    let prs_created = results
        .iter()
        .filter(|r| matches!(r.action, CreateAction::PrCreated))
        .count();

    let total_files: usize = results.iter().map(|r| r.files_affected.len()).sum();

    if opts.use_emoji {
        println!("\n📊 {total} repositories processed:");
        if dry_runs_with_changes > 0 {
            println!("   👀  {dry_runs_with_changes} would change");
        }
        if dry_runs_no_changes > 0 {
            println!("   ➖ {dry_runs_no_changes} no matches");
        }
        if committed > 0 {
            println!("   💾 {committed} committed");
        }
        if prs_created > 0 {
            println!("   📥 {prs_created} PRs created");
        }
        println!("   📄 {total_files} files affected");
        if errors > 0 {
            println!("   ❌ {errors} errors");
        }
    } else {
        println!("\nSummary: {total} repositories processed:");
        if dry_runs_with_changes > 0 {
            println!("   {dry_runs_with_changes} would change");
        }
        if dry_runs_no_changes > 0 {
            println!("   {dry_runs_no_changes} no matches");
        }
        if committed > 0 {
            println!("   {committed} committed");
        }
        if prs_created > 0 {
            println!("   {prs_created} PRs created");
        }
        println!("   {total_files} files affected");
        if errors > 0 {
            println!("   {errors} errors");
        }
    }

    // Add pattern analysis for substitution operations
    display_pattern_analysis(results, opts);
}
