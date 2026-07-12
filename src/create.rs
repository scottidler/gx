use crate::cli::Cli;
use crate::config::Config;
use crate::diff;
use crate::file;
use crate::git;
use crate::github;
use crate::output::{display_unified_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use crate::state::{ChangeState, StateManager};
use crate::transaction::Transaction;
use chrono::Local;
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::path::Path;
use std::sync::Mutex;

/// Statistics for substitution operations
#[derive(Debug, Default, Clone)]
pub struct SubstitutionStats {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_no_matches: usize,
    pub files_no_change: usize,
    pub files_skipped_binary: usize,
    pub total_matches: usize,
}

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

#[derive(Debug, Clone)]
pub enum Change {
    Add(String, String),   // path, content
    Delete,                // delete matched files
    Sub(String, String),   // pattern, replacement
    Regex(String, String), // regex pattern, replacement
}

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub repo: Repo,
    pub change_id: String,
    pub action: CreateAction,
    pub files_affected: Vec<String>,
    pub substitution_stats: Option<SubstitutionStats>,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    /// The branch the repo was on before the change (for state tracking).
    pub original_branch: Option<String>,
    /// The pre-commit HEAD of the base branch (the safe point), set once a
    /// commit lands. `None` for dry runs and pre-commit failures.
    pub base_sha: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CreateAction {
    DryRun, // No changes made (preview)

    Committed, // Changes committed to branch
    PrCreated, // PR created successfully
}

/// Generate a default change ID based on current timestamp
pub fn generate_change_id() -> String {
    let now = Local::now();
    let timestamp = now.format("%Y-%m-%dT%H-%M-%S").to_string();
    format!("GX-{timestamp}")
}

/// Process create command across multiple repositories
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
    info!("Starting create command with change: {change:?}");

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

    info!("Discovered {} repositories", repos.len());

    let filtered_repos = filter_repos(repos, patterns);
    info!(
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

    // Change-level lock (Phase 7 [F6]): held for the whole run so another
    // process's `changes/<id>.json` read-modify-write (`review sync`,
    // `cleanup`, `undo`, ...) can never interleave with this run's incremental
    // saves. The in-process `Mutex<ChangeState>` below still serializes this
    // run's own rayon workers against EACH OTHER; this lock is the
    // cross-process half, so it is acquired ONCE here rather than per-repo
    // (per-repo would make sibling workers in the SAME run fail-fast against
    // each other, since the lock itself doesn't queue).
    let _change_lock = if commit_message.is_some() {
        Some(
            crate::lock::ChangeLock::acquire(&change_id)
                .map_err(|e| eyre::eyre!("Cannot start create for {change_id}: {e}"))?,
        )
    } else {
        None
    };

    // Initialize state tracking if we're going to make changes (not dry run).
    let change_state = if commit_message.is_some() {
        let state = ChangeState::new(change_id.clone(), commit_message.clone());
        Some(Mutex::new(state))
    } else {
        None
    };
    // One state manager, shared for incremental saves after each repo ([A3]).
    let state_manager = if commit_message.is_some() {
        match StateManager::new() {
            Ok(manager) => Some(manager),
            Err(e) => {
                warn!("Failed to create state manager: {e}");
                None
            }
        }
    } else {
        None
    };

    // Determine parallelism
    let parallel_jobs = cli
        .parallel
        .or_else(|| crate::utils::get_jobs_from_config(config))
        .unwrap_or_else(num_cpus::get);

    // Set up thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs)
        .build()
        .context("Failed to create thread pool")?;

    // Process repositories in parallel. The change-state save is now done
    // INSIDE `process_single_repo` (Phase 4 control-flow refactor, F12): a
    // pushed-safe-point save before `finalize()` runs, then a final save once
    // the whole result (including any PR) is known. This fold is display-only;
    // the `Mutex<ChangeState>` + `StateManager` are passed in, and each worker
    // locks briefly to update just its own repo's entry, same as before.
    let results: Vec<CreateResult> = pool.install(|| {
        filtered_repos
            .par_iter()
            .map(|repo| {
                process_single_repo(
                    repo,
                    &change_id,
                    files,
                    &change,
                    commit_message.as_deref(),
                    pr.as_ref(),
                    config,
                    change_state.as_ref(),
                    state_manager.as_ref(),
                )
            })
            .collect()
    });

    if let Some(state_mutex) = change_state {
        if let Ok(state) = state_mutex.into_inner() {
            if !state.repositories.is_empty() {
                info!("Saved change state for {}", state.change_id);
            }
        }
    }

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

/// Update change state based on create result
fn update_change_state(
    state: &mut ChangeState,
    result: &CreateResult,
    pr: Option<&crate::cli::PR>,
) {
    // Only track if the operation actually did something
    match result.action {
        CreateAction::Committed | CreateAction::PrCreated => {
            // Add repository to state
            state.add_repository(result.repo.slug.clone(), result.change_id.clone());

            // Update local path and files modified
            if let Some(repo_state) = state.repositories.get_mut(&result.repo.slug) {
                repo_state.local_path = Some(result.repo.path.to_string_lossy().to_string());
                repo_state.files_modified = result.files_affected.clone();
                repo_state.original_branch = result.original_branch.clone();
            }

            // If PR was created, update PR info using the new set_pr_info method
            if matches!(result.action, CreateAction::PrCreated) {
                if let (Some(pr_number), Some(pr_url)) = (result.pr_number, result.pr_url.clone()) {
                    let is_draft = matches!(pr, Some(crate::cli::PR::Draft));
                    state.set_pr_info(&result.repo.slug, pr_number, pr_url, is_draft);
                }
            }
        }
        CreateAction::DryRun => {
            // Don't track dry runs
        }
    }
}

/// Build an error result in the DryRun (nothing committed) state.
fn dry_run_error(repo: &Repo, change_id: &str, error: String) -> CreateResult {
    CreateResult {
        repo: repo.clone(),
        change_id: change_id.to_string(),
        action: CreateAction::DryRun,
        files_affected: Vec::new(),
        substitution_stats: None,
        pr_number: None,
        pr_url: None,
        original_branch: None,
        base_sha: None,
        error: Some(error),
    }
}

/// Process create command for a single repository with comprehensive rollback.
///
/// Order (design Architecture): lock → stash -u → switch to head → pull →
/// mutate → branch → stage → commit → push → finalize → create PR. Rollback
/// steps are persisted write-ahead via the typed `Transaction`.
///
/// The change-state save is a NAMED control-flow refactor (Phase 4 [F12], panel
/// finding): it happens IN HERE now, not in the caller's outer fold, at two
/// points. First, a safe-point save right after the push (`Phase::Pushed`) but
/// BEFORE `finalize()` runs - `finalize()` deletes the recovery file, so this
/// guarantees a pushed branch is recorded in state OR recovery in every crash
/// window, never neither. Second, a final save once the whole result (including
/// any PR) is known, replacing what the caller's rayon fold used to do.
#[allow(clippy::too_many_arguments)]
fn process_single_repo(
    repo: &Repo,
    change_id: &str,
    file_patterns: &[String],
    change: &Change,
    commit_message: Option<&str>,
    pr: Option<&crate::cli::PR>,
    config: &Config,
    change_state: Option<&Mutex<ChangeState>>,
    state_manager: Option<&StateManager>,
) -> CreateResult {
    debug!(
        "process_single_repo: repo={} change_id={change_id}",
        repo.name
    );
    let repo_path = &repo.path;
    let committing = commit_message.is_some();

    // Per-repo lock: a second concurrent gx invocation must not interleave
    // stash/branch operations on this repo (design Q5).
    let _lock = match crate::lock::RepoLock::acquire(repo_path) {
        Ok(lock) => lock,
        Err(e) => return dry_run_error(repo, change_id, format!("Repository is locked: {e}")),
    };

    let mut transaction = Transaction::new(repo_path.clone(), change_id.to_string(), committing);
    let mut files_affected = Vec::new();
    let mut diff_parts = Vec::new();

    // 1. Determine the original branch; guard against detached HEAD ([A30]).
    let original_branch = match git::get_current_branch_name(repo_path) {
        Ok(branch) if branch.is_empty() => {
            return dry_run_error(
                repo,
                change_id,
                "Repository is in detached HEAD state; check out a branch first".to_string(),
            );
        }
        Ok(branch) => branch,
        Err(e) => {
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to get current branch: {e}"),
            );
        }
    };
    transaction.set_original_branch(original_branch.clone());

    // 2. Stash uncommitted work (including untracked, -u) so the worktree is a
    //    pristine checkout of HEAD during mutation. status --porcelain counts
    //    untracked (??) entries, so the dirty predicate already includes them.
    match git::has_uncommitted_changes(repo_path) {
        Ok(true) => {
            let message = format!("GX auto-stash for {change_id}");
            // Write-ahead (F5): register the stash-restore step keyed by message
            // BEFORE the stash exists, so a crash in the window between creating
            // the stash and learning its SHA still records the WIP to restore.
            if let Err(e) =
                transaction.push_step(crate::transaction::RollbackStep::PopStashByMessage {
                    repo: repo_path.clone(),
                    message: message.clone(),
                })
            {
                transaction.rollback();
                return dry_run_error(repo, change_id, format!("Failed to persist recovery: {e}"));
            }
            match git::stash_save_with_untracked(repo_path, &message) {
                Ok(sha) => {
                    transaction.set_stash_sha(sha.clone());
                    // Swap the placeholder for the SHA-keyed step now that the
                    // stash exists (the SHA survives concurrent stash mutation).
                    if let Err(e) =
                        transaction.swap_last_step(crate::transaction::RollbackStep::PopStash {
                            repo: repo_path.clone(),
                            stash_sha: sha,
                        })
                    {
                        transaction.rollback();
                        return dry_run_error(
                            repo,
                            change_id,
                            format!("Failed to persist recovery: {e}"),
                        );
                    }
                    // Crash hook (Phase 8): the stash exists and its restore step
                    // is persisted (phase `mutating`); an abort here must recover
                    // to a byte-identical worktree with the branch never created.
                    crate::crash::maybe_crash("after-stash");
                }
                Err(e) => {
                    // The stash was never created; roll back to clear the
                    // placeholder (it resolves to a harmless no-op).
                    transaction.rollback();
                    return dry_run_error(repo, change_id, format!("Failed to stash changes: {e}"));
                }
            }
        }
        Ok(false) => {}
        Err(e) => {
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to check repository status: {e}"),
            );
        }
    }

    // 3. Switch to the head branch if we are not already on it. A failure here
    //    (F10) is a hard per-repo error: swallowing it would silently mutate
    //    whatever branch the user happened to be on.
    let head = match git::get_head_branch(repo_path) {
        Ok(head) => head,
        Err(e) => {
            transaction.rollback();
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to determine head branch: {e}"),
            );
        }
    };
    // Write-ahead: ALWAYS register the switch-back to the user's original branch,
    // even in the common `head == original_branch` case where no switch-to-head
    // is needed. Keep-work recovery (`pushed`/`finalizing`) restores the
    // environment by executing SwitchBranch/PopStash steps ONLY; without this
    // step, a keep-work recovery after a push/finalize crash would strand the
    // user on the GX branch instead of returning them to their original branch
    // (finalize's own switch-back never runs on a crash). In full reverse this
    // step is a harmless no-op: DeleteLocalBranch already force-switches off the
    // GX branch to head, which equals the original branch in the common case.
    if let Err(e) = transaction.push_step(crate::transaction::RollbackStep::SwitchBranch {
        repo: repo_path.clone(),
        branch: original_branch.clone(),
    }) {
        transaction.rollback();
        return dry_run_error(repo, change_id, format!("Failed to persist recovery: {e}"));
    }
    if head != original_branch {
        if let Err(e) = git::switch_branch(repo_path, &head) {
            transaction.rollback();
            return dry_run_error(
                repo,
                change_id,
                format!("Failed to switch to head branch: {e}"),
            );
        }
    }

    // 4. Pull latest changes.
    if let Err(e) = git::pull_latest_changes(repo_path) {
        transaction.rollback();
        return dry_run_error(
            repo,
            change_id,
            format!("Failed to pull latest changes: {e}"),
        );
    }

    // 5. Apply the change (each registers its undo step write-ahead).
    let mut substitution_stats = None;
    let change_result = match change {
        Change::Add(path, content) => apply_add_change(
            repo_path,
            path,
            content,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ),
        Change::Delete => apply_delete_change(
            repo_path,
            file_patterns,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ),
        Change::Sub(pattern, replacement) => apply_substitution_change(
            repo_path,
            file_patterns,
            pattern,
            replacement,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        )
        .map(|stats| substitution_stats = Some(stats)),
        Change::Regex(pattern, replacement) => apply_regex_change(
            repo_path,
            file_patterns,
            pattern,
            replacement,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        )
        .map(|stats| substitution_stats = Some(stats)),
    };

    if let Err(e) = change_result {
        transaction.rollback();
        let mut result = dry_run_error(repo, change_id, format!("Failed to apply changes: {e}"));
        result.substitution_stats = substitution_stats;
        return result;
    }

    // No files affected, or dry run: roll back (restores worktree, branch, stash).
    if files_affected.is_empty() || !committing {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: if committing {
                Vec::new()
            } else {
                files_affected
            },
            substitution_stats,
            pr_number: None,
            pr_url: None,
            original_branch: Some(original_branch.clone()),
            base_sha: None,
            error: None,
        };
    }

    let commit_message = commit_message.unwrap_or_default();

    // 6. branch → stage → commit → push (each undo persisted write-ahead).
    let base_sha = match commit_changes_with_rollback(
        repo_path,
        change_id,
        commit_message,
        &files_affected,
        &mut transaction,
    ) {
        Ok(base_sha) => base_sha,
        Err(e) => {
            transaction.rollback();
            let mut result =
                dry_run_error(repo, change_id, format!("Failed to commit changes: {e}"));
            result.substitution_stats = substitution_stats;
            return result;
        }
    };

    // 6b. Pushed safe-point save (F12, control-flow refactor): record the
    //     branch in change state NOW, before finalize() runs and deletes the
    //     recovery file. A crash anywhere from here on - including mid-finalize
    //     - leaves this repo recorded in state even after recovery is gone.
    record_pushed_state(
        change_state,
        state_manager,
        repo,
        change_id,
        &original_branch,
        &files_affected,
        &base_sha,
    );

    // 7. Finalize BEFORE creating the PR: switch back to the original branch and
    //    re-apply the stash. A finalize error (e.g. cannot restore branch) keeps
    //    the recovery file for manual resolution and is reported as Committed.
    let finalize_outcome = match transaction.finalize() {
        Ok(outcome) => outcome,
        Err(e) => {
            let result = CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::Committed,
                files_affected,
                substitution_stats,
                pr_number: None,
                pr_url: None,
                original_branch: Some(original_branch.clone()),
                base_sha: Some(base_sha),
                error: Some(format!("Committed and pushed, but finalize failed: {e}")),
            };
            record_final_state(change_state, state_manager, &result, pr);
            return result;
        }
    };

    // 8. Create the PR against the (already-restored) remote. A PR failure is
    //    surfaced on the result, not swallowed ([A4]; Phase 5 refines).
    let (action, pr_number, pr_url, mut error) = match pr {
        Some(pr) => match create_pull_request(repo, change_id, commit_message, pr, config) {
            Ok(result) => (
                CreateAction::PrCreated,
                Some(result.number),
                Some(result.url),
                None,
            ),
            Err(e) => (
                CreateAction::Committed,
                None,
                None,
                Some(format!("PR creation failed: {e}")),
            ),
        },
        None => (CreateAction::Committed, None, None, None),
    };

    // A stash-restore conflict is surfaced (design Q2): committed, but the user's
    // WIP could not be re-applied; the stash is preserved for manual recovery.
    if let Some((sha, msg)) = finalize_outcome.stash_error {
        let stash_err = format!(
            "stash-restore-failed: could not re-apply stash {sha} ({msg}); recover with `git stash apply {sha}`"
        );
        error = Some(match error {
            Some(existing) => format!("{existing}; {stash_err}"),
            None => stash_err,
        });
    }

    let result = CreateResult {
        repo: repo.clone(),
        change_id: change_id.to_string(),
        action,
        files_affected,
        substitution_stats,
        pr_number,
        pr_url,
        original_branch: Some(original_branch.clone()),
        base_sha: Some(base_sha),
        error,
    };
    record_final_state(change_state, state_manager, &result, pr);
    result
}

