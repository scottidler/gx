//! Branch cleanup after PR merge
//!
//! Provides functionality to clean up local and remote branches
//! after PRs have been merged.

use crate::cli::Cli;
use crate::config::Config;
use crate::confirm::{confirm_destructive, DestructiveOp};
use crate::git;
use crate::lock::{ChangeLock, RepoLock};
use crate::state::{ChangeState, ChangeStatus, RepoChangeStatus, StateManager};
use eyre::{Context, Result};
use log::{debug, info, warn};

/// Count the local branches a cleanup pass would actually `git branch -D` for
/// one change: repos needing cleanup that are eligible under the `force` gate
/// (merged-only unless `--force`). This is the true blast radius the confirm
/// gate reports - not the raw repo count.
fn eligible_cleanup_count(state: &ChangeState, force: bool) -> usize {
    state
        .get_repos_needing_cleanup()
        .iter()
        .filter(|r| force || r.status == RepoChangeStatus::PrMerged)
        .count()
}

/// Result of a cleanup operation
#[derive(Debug)]
pub struct CleanupResult {
    #[allow(dead_code)]
    pub change_id: String,
    pub repos_cleaned: usize,
    pub repos_skipped: usize,
    pub repos_failed: usize,
    pub errors: Vec<String>,
}

/// Process cleanup command
#[allow(clippy::too_many_arguments)]
pub fn process_cleanup_command(
    _cli: &Cli,
    config: &Config,
    change_id: Option<&str>,
    all: bool,
    list: bool,
    include_remote: bool,
    force: bool,
    yes: bool,
) -> Result<()> {
    let state_manager = StateManager::new()?;

    if list {
        return list_cleanable_changes(&state_manager);
    }

    if all {
        return cleanup_all_merged(&state_manager, config, include_remote, force, yes);
    }

    let change_id = change_id
        .ok_or_else(|| eyre::eyre!("Change ID required unless --all or --list is specified"))?;

    cleanup_single_change(
        &state_manager,
        config,
        change_id,
        include_remote,
        force,
        yes,
    )
}

/// List changes that can be cleaned up
fn list_cleanable_changes(state_manager: &StateManager) -> Result<()> {
    let states = state_manager.list()?;

    let cleanable: Vec<_> = states
        .iter()
        .filter(|s| {
            s.status == ChangeStatus::FullyMerged || s.status == ChangeStatus::PartiallyMerged
        })
        .collect();

    if cleanable.is_empty() {
        println!("No changes need cleanup.");
        return Ok(());
    }

    println!("Changes available for cleanup:\n");
    for state in cleanable {
        let repos_needing_cleanup = state.get_repos_needing_cleanup().len();
        let open_prs = state.get_open_prs().len();
        let total_repos = state.repositories.len();
        let merged = state
            .repositories
            .values()
            .filter(|r| r.status == RepoChangeStatus::PrMerged)
            .count();

        println!(
            "  📦 {} ({} repos, {} merged, {} open, {} need cleanup)",
            state.change_id, total_repos, merged, open_prs, repos_needing_cleanup
        );

        if let Some(desc) = &state.description {
            println!("     {}", desc);
        }
    }

    println!("\nRun `gx cleanup <change-id>` to clean up a specific change.");
    println!("Run `gx cleanup --all` to clean up all merged changes.");

    Ok(())
}

