use crate::config::Config;
use crate::diff;
use crate::file;
use crate::git;
use crate::github;
use crate::output::{display_unified_results, StatusOptions};
use crate::repo::{discover_repos, filter_repos, Repo};
use crate::transaction::Transaction;
use crate::cli::Cli;
use chrono::Local;
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum Change {
    Add(String, String),    // path, content
    Delete,                 // delete matched files
    Sub(String, String),    // pattern, replacement
    Regex(String, String),  // regex pattern, replacement
}

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub repo: Repo,
    pub change_id: String,
    pub action: CreateAction,
    pub files_affected: Vec<String>,

    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CreateAction {
    DryRun,           // No changes made (preview)

    Committed,        // Changes committed to branch
    PrCreated,        // PR created successfully
}

/// Generate a default change ID based on current timestamp
pub fn generate_change_id() -> String {
    let now = Local::now();
    let timestamp = now.format("%Y-%m-%dT%H-%M-%S").to_string();
    format!("GX-{}", timestamp)
}

/// Process create command across multiple repositories
pub fn process_create_command(
    cli: &Cli,
    config: &Config,
    files: &[String],
    change_id: Option<String>,
    patterns: &[String],
    commit_message: Option<String>,
    create_pr: bool,
    change: Change,
) -> Result<()> {
    info!("Starting create command with change: {:?}", change);

    let change_id = change_id.unwrap_or_else(generate_change_id);
    let current_dir = std::env::current_dir()?;
    let start_dir = cli.cwd.as_deref().unwrap_or(&current_dir);
    let max_depth = cli.max_depth
        .or_else(|| config.repo_discovery.as_ref().and_then(|rd| rd.max_depth))
        .unwrap_or(3);

    // Discover and filter repositories
    let repos = discover_repos(start_dir, max_depth)
        .context("Failed to discover repositories")?;

    info!("Discovered {} repositories", repos.len());

    let filtered_repos = filter_repos(repos, patterns);
    info!("Filtered to {} repositories matching patterns", filtered_repos.len());

    if filtered_repos.is_empty() {
        println!("No repositories found matching the specified patterns.");
        return Ok(());
    }

    // Determine parallelism
    let parallel_jobs = cli.parallel
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
                    create_pr,
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
    create_pr: bool,
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

                error: Some("Repository has uncommitted changes. Please commit or stash them first.".to_string()),
            };
        }
        Ok(false) => {} // Good, no uncommitted changes
        Err(e) => {
            return CreateResult {
                repo: repo.clone(),
                change_id: change_id.to_string(),
                action: CreateAction::DryRun,
                files_affected: Vec::new(),

                error: Some(format!("Failed to check repository status: {}", e)),
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

                error: Some(format!("Failed to get current branch: {}", e)),
            };
        }
    };

    // Apply changes based on change type
    let change_result = match change {
        Change::Add(path, content) => {
            apply_add_change(repo_path, path, content, &mut transaction, &mut files_affected, &mut diff_parts)
        }
        Change::Delete => {
            apply_delete_change(repo_path, file_patterns, &mut transaction, &mut files_affected, &mut diff_parts)
        }
        Change::Sub(pattern, replacement) => {
            apply_substitution_change(repo_path, file_patterns, pattern, replacement, &mut transaction, &mut files_affected, &mut diff_parts)
        }
        Change::Regex(pattern, replacement) => {
            apply_regex_change(repo_path, file_patterns, pattern, replacement, &mut transaction, &mut files_affected, &mut diff_parts)
        }
    };

    if let Err(e) = change_result {
        transaction.rollback();
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),

            error: Some(format!("Failed to apply changes: {}", e)),
        };
    }

    // If no files were affected, return early
    if files_affected.is_empty() {
        return CreateResult {
            repo: repo.clone(),
            change_id: change_id.to_string(),
            action: CreateAction::DryRun,
            files_affected: Vec::new(),

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
            let final_action = if create_pr {
                match create_pull_request(repo, change_id, commit_message.unwrap()) {
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

                error: Some(format!("Failed to commit changes: {}", e)),
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
    diff_parts.push(format!("  A {}\n{}", file_path, crate::utils::indent(&diff, 4)));

    // Add rollback action to delete the created file
    let full_path_clone = full_path.clone();
    transaction.add_rollback(move || {
        if full_path_clone.exists() {
            std::fs::remove_file(&full_path_clone)
                .with_context(|| format!("Failed to rollback file creation: {}", full_path_clone.display()))?;
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
        let content = std::fs::read_to_string(&full_path)
            .with_context(|| format!("Failed to read file for deletion: {}", full_path.display()))?;

        // Create backup for rollback
        let backup_path = file::backup_file(&full_path)?;

        // Delete file
        file::delete_file(&full_path)?;

        let diff = diff::generate_diff(&content, "", 3);
        files_affected.push(file_path.to_string_lossy().to_string());
        diff_parts.push(format!("  D {}\n{}", file_path.display(), crate::utils::indent(&diff, 4)));

        // Add rollback action
        let backup_path_clone = backup_path.clone();
        let full_path_clone = full_path.clone();
        transaction.add_rollback(move || {
            file::restore_from_backup(&backup_path_clone, &full_path_clone)
        });
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
) -> Result<()> {
    let mut all_files = Vec::new();

    // Find files matching all patterns
    for file_pattern in file_patterns {
        let files = file::find_files_in_repo(repo_path, file_pattern)?;
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

        // Try to apply substitution
        if let Some((updated_content, diff)) = file::apply_substitution_to_file(&full_path, pattern, replacement, 3)? {
            // Create backup for rollback
            let backup_path = file::backup_file(&full_path)?;

            // Write updated content
            file::write_file_content(&full_path, &updated_content)?;

            files_affected.push(file_path.to_string_lossy().to_string());
            diff_parts.push(format!("  M {}\n{}", file_path.display(), crate::utils::indent(&diff, 4)));

            // Add rollback action
            let backup_path_clone = backup_path.clone();
            let full_path_clone = full_path.clone();
            transaction.add_rollback(move || {
                file::restore_from_backup(&backup_path_clone, &full_path_clone)
            });
        }
    }

    Ok(())
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
) -> Result<()> {
    let mut all_files = Vec::new();

    // Find files matching all patterns
    for file_pattern in file_patterns {
        let files = file::find_files_in_repo(repo_path, file_pattern)?;
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

        // Try to apply regex substitution
        if let Some((updated_content, diff)) = file::apply_regex_to_file(&full_path, pattern, replacement, 3)? {
            // Create backup for rollback
            let backup_path = file::backup_file(&full_path)?;

            // Write updated content
            file::write_file_content(&full_path, &updated_content)?;

            files_affected.push(file_path.to_string_lossy().to_string());
            diff_parts.push(format!("  M {}\n{}", file_path.display(), crate::utils::indent(&diff, 4)));

            // Add rollback action
            let backup_path_clone = backup_path.clone();
            let full_path_clone = full_path.clone();
            transaction.add_rollback(move || {
                file::restore_from_backup(&backup_path_clone, &full_path_clone)
            });
        }
    }

    Ok(())
}

/// Commit changes to a new branch
fn commit_changes(
    repo_path: &Path,
    change_id: &str,
    original_branch: &str,
    commit_message: &str,
    transaction: &mut Transaction,
) -> Result<()> {
    // Create and switch to new branch
    git::create_branch(repo_path, change_id)
        .with_context(|| format!("Failed to create branch: {}", change_id))?;

    // Add rollback to switch back to original branch
    let original_branch = original_branch.to_string();
    let repo_path_clone = repo_path.to_path_buf();
    let change_id_clone = change_id.to_string();
    transaction.add_rollback(move || {
        // Switch back to original branch
        if let Err(e) = git::switch_branch(&repo_path_clone, &original_branch) {
            warn!("Failed to switch back to original branch {}: {}", original_branch, e);
        }

        // Delete the created branch
        if let Err(e) = git::delete_local_branch(&repo_path_clone, &change_id_clone) {
            warn!("Failed to delete branch {}: {}", change_id_clone, e);
        }

        Ok(())
    });

    // Stage all changes
    git::add_all_changes(repo_path)
        .context("Failed to stage changes")?;

    // Commit changes
    git::commit_changes(repo_path, commit_message)
        .context("Failed to commit changes")?;

    // Push branch to remote
    git::push_branch(repo_path, change_id)
        .context("Failed to push branch")?;

    Ok(())
}

/// Create a pull request for the changes
fn create_pull_request(repo: &Repo, change_id: &str, commit_message: &str) -> Result<()> {
    if let Some(repo_slug) = &repo.slug {
        github::create_pr(repo_slug, change_id, commit_message)
            .with_context(|| format!("Failed to create PR for {}", repo_slug))?;
        info!("Created PR for repository: {}", repo_slug);
    } else {
        return Err(eyre::eyre!("Repository {} has no slug, cannot create PR", repo.name));
    }
    Ok(())
}

/// Display summary of create results
fn display_create_summary(results: &[CreateResult], opts: &StatusOptions) {
    let total = results.len();
    let successful = results.iter().filter(|r| r.error.is_none()).count();
    let errors = total - successful;

    let dry_runs = results.iter().filter(|r| matches!(r.action, CreateAction::DryRun)).count();
    let committed = results.iter().filter(|r| matches!(r.action, CreateAction::Committed)).count();
    let prs_created = results.iter().filter(|r| matches!(r.action, CreateAction::PrCreated)).count();

    let total_files: usize = results.iter().map(|r| r.files_affected.len()).sum();

    if opts.use_emoji {
        println!("\n📊 {} repositories processed:", total);
        println!("   👁️  {} dry runs", dry_runs);
        println!("   💾 {} committed", committed);
        println!("   📥 {} PRs created", prs_created);
        println!("   📄 {} files affected", total_files);
        if errors > 0 {
            println!("   ❌ {} errors", errors);
        }
    } else {
        println!("\nSummary: {} repositories processed:", total);
        println!("   {} dry runs", dry_runs);
        println!("   {} committed", committed);
        println!("   {} PRs created", prs_created);
        println!("   {} files affected", total_files);
        if errors > 0 {
            println!("   {} errors", errors);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

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
        assert!(!format!("{:?}", add).is_empty());
        assert!(!format!("{:?}", delete).is_empty());
        assert!(!format!("{:?}", sub).is_empty());
        assert!(!format!("{:?}", regex).is_empty());
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

            error: None,
        };

        let debug_str = format!("{:?}", result);
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
            assert!(!format!("{:?}", action).is_empty());
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
        assert!(result.unwrap_err().to_string().contains("File already exists"));
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