/// Record the just-pushed branch in change state (the F12 safe point): saved
/// BEFORE `finalize()` runs, so a crash during finalize (which deletes the
/// recovery file) still leaves this repo recorded in at least one store.
fn record_pushed_state(
    change_state: Option<&Mutex<ChangeState>>,
    state_manager: Option<&StateManager>,
    repo: &Repo,
    change_id: &str,
    original_branch: &str,
    files_affected: &[String],
    base_sha: &str,
) {
    debug!(
        "record_pushed_state: repo={} change_id={change_id} base_sha={base_sha}",
        repo.slug
    );
    let Some(state_mutex) = change_state else {
        return;
    };
    let Ok(mut state) = state_mutex.lock() else {
        warn!(
            "Change state mutex poisoned; skipping pushed safe-point save for {}",
            repo.slug
        );
        return;
    };
    state.add_repository(repo.slug.clone(), change_id.to_string());
    if let Some(repo_state) = state.repositories.get_mut(&repo.slug) {
        repo_state.local_path = Some(repo.path.to_string_lossy().to_string());
        repo_state.files_modified = files_affected.to_vec();
        repo_state.original_branch = Some(original_branch.to_string());
        repo_state.base_sha = Some(base_sha.to_string());
    }
    if let Some(manager) = state_manager {
        if let Err(e) = manager.save(&state) {
            warn!(
                "Failed to save pushed safe-point state for {}: {e}",
                repo.slug
            );
        }
    }
}