/// Clean up all merged changes
fn cleanup_all_merged(
    state_manager: &StateManager,
    config: &Config,
    include_remote: bool,
    force: bool,
    yes: bool,
) -> Result<()> {
    let states = state_manager.list()?;

    let cleanable: Vec<_> = states
        .into_iter()
        .filter(|s| {
            s.status == ChangeStatus::FullyMerged
                || (force && s.status == ChangeStatus::PartiallyMerged)
        })
        .collect();

    if cleanable.is_empty() {
        println!("No changes to clean up.");
        return Ok(());
    }

    // Confirm gate (Phase 3): the true blast radius is every local branch this
    // pass would `-D` across all cleanable changes. Prompt only once that count
    // reaches the threshold; fail closed on non-interactive stdin without
    // `--yes`.
    let total_branches: usize = cleanable
        .iter()
        .map(|s| eligible_cleanup_count(s, force))
        .sum();
    let threshold = config.cleanup_confirm_threshold();
    if total_branches >= threshold
        && !confirm_destructive(DestructiveOp::Cleanup, total_branches, yes)?
    {
        println!("Aborted; no branches deleted.");
        return Ok(());
    }

    println!("Cleaning up {} change(s)...\n", cleanable.len());

    let mut total_cleaned = 0;
    let mut total_skipped = 0;
    let mut total_failed = 0;

    for candidate in cleanable {
        let change_id = candidate.change_id.clone();

        // Change-level lock (Phase 7 [F6]), then reload fresh: `list()` above
        // may have read this change's state before another process's
        // read-modify-write landed, so the listing copy is discarded and the
        // authoritative load happens under the lock, right before mutating.
        let _change_lock = match ChangeLock::acquire(&change_id) {
            Ok(lock) => lock,
            Err(e) => {
                warn!("Failed to cleanup {change_id}: change is locked: {e}");
                total_failed += 1;
                continue;
            }
        };
        let mut state = match state_manager.load(&change_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!("Change {change_id} disappeared before cleanup");
                continue;
            }
            Err(e) => {
                warn!("Failed to reload {change_id} before cleanup: {e}");
                total_failed += 1;
                continue;
            }
        };

        match cleanup_change(&mut state, include_remote, force) {
            Ok(result) => {
                total_cleaned += result.repos_cleaned;
                total_skipped += result.repos_skipped;
                total_failed += result.repos_failed;

                // Update state
                state_manager.save(&state)?;

                // Delete state if fully cleaned
                if state.get_repos_needing_cleanup().is_empty() {
                    state_manager.delete(&state.change_id)?;
                }
            }
            Err(e) => {
                warn!("Failed to cleanup {change_id}: {e}");
                total_failed += 1;
            }
        }
    }

    println!("\n📊 Cleanup summary:");
    println!("   🧹 {} branches cleaned", total_cleaned);
    println!("   ⏩  {} skipped", total_skipped);
    if total_failed > 0 {
        println!("   ❌ {} failed", total_failed);
    }

    Ok(())
}

/// Clean up a single change
fn cleanup_single_change(
    state_manager: &StateManager,
    config: &Config,
    change_id: &str,
    include_remote: bool,
    force: bool,
    yes: bool,
) -> Result<()> {
    // Change-level lock (Phase 7 [F6]): held across the whole load-mutate-save
    // cycle below so a concurrent `review sync`/`approve`/`delete`/`undo` on
    // this change-id can't interleave and lose an update.
    let _change_lock = ChangeLock::acquire(change_id)
        .with_context(|| format!("Failed to acquire change lock for {change_id}"))?;

    let mut state = state_manager
        .load(change_id)?
        .ok_or_else(|| eyre::eyre!("Change not found: {}", change_id))?;

    // Confirm gate (Phase 3): prompt once the branch count reaches the
    // threshold, fail closed on non-interactive stdin without `--yes`. Held
    // under the change lock but before any `-D` runs.
    let branch_count = eligible_cleanup_count(&state, force);
    let threshold = config.cleanup_confirm_threshold();
    if branch_count >= threshold && !confirm_destructive(DestructiveOp::Cleanup, branch_count, yes)?
    {
        println!("Aborted; no branches deleted.");
        return Ok(());
    }

    let result = cleanup_change(&mut state, include_remote, force)?;

    // Update state
    state_manager.save(&state)?;

    // Print summary
    println!("\n📊 Cleanup for {}:", change_id);
    println!("   🧹 {} branches cleaned", result.repos_cleaned);
    println!("   ⏩  {} skipped", result.repos_skipped);
    if result.repos_failed > 0 {
        println!("   ❌ {} failed", result.repos_failed);
        for error in &result.errors {
            println!("      - {}", error);
        }
    }

    // Delete state if fully cleaned
    if state.get_repos_needing_cleanup().is_empty() && result.repos_failed == 0 {
        state_manager.delete(change_id)?;
        println!("   ✅ Change state removed");
        // Retention (design Data Model): the proposal dir is removed when the
        // change reaches its cleaned-up terminal. Best-effort + idempotent; a
        // change that never had a proposal is a harmless no-op.
        if let Err(e) = crate::create::manifest::remove_proposal_dir(change_id) {
            warn!("Failed to remove proposal artifacts for {change_id}: {e}");
        }
    }

    Ok(())
}

