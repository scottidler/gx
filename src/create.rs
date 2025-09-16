use crate::cli::Cli;
use crate::config::Config;
use crate::diff;
use crate::file;
use crate::git;
use crate::github;
use crate::output::{display_unified_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use crate::transaction::Transaction;
use chrono::Local;
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::path::Path;

/// Statistics for substitution operations
#[derive(Debug, Default, Clone)]
pub struct SubstitutionStats {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_no_matches: usize,
    pub files_no_change: usize,
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
    let repos = discover_repos(start_dir, max_depth).context("Failed to discover repositories")?;

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
            for file_pattern in files {
                if let Ok(files_found) = file::find_files_in_repo(&repo.path, file_pattern) {
                    for file in files_found {
                        matched_files.push(file.display().to_string());
                        total_files += 1;
                    }
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
            let display_name = repo.slug.as_ref().unwrap_or(&repo.name);
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
    let repos = discover_repos(start_dir, max_depth).context("Failed to discover repositories")?;

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

    // Process repositories in parallel
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
                )
            })
            .collect()
    });

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

/// Process create command for a single repository
fn process_single_repo(
    repo: &Repo,
    change_id: &str,
    file_patterns: &[String],
    change: &Change,
    commit_message: Option<&str>,
    pr: Option<&crate::cli::PR>,
) -> CreateResult {
    debug!("Processing repository: {}", repo.name);

    let mut transaction = Transaction::new();
    let repo_path = &repo.path;
    let mut files_affected = Vec::new();
    let mut diff_parts = Vec::new();

    // Check if repository has uncommitted changes
    match git::has_uncommitted_changes(repo_path) {
        Ok(true) => {
            return CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected: Vec::new(),
                substitution_stats: None,

                error: Some(
                    "Repository has uncommitted changes. Please commit or stash them first."
                        .to_string(),
                ),
            };
        }
        Ok(false) => {} // Good, no uncommitted changes
        Err(e) => {
            return CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected: Vec::new(),
                substitution_stats: None,

                error: Some(format!("Failed to check repository status: {e}")),
            };
        }
    }

    // Get current branch for rollback
    let original_branch = match git::get_current_branch_name(repo_path) {
        Ok(branch) => branch,
        Err(e) => {
            return CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected: Vec::new(),
                substitution_stats: None,

                error: Some(format!("Failed to get current branch: {e}")),
            };
        }
    };

    // Apply changes based on change type
    let mut substitution_stats = None;
    let change_result = match change {
        Change::Add(path, content) => apply_add_change(
            repo_path,
            path,
            content,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ).map(|_| ()),
        Change::Delete => apply_delete_change(
            repo_path,
            file_patterns,
            &mut transaction,
            &mut files_affected,
            &mut diff_parts,
        ).map(|_| ()),
        Change::Sub(pattern, replacement) => {
            match apply_substitution_change(
                repo_path,
                file_patterns,
                pattern,
                replacement,
                &mut transaction,
                &mut files_affected,
                &mut diff_parts,
            ) {
                Ok(stats) => {
                    substitution_stats = Some(stats);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        Change::Regex(pattern, replacement) => {
            match apply_regex_change(
                repo_path,
                file_patterns,
                pattern,
                replacement,
                &mut transaction,
                &mut files_affected,
                &mut diff_parts,
            ) {
                Ok(stats) => {
                    substitution_stats = Some(stats);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
    };

    if let Err(e) = change_result {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),
            substitution_stats,

            error: Some(format!("Failed to apply changes: {e}")),
        };
    }

    // If no files were affected, return early
    if files_affected.is_empty() {
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),
            substitution_stats,

            error: None,
        };
    }

    // If no commit message, this is a dry run
    if commit_message.is_none() {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected,
            substitution_stats,

            error: None,
        };
    }

    // Create branch and commit changes
    let commit_result = commit_changes(
        repo_path,
        change_id,
        &original_branch,
        commit_message.unwrap(),
        &mut transaction,
    );

    match commit_result {
        Ok(()) => {
            let final_action = if let Some(pr) = pr {
                match create_pull_request(repo, change_id, commit_message.unwrap(), pr) {
                    Ok(()) => CreateAction::PrCreated,
                    Err(e) => {
                        warn!("Failed to create PR for {}: {}", repo.name, e);
                        CreateAction::Committed
                    }
                }
            } else {
                CreateAction::Committed
            };

            transaction.commit();
            CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: final_action,
                files_affected,
                substitution_stats,

                error: None,
            }
        }
        Err(e) => {
            transaction.rollback();
            CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected,
                substitution_stats,

                error: Some(format!("Failed to commit changes: {e}")),
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
    let full_path = repo_path.join(file_path);

    // Check if file already exists
    if full_path.exists() {
        return Err(eyre::eyre!("File already exists: {}", file_path));
    }

    // Create file and generate diff
    let (_, diff) = file::create_file_with_content(&full_path, content, 3)?;

    files_affected.push(file_path.to_string());
    diff_parts.push(format!(
        "  A {}\n{}",
        file_path,
        crate::utils::indent(&diff, 4)
    ));

    // Add rollback action to delete the created file
    let full_path_clone = full_path.clone();
    transaction.add_rollback(move || {
        if full_path_clone.exists() {
            std::fs::remove_file(&full_path_clone).with_context(|| {
                format!(
                    "Failed to rollback file creation: {}",
                    full_path_clone.display()
                )
            })?;
        }
        Ok(())
    });

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
    let mut all_files = Vec::new();

    // Find files matching all patterns
    for pattern in file_patterns {
        let files = file::find_files_in_repo(repo_path, pattern)?;
        all_files.extend(files);
    }

    // Remove duplicates
    all_files.sort();
    all_files.dedup();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Read content for diff
        let content = std::fs::read_to_string(&full_path).with_context(|| {
            format!("Failed to read file for deletion: {}", full_path.display())
        })?;

        // Create backup for rollback
        let backup_path = file::backup_file(&full_path)?;

        // Delete file
        file::delete_file(&full_path)?;

        let diff = diff::generate_diff(&content, "", 3);
        files_affected.push(file_path.to_string_lossy().to_string());
        diff_parts.push(format!(
            "  D {}\n{}",
            file_path.display(),
            crate::utils::indent(&diff, 4)
        ));

        // Add rollback action
        let backup_path_clone = backup_path.clone();
        let full_path_clone = full_path.clone();
        transaction
            .add_rollback(move || file::restore_from_backup(&backup_path_clone, &full_path_clone));
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
    let mut all_files = Vec::new();
    let mut stats = SubstitutionStats::default();

    // Find files matching all patterns
    for file_pattern in file_patterns {
        let files = file::find_files_in_repo(repo_path, file_pattern)?;
        all_files.extend(files);
    }

    // Remove duplicates
    all_files.sort();
    all_files.dedup();

    stats.files_scanned = all_files.len();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Try to apply substitution
        match file::apply_substitution_to_file(&full_path, pattern, replacement, 3)? {
            diff::SubstitutionResult::Changed(updated_content, diff) => {
                // Create backup for rollback
                let backup_path = file::backup_file(&full_path)?;

                // Write updated content
                file::write_file_content(&full_path, &updated_content)?;

                files_affected.push(file_path.to_string_lossy().to_string());
                diff_parts.push(format!(
                    "  M {}\n{}",
                    file_path.display(),
                    crate::utils::indent(&diff, 4)
                ));

                // Add rollback action
                let backup_path_clone = backup_path.clone();
                let full_path_clone = full_path.clone();
                transaction.add_rollback(move || {
                    file::restore_from_backup(&backup_path_clone, &full_path_clone)
                });

                stats.files_changed += 1;
                // Count matches in the original content
                let original_content = std::fs::read_to_string(&full_path).unwrap_or_default();
                stats.total_matches += original_content.matches(pattern).count();
            }
            diff::SubstitutionResult::NoMatches => {
                // Pattern didn't match anything in this file
                debug!("No matches found for pattern '{}' in {}", pattern, file_path.display());
                stats.files_no_matches += 1;
            }
            diff::SubstitutionResult::NoChange => {
                // Pattern matched but replacement resulted in no changes
                debug!("Pattern '{}' matched but no changes resulted in {}", pattern, file_path.display());
                stats.files_no_change += 1;
                // Count matches even though no changes were made
                let original_content = std::fs::read_to_string(&full_path).unwrap_or_default();
                stats.total_matches += original_content.matches(pattern).count();
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
    let mut all_files = Vec::new();
    let mut stats = SubstitutionStats::default();

    // Find files matching all patterns
    for file_pattern in file_patterns {
        let files = file::find_files_in_repo(repo_path, file_pattern)?;
        all_files.extend(files);
    }

    // Remove duplicates
    all_files.sort();
    all_files.dedup();

    stats.files_scanned = all_files.len();

    for file_path in all_files {
        let full_path = repo_path.join(&file_path);

        if !full_path.exists() {
            continue;
        }

        // Try to apply regex substitution
        match file::apply_regex_to_file(&full_path, pattern, replacement, 3)? {
            diff::SubstitutionResult::Changed(updated_content, diff) => {
                // Create backup for rollback
                let backup_path = file::backup_file(&full_path)?;

                // Write updated content
                file::write_file_content(&full_path, &updated_content)?;

                files_affected.push(file_path.to_string_lossy().to_string());
                diff_parts.push(format!(
                    "  M {}\n{}",
                    file_path.display(),
                    crate::utils::indent(&diff, 4)
                ));

                // Add rollback action
                let backup_path_clone = backup_path.clone();
                let full_path_clone = full_path.clone();
                transaction.add_rollback(move || {
                    file::restore_from_backup(&backup_path_clone, &full_path_clone)
                });

                stats.files_changed += 1;
                // Count regex matches in the original content
                let original_content = std::fs::read_to_string(&full_path).unwrap_or_default();
                if let Ok(regex) = regex::Regex::new(pattern) {
                    stats.total_matches += regex.find_iter(&original_content).count();
                }
            }
            diff::SubstitutionResult::NoMatches => {
                // Pattern didn't match anything in this file
                debug!("No matches found for regex pattern '{}' in {}", pattern, file_path.display());
                stats.files_no_matches += 1;
            }
            diff::SubstitutionResult::NoChange => {
                // Pattern matched but replacement resulted in no changes
                debug!("Regex pattern '{}' matched but no changes resulted in {}", pattern, file_path.display());
                stats.files_no_change += 1;
                // Count matches even though no changes were made
                let original_content = std::fs::read_to_string(&full_path).unwrap_or_default();
                if let Ok(regex) = regex::Regex::new(pattern) {
                    stats.total_matches += regex.find_iter(&original_content).count();
                }
            }
        }
    }

    Ok(stats)
}

/// Commit changes to a new branch
fn commit_changes(
    repo_path: &Path,
    change_id: &str,
    original_branch: &str,
    commit_message: &str,
    transaction: &mut Transaction,
) -> Result<()> {
    // Check if branch existed before we try to create it
    let branch_existed = git::branch_exists_locally(repo_path, change_id)
        .unwrap_or(false);

    // Create and switch to branch (or switch to existing)
    git::create_branch(repo_path, change_id)
        .with_context(|| format!("Failed to create or switch to branch: {change_id}"))?;

    // Add rollback to switch back to original branch
    let original_branch = original_branch.to_string();
    let repo_path_clone = repo_path.to_path_buf();
    let change_id_clone = change_id.to_string();
    transaction.add_rollback(move || {
        // Switch back to original branch
        if let Err(e) = git::switch_branch(&repo_path_clone, &original_branch) {
            warn!("Failed to switch back to original branch {original_branch}: {e}");
        }

        // Only delete the branch if we created it (not if it existed before)
        if !branch_existed {
            if let Err(e) = git::delete_local_branch(&repo_path_clone, &change_id_clone) {
                warn!("Failed to delete branch {change_id_clone}: {e}");
            }
        }

        Ok(())
    });

    // Stage all changes
    git::add_all_changes(repo_path).context("Failed to stage changes")?;

    // Commit changes
    git::commit_changes(repo_path, commit_message).context("Failed to commit changes")?;

    // Push branch to remote
    git::push_branch(repo_path, change_id).context("Failed to push branch")?;

    Ok(())
}

/// Create a pull request for the changes
fn create_pull_request(
    repo: &Repo,
    change_id: &str,
    commit_message: &str,
    pr: &crate::cli::PR,
) -> Result<()> {
    if let Some(repo_slug) = &repo.slug {
        github::create_pr(repo_slug, change_id, commit_message, pr)
            .with_context(|| format!("Failed to create PR for {repo_slug}"))?;
        info!("Created PR for repository: {repo_slug}");
    } else {
        return Err(eyre::eyre!(
            "Repository {} has no slug, cannot create PR",
            repo.name
        ));
    }
    Ok(())
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

            if files_changed == 0 && total_files_scanned > 0 {
                println!("   ⚠️  No files were modified by the pattern");
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

    let dry_runs = results
        .iter()
        .filter(|r| matches!(r.action, CreateAction::DryRun))
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
        println!("   👁️  {dry_runs} dry runs");
        println!("   💾 {committed} committed");
        println!("   📥 {prs_created} PRs created");
        println!("   📄 {total_files} files affected");
        if errors > 0 {
            println!("   ❌ {errors} errors");
        }
    } else {
        println!("\nSummary: {total} repositories processed:");
        println!("   {dry_runs} dry runs");
        println!("   {committed} committed");
        println!("   {prs_created} PRs created");
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
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_generate_change_id() {
        let change_id = generate_change_id();
        assert!(change_id.starts_with("GX-"));
        assert!(change_id.len() > 10); // Should have timestamp
    }

    #[test]
    fn test_change_debug() {
        let add = Change::Add("test.txt".to_string(), "content".to_string());
        let delete = Change::Delete;
        let sub = Change::Sub("old".to_string(), "new".to_string());
        let regex = Change::Regex(r"\d+".to_string(), "X".to_string());

        // Ensure Debug is implemented
        assert!(!format!("{add:?}").is_empty());
        assert!(!format!("{delete:?}").is_empty());
        assert!(!format!("{sub:?}").is_empty());
        assert!(!format!("{regex:?}").is_empty());
    }

    #[test]
    fn test_create_result_debug() {
        let temp_dir = TempDir::new().unwrap();
        let repo = Repo::new(temp_dir.path().to_path_buf());

        let result = CreateResult {
            repo,
            change_id: "test-change".to_string(),
            action: CreateAction::DryRun,
            files_affected: vec!["test.txt".to_string()],
            substitution_stats: None,

            error: None,
        };

        let debug_str = format!("{result:?}");
        assert!(debug_str.contains("test-change"));
        assert!(debug_str.contains("DryRun"));
    }

    #[test]
    fn test_create_action_debug() {
        let actions = vec![
            CreateAction::DryRun,
            CreateAction::Committed,
            CreateAction::Committed,
            CreateAction::PrCreated,
        ];

        for action in actions {
            assert!(!format!("{action:?}").is_empty());
        }
    }

    #[test]
    fn test_apply_add_change() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();
        let mut transaction = Transaction::new();
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

        let mut transaction = Transaction::new();
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

        let mut transaction = Transaction::new();
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

        let mut transaction = Transaction::new();
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
}