/// Fold a finished repo's result into change state and save. This is now the
/// ONLY place a finished repo's outcome is saved (the caller's outer rayon
/// fold is display-only, Phase 4 control-flow refactor). Re-records `base_sha`
/// since `update_change_state` -> `add_repository` resets the entry.
fn record_final_state(
    change_state: Option<&Mutex<ChangeState>>,
    state_manager: Option<&StateManager>,
    result: &CreateResult,
    pr: Option<&crate::cli::PR>,
) {
    debug!(
        "record_final_state: repo={} action={:?}",
        result.repo.slug, result.action
    );
    let Some(state_mutex) = change_state else {
        return;
    };
    let Ok(mut state) = state_mutex.lock() else {
        warn!(
            "Change state mutex poisoned; skipping final state save for {}",
            result.repo.slug
        );
        return;
    };
    update_change_state(&mut state, result, pr);
    if let Some(repo_state) = state.repositories.get_mut(&result.repo.slug) {
        repo_state.base_sha = result.base_sha.clone();
    }
    if matches!(
        result.action,
        CreateAction::Committed | CreateAction::PrCreated
    ) {
        if let Some(manager) = state_manager {
            if let Err(e) = manager.save(&state) {
                warn!("Failed to save change state: {e}");
            }
        }
    }
}