/// Clean up branches for a change
fn cleanup_change(
    state: &mut ChangeState,
    include_remote: bool,
    force: bool,
) -> Result<CleanupResult> {
    // Get repos needing cleanup - collect into owned data
    let repos_to_clean: Vec<_> = state
        .get_repos_needing_cleanup()
        .iter()
        .map(|r| {
            (
                r.repo_slug.clone(),
                r.branch_name.clone(),
                r.status.clone(),
                r.local_path.clone(),
            )
        })
        .collect();

    let mut cleaned = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for (repo_slug, branch_name, status, recorded_path) in repos_to_clean {
        // Check if we should clean this repo
        if !force && status != RepoChangeStatus::PrMerged {
            info!("Skipping {} - PR not merged", repo_slug);
            skipped += 1;
            continue;
        }

        // Resolve the repo via its recorded local_path first ([A16]); fall back
        // to a CWD search only when no path was recorded. A recorded-but-missing
        // path is reported as a failure, not silently skipped.
        let local_path = match recorded_path {
            Some(path) => {
                let path = std::path::PathBuf::from(&path);
                // Layout-aware: accept a flat repo (`.git` dir), a linked
                // worktree (`.git` pointer file), or a bare container.
                if crate::bare::is_git_path(&path) {
                    path
                } else {
                    warn!(
                        "Recorded path for {} no longer a git repo: {}",
                        repo_slug,
                        path.display()
                    );
                    errors.push(format!(
                        "{}: recorded path missing: {}",
                        repo_slug,
                        path.display()
                    ));
                    failed += 1;
                    continue;
                }
            }
            None => match find_repo_locally(&repo_slug) {
                Some(p) => p,
                None => {
                    info!("Skipping {} - local repo not found", repo_slug);
                    skipped += 1;
                    continue;
                }
            },
        };

        // Per-repo lock (Phase 7 [F6]): a second concurrent gx invocation must
        // not interleave a branch delete with any other mutation on this repo.
        let _lock = match RepoLock::acquire(&local_path) {
            Ok(lock) => lock,
            Err(e) => {
                warn!("Repository locked, skipping cleanup for {repo_slug}: {e}");
                errors.push(format!("{repo_slug}: repository is locked: {e}"));
                failed += 1;
                continue;
            }
        };

        // Fetched-ancestry guard (Phase 4): even for a `PrMerged` repo, PROVE
        // the branch's commits are all contained in the freshly-fetched base ref
        // before `-D`. `PrMerged` stays a fast-path signal, but the git-level
        // ancestry check is the real guard against deleting unmerged work.
        // `--force` bypasses it (the operator explicitly opted into an
        // unconditional delete); on Ok(false) or a verification error we fail
        // CLOSED and PRESERVE the branch.
        if !force {
            match git::branch_merged_into_base(&local_path, &branch_name) {
                Ok(true) => {}
                Ok(false) => {
                    warn!(
                        "Skipping {repo_slug}: branch {branch_name} has commits not in the base branch; re-check the merge or use --force"
                    );
                    skipped += 1;
                    continue;
                }
                Err(e) => {
                    warn!(
                        "Skipping {repo_slug}: could not verify {branch_name} is merged into the base branch: {e}"
                    );
                    errors.push(format!("{repo_slug}: ancestry check failed: {e}"));
                    failed += 1;
                    continue;
                }
            }
        }

        // Delete local branch. Existence is checked explicitly FIRST (F13) so
        // an already-deleted branch is a no-op rather than the caller sniffing
        // the delete error's text for "not found"/"does not exist".
        match git::branch_exists_locally(&local_path, &branch_name) {
            Ok(true) => match git::delete_local_branch(&local_path, &branch_name) {
                Ok(()) => {
                    info!("🧹 Deleted local branch {} in {}", branch_name, repo_slug);
                    cleaned += 1;
                    state.mark_cleaned_up(&repo_slug);
                }
                Err(e) => {
                    warn!(
                        "Failed to delete branch {} in {}: {}",
                        branch_name, repo_slug, e
                    );
                    errors.push(format!("{}: {}", repo_slug, e));
                    failed += 1;
                }
            },
            Ok(false) => {
                debug!("Branch {branch_name} already deleted in {repo_slug}");
                state.mark_cleaned_up(&repo_slug);
                skipped += 1;
            }
            Err(e) => {
                warn!("Failed to check local branch {branch_name} in {repo_slug}: {e}");
                errors.push(format!("{repo_slug}: {e}"));
                failed += 1;
            }
        }

        // Optionally delete remote branch. `git::delete_remote_branch` already
        // pre-probes existence (F13), so an already-absent branch is a silent
        // no-op; only a genuine failure is worth a warning here.
        if include_remote {
            if let Err(e) = git::delete_remote_branch(&local_path, &branch_name) {
                warn!(
                    "Failed to delete remote branch {} in {}: {}",
                    branch_name, repo_slug, e
                );
            }
        }
    }

    Ok(CleanupResult {
        change_id: state.change_id.clone(),
        repos_cleaned: cleaned,
        repos_skipped: skipped,
        repos_failed: failed,
        errors,
    })
}

/// Try to find a repository locally by slug
fn find_repo_locally(repo_slug: &str) -> Option<std::path::PathBuf> {
    // Extract repo name from slug
    let repo_name = repo_slug.split('/').next_back()?;

    // Check current directory and parent directories
    let current = std::env::current_dir().ok()?;

    // Try: ./repo_name
    let direct = current.join(repo_name);
    if direct.join(".git").exists() {
        return Some(direct);
    }

    // Try: ./org/repo_name (full slug path)
    let with_org = current.join(repo_slug);
    if with_org.join(".git").exists() {
        return Some(with_org);
    }

    // Try: look in subdirectories matching org name
    if let Some(org) = repo_slug.split('/').next() {
        let org_dir = current.join(org);
        if org_dir.is_dir() {
            let repo_in_org = org_dir.join(repo_name);
            if repo_in_org.join(".git").exists() {
                return Some(repo_in_org);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChangeState, RepoChangeStatus};
    use crate::test_utils::run_git_command;
    use tempfile::TempDir;

    #[test]
    fn test_find_repo_locally_not_found() {
        let result = find_repo_locally("nonexistent/repo");
        assert!(result.is_none());
    }

    #[test]
    fn test_cleanup_uses_recorded_local_path() {
        // cleanup_change must resolve repos via the recorded local_path, so it
        // works from any CWD (not just one containing the repo) ([A16]).
        let repo = TempDir::new().unwrap();
        let p = repo.path();
        run_git_command(&["init", "--quiet"], p);
        run_git_command(&["config", "user.email", "t@e.com"], p);
        run_git_command(&["config", "user.name", "T"], p);
        run_git_command(&["config", "commit.gpgsign", "false"], p);
        std::fs::write(p.join("README.md"), "# r").unwrap();
        run_git_command(&["add", "-A"], p);
        run_git_command(&["commit", "--quiet", "-m", "init"], p);
        run_git_command(&["branch", "GX-cleanup"], p);
        assert!(crate::git::branch_exists_locally(p, "GX-cleanup").unwrap());

        let mut state = ChangeState::new("GX-cleanup".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-cleanup".to_string());
        {
            let rs = state.repositories.get_mut("org/repo").unwrap();
            rs.local_path = Some(p.to_string_lossy().to_string());
            rs.status = RepoChangeStatus::PrMerged;
        }

        // force=true so it cleans even though we didn't go through a real merge.
        let result = cleanup_change(&mut state, false, true).unwrap();
        assert_eq!(result.repos_cleaned, 1);
        assert!(!crate::git::branch_exists_locally(p, "GX-cleanup").unwrap());
    }

    #[test]
    fn test_cleanup_change_empty_state() {
        let mut state = ChangeState::new("test".to_string(), None);
        let result = cleanup_change(&mut state, false, false).unwrap();

        assert_eq!(result.repos_cleaned, 0);
        assert_eq!(result.repos_skipped, 0);
        assert_eq!(result.repos_failed, 0);
    }

    #[test]
    fn test_cleanup_change_with_repos_not_found() {
        let mut state = ChangeState::new("test".to_string(), None);
        state.add_repository("nonexistent/repo".to_string(), "GX-test".to_string());
        // Set status to merged so it would be eligible for cleanup
        state
            .repositories
            .get_mut("nonexistent/repo")
            .unwrap()
            .status = RepoChangeStatus::PrMerged;

        let result = cleanup_change(&mut state, false, false).unwrap();

        // Should skip because local repo not found
        assert_eq!(result.repos_cleaned, 0);
        assert_eq!(result.repos_skipped, 1);
        assert_eq!(result.repos_failed, 0);
    }

    #[test]
    fn test_list_cleanable_changes_empty() {
        let temp_dir = TempDir::new().unwrap();
        let manager = StateManager::with_dir(temp_dir.path().to_path_buf());

        // Should not error on empty state
        let result = list_cleanable_changes(&manager);
        assert!(result.is_ok());
    }

    /// The confirm gate's blast-radius count is the branches a cleanup would
    /// actually `-D`: merged-only without `--force`, merged+closed with it.
    #[test]
    fn test_eligible_cleanup_count_respects_force_and_merged() {
        let mut state = ChangeState::new("GX-count".to_string(), None);
        state.add_repository("org/merged".to_string(), "GX-count".to_string());
        state.add_repository("org/closed".to_string(), "GX-count".to_string());
        state.repositories.get_mut("org/merged").unwrap().status = RepoChangeStatus::PrMerged;
        state.repositories.get_mut("org/closed").unwrap().status = RepoChangeStatus::PrClosed;

        // Without --force, only the MERGED repo is eligible for -D.
        assert_eq!(eligible_cleanup_count(&state, false), 1);
        // With --force, both needing-cleanup repos (merged + closed) are eligible.
        assert_eq!(eligible_cleanup_count(&state, true), 2);
    }

    /// Fetched-ancestry guard bite (Phase 4): `cleanup` WITHOUT `--force` must
    /// PRESERVE a branch whose commits are NOT contained in the freshly-fetched
    /// base ref - even when the recorded status is `PrMerged` (the fast-path
    /// signal is trusted no further than the git-level ancestry check). The
    /// branch carries a commit that never landed on the base; the ancestry check
    /// returns "not an ancestor", so the branch survives. Remove the guard and
    /// the branch is `-D`'d (test fails).
    #[test]
    fn test_cleanup_preserves_branch_with_commits_absent_from_base() {
        // An upstream repo with a base branch, cloned locally; the gx branch then
        // gets an EXTRA local commit that never merged upstream.
        let parent = TempDir::new().unwrap();
        let upstream = parent.path().join("upstream");
        std::fs::create_dir(&upstream).unwrap();
        run_git_command(&["init", "--quiet"], &upstream);
        run_git_command(&["config", "user.email", "t@e.com"], &upstream);
        run_git_command(&["config", "user.name", "T"], &upstream);
        run_git_command(&["config", "commit.gpgsign", "false"], &upstream);
        std::fs::write(upstream.join("README.md"), "# base").unwrap();
        run_git_command(&["add", "-A"], &upstream);
        run_git_command(&["commit", "--quiet", "-m", "base commit"], &upstream);

        // Clone so origin/HEAD (the resolved base) and an `origin` remote exist.
        let work = parent.path().join("work");
        run_git_command(
            &[
                "clone",
                "--quiet",
                &upstream.to_string_lossy(),
                &work.to_string_lossy(),
            ],
            parent.path(),
        );
        run_git_command(&["config", "user.email", "t@e.com"], &work);
        run_git_command(&["config", "user.name", "T"], &work);
        run_git_command(&["config", "commit.gpgsign", "false"], &work);

        // A gx branch with an extra commit that is NOT on the base branch.
        run_git_command(&["checkout", "--quiet", "-b", "GX-unmerged"], &work);
        std::fs::write(work.join("extra.txt"), "unmerged work").unwrap();
        run_git_command(&["add", "-A"], &work);
        run_git_command(&["commit", "--quiet", "-m", "unmerged commit"], &work);
        assert!(crate::git::branch_exists_locally(&work, "GX-unmerged").unwrap());

        // Recorded as PrMerged (the fast-path signal) - the ancestry check must
        // STILL veto the delete because the commit isn't in the base.
        let mut state = ChangeState::new("GX-unmerged".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-unmerged".to_string());
        {
            let rs = state.repositories.get_mut("org/repo").unwrap();
            rs.local_path = Some(work.to_string_lossy().to_string());
            rs.status = RepoChangeStatus::PrMerged;
        }

        // force = false -> the fetched-ancestry guard runs and preserves.
        let result = cleanup_change(&mut state, false, false).unwrap();
        assert_eq!(
            result.repos_cleaned, 0,
            "a branch with commits absent from the base must NOT be deleted"
        );
        assert_eq!(result.repos_skipped, 1, "the unmerged branch is skipped");
        assert!(
            crate::git::branch_exists_locally(&work, "GX-unmerged").unwrap(),
            "the unmerged branch must survive a non-force cleanup"
        );
    }

    /// The POSITIVE squash-merge cleanup bite test (gx-shakedown-fixes doc,
    /// Phase 3): this is the test that would have caught Bug 1 -
    /// production-hardening Phase 4 shipped only the negative case above. `gx
    /// review approve` merges with `gh pr merge --squash`, which writes ONE
    /// NEW commit onto the base (same diff, different SHA) - the branch's own
    /// commit never becomes a literal ancestor of base. Against the OLD
    /// `--is-ancestor` commit-identity guard this scenario FAILS: the squash
    /// commit is never an ancestor, so the branch is skipped and the
    /// assertions below fail (`repos_cleaned == 0`, branch still present).
    /// The `git cherry` patch-identity guard (Phase 2) recognizes the
    /// squashed commit's patch as already present in base and cleans it
    /// WITHOUT `--force`.
    #[test]
    fn test_cleanup_squash_merged_branch_is_cleaned_without_force() {
        // An upstream repo with a base branch, cloned locally.
        let parent = TempDir::new().unwrap();
        let upstream = parent.path().join("upstream");
        std::fs::create_dir(&upstream).unwrap();
        run_git_command(&["init", "--quiet"], &upstream);
        run_git_command(&["config", "user.email", "t@e.com"], &upstream);
        run_git_command(&["config", "user.name", "T"], &upstream);
        run_git_command(&["config", "commit.gpgsign", "false"], &upstream);
        std::fs::write(upstream.join("README.md"), "# base").unwrap();
        run_git_command(&["add", "-A"], &upstream);
        run_git_command(&["commit", "--quiet", "-m", "base commit"], &upstream);

        // Clone so origin/HEAD (the resolved base) and an `origin` remote exist.
        let work = parent.path().join("work");
        run_git_command(
            &[
                "clone",
                "--quiet",
                &upstream.to_string_lossy(),
                &work.to_string_lossy(),
            ],
            parent.path(),
        );
        run_git_command(&["config", "user.email", "t@e.com"], &work);
        run_git_command(&["config", "user.name", "T"], &work);
        run_git_command(&["config", "commit.gpgsign", "false"], &work);
        let base_branch = crate::test_utils::get_current_branch(&work);

        // gx's own create flow: a feature branch with exactly ONE commit.
        run_git_command(&["checkout", "--quiet", "-b", "GX-squash"], &work);
        std::fs::write(work.join("feature.txt"), "feature change").unwrap();
        run_git_command(&["add", "-A"], &work);
        run_git_command(&["commit", "--quiet", "-m", "feature commit"], &work);
        assert!(crate::git::branch_exists_locally(&work, "GX-squash").unwrap());

        // Push the branch to upstream, as `gx create` does, so upstream has a
        // ref it can squash-merge (the PR).
        let push = run_git_command(&["push", "--quiet", "origin", "GX-squash"], &work);
        assert!(
            push.status.success(),
            "push of GX-squash to upstream failed: {}",
            String::from_utf8_lossy(&push.stderr)
        );

        // Simulate `gh pr merge --squash`: upstream squash-merges the branch
        // onto its default branch as ONE NEW commit (new SHA, same diff -
        // the branch's own commit never enters base history by identity).
        let squash = run_git_command(&["merge", "--quiet", "--squash", "GX-squash"], &upstream);
        assert!(
            squash.status.success(),
            "git merge --squash failed: {}",
            String::from_utf8_lossy(&squash.stderr)
        );
        let commit = run_git_command(
            &["commit", "--quiet", "-m", "squash-merge feature commit"],
            &upstream,
        );
        assert!(
            commit.status.success(),
            "commit of squashed change failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );

        // Switch `work`'s HEAD back to the base branch, as a real campaign
        // would after review approve merges the PR - `git branch -D` refuses
        // to delete the branch a worktree currently has checked out, which is
        // orthogonal to the guard under test.
        run_git_command(&["checkout", "--quiet", &base_branch], &work);

        // Recorded as PrMerged - the fast-path signal `gx review approve` sets.
        let mut state = ChangeState::new("GX-squash".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-squash".to_string());
        {
            let rs = state.repositories.get_mut("org/repo").unwrap();
            rs.local_path = Some(work.to_string_lossy().to_string());
            rs.status = RepoChangeStatus::PrMerged;
        }

        // force = false: the patch-identity guard must PROVE the squash merge
        // and clean the branch WITHOUT --force. cleanup_change fetches origin
        // inside `work` (picking up upstream's new squash commit) before
        // proving via `git cherry`.
        let result = cleanup_change(&mut state, false, false).unwrap();
        assert_eq!(
            result.repos_cleaned, 1,
            "a squash-merged branch must be cleaned WITHOUT --force"
        );
        assert!(
            !crate::git::branch_exists_locally(&work, "GX-squash").unwrap(),
            "the squash-merged branch must be deleted"
        );
    }

    /// Command-level bite (Phase 3): the confirm gate is WIRED into `cleanup`.
    /// With the eligible-branch count at/above the threshold and non-interactive
    /// stdin (as under `cargo test`) without `--yes`, `cleanup --all` FAILS
    /// CLOSED (loud error naming `--yes`) and deletes NOTHING - the real gx
    /// branch that a cleanup would `-D` still exists afterward. The gate runs
    /// before any `ChangeLock` acquisition, so this test never touches the
    /// flock family. Remove the gate and the branch is deleted (test fails).
    #[test]
    fn test_cleanup_all_fails_closed_and_deletes_nothing() {
        use clap::Parser;
        let guard = crate::test_utils::env_lock();
        let prior_data_home = std::env::var("XDG_DATA_HOME").ok();
        let data_home = TempDir::new().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

        // A real repo with a gx branch a cleanup WOULD delete.
        let repo = TempDir::new().unwrap();
        let p = repo.path();
        run_git_command(&["init", "--quiet"], p);
        run_git_command(&["config", "user.email", "t@e.com"], p);
        run_git_command(&["config", "user.name", "T"], p);
        run_git_command(&["config", "commit.gpgsign", "false"], p);
        std::fs::write(p.join("README.md"), "# r").unwrap();
        run_git_command(&["add", "-A"], p);
        run_git_command(&["commit", "--quiet", "-m", "init"], p);
        run_git_command(&["branch", "GX-cleanup-gate"], p);
        assert!(crate::git::branch_exists_locally(p, "GX-cleanup-gate").unwrap());

        let manager = StateManager::new().unwrap();
        let mut state = ChangeState::new("GX-cleanup-gate".to_string(), None);
        state.add_repository("org/repo".to_string(), "GX-cleanup-gate".to_string());
        state.repositories.get_mut("org/repo").unwrap().local_path =
            Some(p.to_string_lossy().to_string());
        // Merged -> the change is FullyMerged and the repo is eligible for -D.
        state.mark_merged("org/repo");
        manager.save(&state).unwrap();

        let cli = Cli::parse_from(["gx", "cleanup", "--all"]);
        let config = Config {
            cleanup: Some(crate::config::CleanupConfig {
                confirm_threshold: Some(1),
            }),
            ..Config::default()
        };

        // all=true, force=false, yes=false -> 1 eligible >= threshold 1 trips
        // the gate, which fails closed on non-interactive stdin.
        let result = process_cleanup_command(&cli, &config, None, true, false, false, false, false);
        assert!(
            result.is_err(),
            "cleanup must fail closed on non-interactive stdin without --yes"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("--yes"), "error must name --yes: {msg}");
        assert!(
            crate::git::branch_exists_locally(p, "GX-cleanup-gate").unwrap(),
            "the branch must survive a fail-closed cleanup (ZERO deletions)"
        );

        match prior_data_home {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        drop(guard);
    }
}