/// Apply add change (create new file)
fn apply_add_change(
    repo_path: &Path,
    file_path: &str,
    content: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<()> {
    // Validate and resolve the path; `gx add` is the one write path that does
    // not flow through FileSet, so it enforces the same policy directly ([A32]).
    let full_path = file::validate_new_file_path(repo_path, file_path)?;

    // Check if file already exists
    if full_path.exists() {
        return Err(eyre::eyre!("File already exists: {}", file_path));
    }

    // Write-ahead: register removal of the created file before creating it.
    transaction.push_step(crate::transaction::RollbackStep::RemoveCreatedFile {
        path: full_path.clone(),
    })?;

    // Create file and generate diff
    let (_, diff) = file::create_file_with_content(&full_path, content, 3)?;

    files_affected.push(file_path.to_string());
    diff_parts.push(format!(
        "  A {}\n{}",
        file_path,
        crate::utils::indent(&diff, 4)
    ));

    Ok(())
}

/// Apply delete change (remove matching files)
fn apply_delete_change(
    repo_path: &Path,
    file_patterns: &[String],
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<()> {
    // Find tracked files matching all patterns (deduped + sorted).
    let all_files = file::FileSet::matching_any(repo_path, file_patterns)?;

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Read content for diff; skip non-UTF-8 (binary) files ([A21]).
        let Some(content) = file::read_utf8_or_skip(&full_path)? else {
            continue;
        };

        // Out-of-tree backup, then write-ahead register the restore before delete.
        let backup_path = transaction.backup_path_for(&file_path)?;
        let mode = file::create_backup(&full_path, &backup_path)?;
        transaction.push_step(crate::transaction::RollbackStep::RestoreBackup {
            backup: backup_path,
            original: full_path.clone(),
            mode,
        })?;

        // Delete file
        file::delete_file(&full_path)?;

        let diff = diff::generate_diff(&content, "", 3);
        files_affected.push(file_path.to_string_lossy().to_string());
        diff_parts.push(format!(
            "  D {}\n{}",
            file_path.display(),
            crate::utils::indent(&diff, 4)
        ));
    }

    Ok(())
}

/// Apply substitution change
fn apply_substitution_change(
    repo_path: &Path,
    file_patterns: &[String],
    pattern: &str,
    replacement: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<SubstitutionStats> {
    let mut stats = SubstitutionStats::default();

    // Find tracked files matching all patterns (deduped + sorted).
    let all_files = file::FileSet::matching_any(repo_path, file_patterns)?;
    stats.files_scanned = all_files.len();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Try to apply substitution
        match file::apply_substitution_to_file(&full_path, pattern, replacement, 3)? {
            diff::SubstitutionResult::Changed {
                content: updated_content,
                diff,
                matches,
            } => {
                // Out-of-tree backup, then write-ahead register the restore.
                let backup_path = transaction.backup_path_for(&file_path)?;
                let mode = file::create_backup(&full_path, &backup_path)?;
                transaction.push_step(crate::transaction::RollbackStep::RestoreBackup {
                    backup: backup_path,
                    original: full_path.clone(),
                    mode,
                })?;

                // Write updated content
                file::write_file_content(&full_path, &updated_content)?;

                files_affected.push(file_path.to_string_lossy().to_string());
                diff_parts.push(format!(
                    "  M {}\n{}",
                    file_path.display(),
                    crate::utils::indent(&diff, 4)
                ));

                stats.files_changed += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::NoMatches => {
                debug!(
                    "No matches found for pattern '{}' in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_matches += 1;
            }
            diff::SubstitutionResult::NoChange { matches } => {
                debug!(
                    "Pattern '{}' matched but no changes resulted in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_change += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::SkippedBinary => {
                stats.files_skipped_binary += 1;
            }
        }
    }

    Ok(stats)
}

/// Apply regex change
fn apply_regex_change(
    repo_path: &Path,
    file_patterns: &[String],
    pattern: &str,
    replacement: &str,
    transaction: &mut Transaction,
    files_affected: &mut Vec<String>,
    diff_parts: &mut Vec<String>,
) -> Result<SubstitutionStats> {
    let mut stats = SubstitutionStats::default();

    // Find tracked files matching all patterns (deduped + sorted).
    let all_files = file::FileSet::matching_any(repo_path, file_patterns)?;
    stats.files_scanned = all_files.len();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Try to apply regex substitution
        match file::apply_regex_to_file(&full_path, pattern, replacement, 3)? {
            diff::SubstitutionResult::Changed {
                content: updated_content,
                diff,
                matches,
            } => {
                // Out-of-tree backup, then write-ahead register the restore.
                let backup_path = transaction.backup_path_for(&file_path)?;
                let mode = file::create_backup(&full_path, &backup_path)?;
                transaction.push_step(crate::transaction::RollbackStep::RestoreBackup {
                    backup: backup_path,
                    original: full_path.clone(),
                    mode,
                })?;

                // Write updated content
                file::write_file_content(&full_path, &updated_content)?;

                files_affected.push(file_path.to_string_lossy().to_string());
                diff_parts.push(format!(
                    "  M {}\n{}",
                    file_path.display(),
                    crate::utils::indent(&diff, 4)
                ));

                stats.files_changed += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::NoMatches => {
                debug!(
                    "No matches found for regex pattern '{}' in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_matches += 1;
            }
            diff::SubstitutionResult::NoChange { matches } => {
                debug!(
                    "Regex pattern '{}' matched but no changes resulted in {}",
                    pattern,
                    file_path.display()
                );
                stats.files_no_change += 1;
                stats.total_matches += matches;
            }
            diff::SubstitutionResult::SkippedBinary => {
                stats.files_skipped_binary += 1;
            }
        }
    }

    Ok(stats)
}

/// Create the gx branch, stage, commit, and push - registering each undo step
/// write-ahead. The success-path branch restoration and stash pop are handled by
/// `Transaction::finalize`, not here. Returns the pre-commit HEAD (the safe
/// point `ResetCommit` already captures), so the caller can record `base_sha`
/// (F11/F12) at the pushed-state safe point before `finalize()` runs.
fn commit_changes_with_rollback(
    repo_path: &Path,
    change_id: &str,
    commit_message: &str,
    files_affected: &[String],
    transaction: &mut Transaction,
) -> Result<String> {
    use crate::transaction::{Phase, RollbackStep};

    // Whether the branch pre-existed gx's run (so rollback won't delete it).
    let branch_existed = git::branch_exists_locally(repo_path, change_id).unwrap_or(false);

    // Record the GX branch name so recovery (phase reporting, the `pushing`
    // probe, `gx undo`) need not re-derive it.
    transaction.set_branch(change_id.to_string());

    // Write-ahead: register branch deletion before creating the branch.
    transaction.push_step(RollbackStep::DeleteLocalBranch {
        repo: repo_path.to_path_buf(),
        branch: change_id.to_string(),
        branch_existed,
    })?;
    git::create_branch(repo_path, change_id)
        .with_context(|| format!("Failed to create or switch to branch: {change_id}"))?;
    // Crash hook (Phase 8): the GX branch exists and its delete step is
    // persisted (phase `mutating`); recovery full-reverses, remote branch absent.
    crate::crash::maybe_crash("after-branch");

    // Record the pre-commit HEAD so rollback resets to a known target, and
    // register the reset write-ahead before committing.
    let expected_sha = git::get_head_sha(repo_path)?;
    transaction.push_step(RollbackStep::ResetCommit {
        repo: repo_path.to_path_buf(),
        expected_sha: expected_sha.clone(),
    })?;

    // Stage only the specific files we modified - never "git add .".
    git::add_files(repo_path, files_affected).context("Failed to stage files")?;
    git::commit_changes(repo_path, commit_message).context("Failed to commit changes")?;
    // Crash hook (Phase 8): the commit is on the GX branch and the reset step is
    // persisted (phase `mutating`); recovery full-reverses, remote branch absent.
    crate::crash::maybe_crash("after-commit");

    // Stamp `pushing` write-ahead: a kill after this stamp but before the push
    // completes is classified at recovery time by a read-only ls-remote probe.
    // Rollback no longer registers a remote-delete step - `gx undo` owns remote
    // reversal, so nothing on the rollback path can ever delete a pushed branch.
    transaction.set_phase(Phase::Pushing)?;
    // Crash hook (Phase 8): `pushing` is stamped but the push has NOT run; the
    // ls-remote probe finds the branch absent and dispatches a full reverse.
    crate::crash::maybe_crash("before-push");
    git::push_branch(repo_path, change_id).context("Failed to push branch")?;
    // Stamp `pushed`: the branch is now shared; recovery keeps the work.
    transaction.set_phase(Phase::Pushed)?;
    // Crash hook (Phase 8): the branch is pushed and `pushed` is stamped;
    // recovery keeps the shared work (remote branch retained).
    crate::crash::maybe_crash("after-push");

    Ok(expected_sha)
}

/// Create a pull request for the changes
/// Returns the PR number and URL on success
fn create_pull_request(
    repo: &Repo,
    change_id: &str,
    commit_message: &str,
    pr: &crate::cli::PR,
    config: &Config,
) -> Result<github::CreatePrResult> {
    let repo_slug = &repo.slug;
    let base = resolve_base_branch(repo, config);
    let result = github::create_pr(repo_slug, change_id, commit_message, &base, pr, config)
        .with_context(|| format!("Failed to create PR for {repo_slug}"))?;
    info!(
        "Created PR #{} for repository: {} - {}",
        result.number, repo_slug, result.url
    );
    Ok(result)
}

/// Resolve the repo's default base branch: prefer the local head branch, then
/// the GitHub API's default_branch, falling back to `main` with a warning - a
/// lookup failure must never drop the PR ([A4]).
fn resolve_base_branch(repo: &Repo, config: &Config) -> String {
    if let Ok(branch) = git::get_head_branch(&repo.path) {
        return branch;
    }
    let org = repo.slug.split('/').next().unwrap_or("");
    if let Ok(token) = github::read_token(org, config) {
        if let Ok(branch) = github::get_default_branch(&repo.slug, &token) {
            return branch;
        }
    }
    warn!(
        "Could not resolve default branch for {}; falling back to main",
        repo.slug
    );
    "main".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RepoChangeStatus;
    use crate::test_utils::run_git_command;
    use std::fs;
    use tempfile::TempDir;

    /// Initialize a git repo and commit all current files (fail-loud).
    fn init_git_repo(repo_path: &Path) {
        let init = run_git_command(&["init", "--quiet"], repo_path);
        assert!(init.status.success(), "git init failed");
        run_git_command(&["config", "user.email", "test@example.com"], repo_path);
        run_git_command(&["config", "user.name", "Test User"], repo_path);
        run_git_command(&["config", "commit.gpgsign", "false"], repo_path);
        let add = run_git_command(&["add", "-A"], repo_path);
        assert!(add.status.success(), "git add failed");
        let commit = run_git_command(&["commit", "--quiet", "-m", "init"], repo_path);
        assert!(commit.status.success(), "git commit failed");
    }

    #[test]
    fn test_generate_change_id() {
        let change_id = generate_change_id();
        assert!(change_id.starts_with("GX-"));
        assert!(change_id.len() > 10); // Should have timestamp
    }

    #[test]
    fn test_process_single_repo_hard_errors_on_head_branch_failure() {
        // F10: a `get_head_branch` failure must surface as a hard per-repo
        // error, not be silently swallowed (which would leave the repo on
        // whatever branch the user happened to be on).
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path().to_path_buf();
        fs::write(repo_path.join("README.md"), "# repo").unwrap();
        init_git_repo(&repo_path);
        // No `origin` remote: get_head_branch() can neither read
        // origin/HEAD nor confirm main/master exist remotely, so it errors.
        let repo = Repo::new(repo_path).unwrap();

        let result = process_single_repo(
            &repo,
            "GX-test",
            &["**/*.md".to_string()],
            &Change::Delete,
            None,
            None,
            &Config::default(),
            None,
            None,
        );

        assert!(
            result.error.is_some(),
            "a get_head_branch failure must be a hard error, not swallowed"
        );
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("determine head branch"),
            "error should name the head-branch failure, got: {:?}",
            result.error
        );
    }

    #[test]
    fn test_apply_add_change() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();
        let mut transaction =
            Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
        let mut files_affected = Vec::new();
        let mut diff_parts = Vec::new();

        let result = apply_add_change(
            repo_path,
            "new_file.txt",
            "Hello, world!",
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        assert!(result.is_ok());
        assert_eq!(files_affected.len(), 1);
        assert_eq!(files_affected[0], "new_file.txt");
        assert_eq!(diff_parts.len(), 1);
        assert!(repo_path.join("new_file.txt").exists());

        // Test rollback
        transaction.rollback();
        assert!(!repo_path.join("new_file.txt").exists());
    }

    #[test]
    fn test_apply_add_change_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();
        let file_path = repo_path.join("existing.txt");
        fs::write(&file_path, "existing content").unwrap();

        let mut transaction =
            Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
        let mut files_affected = Vec::new();
        let mut diff_parts = Vec::new();

        let result = apply_add_change(
            repo_path,
            "existing.txt",
            "new content",
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("File already exists"));
    }

    #[test]
    fn test_apply_delete_change() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create test files
        fs::write(repo_path.join("file1.txt"), "content1").unwrap();
        fs::write(repo_path.join("file2.txt"), "content2").unwrap();
        fs::write(repo_path.join("file3.md"), "markdown").unwrap();
        init_git_repo(repo_path);

        let mut transaction =
            Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
        let mut files_affected = Vec::new();
        let mut diff_parts = Vec::new();
        let patterns = vec!["*.txt".to_string()];

        let result = apply_delete_change(
            repo_path,
            &patterns,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        assert!(result.is_ok());
        assert_eq!(files_affected.len(), 2);
        assert!(!repo_path.join("file1.txt").exists());
        assert!(!repo_path.join("file2.txt").exists());
        assert!(repo_path.join("file3.md").exists()); // Should not be deleted

        // Test rollback
        transaction.rollback();
        assert!(repo_path.join("file1.txt").exists());
        assert!(repo_path.join("file2.txt").exists());
    }

    #[test]
    fn test_apply_substitution_change() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create test file
        fs::write(repo_path.join("test.txt"), "Hello world\nHello again").unwrap();
        init_git_repo(repo_path);

        let mut transaction =
            Transaction::new(repo_path.to_path_buf(), "GX-test".to_string(), false);
        let mut files_affected = Vec::new();
        let mut diff_parts = Vec::new();
        let patterns = vec!["*.txt".to_string()];

        let result = apply_substitution_change(
            repo_path,
            &patterns,
            "Hello",
            "Hi",
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        );

        assert!(result.is_ok());
        assert_eq!(files_affected.len(), 1);

        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "Hi world\nHi again");

        // Test rollback
        transaction.rollback();
        let content = fs::read_to_string(repo_path.join("test.txt")).unwrap();
        assert_eq!(content, "Hello world\nHello again");
    }

    // ---- Phase 4: pushed-state safe point (F12) ----

    /// Init `repo` with a bare `origin` remote at `bare`, push the initial
    /// branch, and set `origin/HEAD`. Returns the default branch name.
    fn init_repo_with_bare_remote(repo: &Path, bare: &Path) -> String {
        let parent = bare.parent().unwrap();
        run_git_command(
            &["init", "--quiet", "--bare", bare.to_str().unwrap()],
            parent,
        );
        fs::create_dir_all(repo).unwrap();
        fs::write(repo.join("README.md"), "# repo\n").unwrap();
        init_git_repo(repo);
        run_git_command(&["remote", "add", "origin", bare.to_str().unwrap()], repo);
        let branch = crate::git::get_current_branch_name(repo).unwrap();
        run_git_command(&["push", "--quiet", "-u", "origin", &branch], repo);
        run_git_command(&["remote", "set-head", "origin", &branch], repo);
        branch
    }

    /// Point `XDG_DATA_HOME` at `dir` for the duration of `f`, serialized
    /// behind the shared `ENV_LOCK` (env vars are process-global).
    fn with_data_home<F: FnOnce()>(dir: &Path, f: F) {
        let guard = crate::test_utils::ENV_LOCK.lock().unwrap();
        let prior = std::env::var("XDG_DATA_HOME").ok();
        unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
        f();
        match prior {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        drop(guard);
    }

    #[test]
    fn test_pushed_state_recorded_before_finalize_deletes_recovery() {
        // F12, "state-saved-first" order: the pushed safe-point save happens
        // BEFORE finalize() runs (finalize deletes the recovery file). A crash
        // any time after the save - even after finalize already cleaned up the
        // recovery file - still leaves the pushed branch recorded, because
        // state landed first.
        let data_home = TempDir::new().unwrap();
        with_data_home(data_home.path(), || {
            let ws = TempDir::new().unwrap();
            let repo_path = ws.path().join("repo");
            let bare = ws.path().join("repo.git");
            let branch = init_repo_with_bare_remote(&repo_path, &bare);
            fs::write(repo_path.join("README.md"), "# repo\nupdated\n").unwrap();

            let change_id = "GX-safepoint";
            let mut transaction = Transaction::new(repo_path.clone(), change_id.to_string(), true);
            let base_sha = commit_changes_with_rollback(
                &repo_path,
                change_id,
                "test commit",
                &["README.md".to_string()],
                &mut transaction,
            )
            .expect("commit+push should succeed");

            let repo = Repo::new(repo_path.clone()).unwrap();
            let change_state = Mutex::new(ChangeState::new(change_id.to_string(), None));
            let state_manager = StateManager::new().unwrap();

            record_pushed_state(
                Some(&change_state),
                Some(&state_manager),
                &repo,
                change_id,
                &branch,
                &["README.md".to_string()],
                &base_sha,
            );

            // Simulate the run continuing to finalize (which deletes the
            // recovery file) - the state save already happened, so it survives
            // regardless of what happens to the recovery file next.
            transaction.finalize().expect("finalize should succeed");

            let recoveries = Transaction::list_recovery_states().unwrap();
            assert!(
                recoveries.iter().all(|r| r.repo_path != repo_path),
                "finalize should have removed the recovery file"
            );

            let loaded = state_manager
                .load(change_id)
                .unwrap()
                .expect("change state must have been saved");
            let repo_state = loaded
                .repositories
                .get(&repo.slug)
                .expect("repo must be recorded");
            assert_eq!(repo_state.branch_name, change_id);
            assert_eq!(repo_state.base_sha.as_deref(), Some(base_sha.as_str()));
        });
    }

    #[test]
    fn test_pushed_branch_recorded_via_recovery_when_state_save_not_reached() {
        // F12, "recovery-only" order: if the process dies between the pushed
        // phase stamp and the pushed safe-point save (never reached), the
        // recovery file - stamped write-ahead BEFORE the push ran - still
        // records the branch on its own.
        let data_home = TempDir::new().unwrap();
        with_data_home(data_home.path(), || {
            let ws = TempDir::new().unwrap();
            let repo_path = ws.path().join("repo");
            let bare = ws.path().join("repo.git");
            init_repo_with_bare_remote(&repo_path, &bare);
            fs::write(repo_path.join("README.md"), "# repo\nupdated\n").unwrap();

            let change_id = "GX-recoveryonly";
            let mut transaction = Transaction::new(repo_path.clone(), change_id.to_string(), true);
            commit_changes_with_rollback(
                &repo_path,
                change_id,
                "test commit",
                &["README.md".to_string()],
                &mut transaction,
            )
            .expect("commit+push should succeed");

            // Simulate a crash right here: record_pushed_state is never
            // called, and finalize() never runs.
            let recoveries = Transaction::list_recovery_states().unwrap();
            let recorded = recoveries
                .iter()
                .find(|r| r.repo_path == repo_path)
                .expect("recovery file must exist for the pushed branch");
            assert_eq!(recorded.phase, crate::transaction::Phase::Pushed);
            assert_eq!(recorded.branch.as_deref(), Some(change_id));

            // No change state was ever saved for this change id.
            let state_manager = StateManager::new().unwrap();
            assert!(state_manager.load(change_id).unwrap().is_none());
        });
    }

    #[test]
    fn test_process_single_repo_records_state_with_base_sha() {
        // End-to-end (Phase 4 control-flow refactor): process_single_repo
        // itself - not just the lower-level helpers above - saves state with
        // base_sha via the Mutex<ChangeState>/StateManager now threaded in.
        let data_home = TempDir::new().unwrap();
        with_data_home(data_home.path(), || {
            let ws = TempDir::new().unwrap();
            let repo_path = ws.path().join("repo");
            let bare = ws.path().join("repo.git");
            init_repo_with_bare_remote(&repo_path, &bare);
            fs::write(repo_path.join("file1.txt"), "content1").unwrap();
            run_git_command(&["add", "-A"], &repo_path);
            run_git_command(&["commit", "--quiet", "-m", "add file1"], &repo_path);
            run_git_command(&["push", "--quiet"], &repo_path);

            let repo = Repo::new(repo_path.clone()).unwrap();
            let change_id = "GX-e2e-state";
            let change_state = Mutex::new(ChangeState::new(
                change_id.to_string(),
                Some("test".to_string()),
            ));
            let state_manager = StateManager::new().unwrap();

            let result = process_single_repo(
                &repo,
                change_id,
                &["file1.txt".to_string()],
                &Change::Delete,
                Some("delete file1"),
                None,
                &Config::default(),
                Some(&change_state),
                Some(&state_manager),
            );

            assert!(
                result.error.is_none(),
                "expected success, got: {:?}",
                result.error
            );
            assert!(result.base_sha.is_some());

            let loaded = state_manager
                .load(change_id)
                .unwrap()
                .expect("change state must have been saved");
            let repo_state = loaded
                .repositories
                .get(&repo.slug)
                .expect("repo must be recorded");
            assert_eq!(repo_state.base_sha, result.base_sha);
            assert_eq!(repo_state.status, RepoChangeStatus::BranchCreated);
        });
    }
}
