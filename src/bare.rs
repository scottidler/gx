//! Vendored, minimal bare-container awareness for gx.
//!
//! `clone --bare` produces a **bare container**: a directory holding a shared
//! `.bare/` git db, a `.git` pointer file (`gitdir: ./.bare`), and one linked
//! worktree per branch (`main/`, `<branch>/`). The container root is NOT a work
//! tree, so `git -C <root> status` fails; git commands must run inside a
//! worktree instead.
//!
//! This module is a deliberate ~vendored copy of the detection/enumeration
//! logic (see the flat-by-default clone design doc, Alternative 2): gx is a
//! separate repo and must not take a cross-repo `path`/`git` dependency on
//! git-tools' `common` crate. It is intentionally small; if a third consumer
//! appears, extract a published crate then.
//!
//! **Semantics:** a bare container is ONE logical repo == its *default
//! worktree*. gx operates in that single worktree and never fans write commands
//! out across the container's N worktrees.

use eyre::{eyre, Result};
use log::{debug, trace};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One row parsed from `git worktree list --porcelain`.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    /// Short branch name (`refs/heads/` stripped). `None` = detached HEAD or the
    /// bare entry.
    pub branch: Option<String>,
    /// The `.bare` entry itself (the shared db), not a checkout.
    pub bare: bool,
}

/// Strict bare-container detection.
///
/// `path` is a container only if it holds BOTH a `.bare/` directory AND a `.git`
/// pointer *file* whose contents start with `gitdir:` and reference `.bare`. A
/// lone `.bare/` directory, a `.git` *directory* (flat repo), or a linked
/// worktree (whose `.git` file points into `.bare/worktrees/<id>`, with no
/// sibling `.bare/`) is NOT a container.
pub fn is_bare_container(path: &Path) -> bool {
    // `.bare` must be a directory. This is the cheap discriminator that fails
    // fast for the ~99% of directories that are not containers, so this stays
    // affordable to call once per directory during discovery.
    if !path.join(".bare").is_dir() {
        return false;
    }
    // `.git` must be a pointer *file*, not a directory.
    let git_pointer = path.join(".git");
    if !git_pointer.is_file() {
        return false;
    }
    match std::fs::read_to_string(&git_pointer) {
        Ok(content) => {
            let content = content.trim();
            content.starts_with("gitdir:") && content.contains(".bare")
        }
        Err(_) => false,
    }
}

/// True if `path` is a git checkout gx can operate in: a flat repo (`.git`
/// directory), a linked worktree (`.git` pointer file), or a bare container.
///
/// Used by consumers that only need "is this still a git repo at this path?"
/// without caring which layout it is.
pub fn is_git_path(path: &Path) -> bool {
    path.join(".git").exists() || is_bare_container(path)
}

/// Enumerate the worktrees of the container at `container` via a single
/// `git worktree list --porcelain` invocation.
pub fn resolve_worktrees(container: &Path) -> Result<Vec<Worktree>> {
    debug!("resolve_worktrees: container={}", container.display());
    let output = Command::new("git")
        .arg("-C")
        .arg(container)
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    if !output.status.success() {
        return Err(eyre!(
            "git worktree list failed in {}: {}",
            container.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let worktrees = parse_worktree_porcelain(&stdout);
    debug!("resolve_worktrees: found {} worktree(s)", worktrees.len());
    Ok(worktrees)
}

/// Parse `git worktree list --porcelain` output into rows. Each worktree is a
/// blank-line-separated paragraph beginning with a `worktree <path>` line.
fn parse_worktree_porcelain(stdout: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;

    for line in stdout.lines() {
        trace!("parse_worktree_porcelain: line={line}");
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(previous) = current.take() {
                worktrees.push(previous);
            }
            current = Some(Worktree {
                path: PathBuf::from(path.trim()),
                branch: None,
                bare: false,
            });
        } else if let Some(worktree) = current.as_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                worktree.branch = Some(branch.trim().trim_start_matches("refs/heads/").to_string());
            } else if line.trim() == "bare" {
                worktree.bare = true;
            }
        }
    }
    if let Some(last) = current.take() {
        worktrees.push(last);
    }
    worktrees
}

/// Resolve the *default worktree* of a bare container.
///
/// The default worktree is the linked worktree checked out on the container's
/// default branch (`git symbolic-ref HEAD` against the shared `.bare` db). Falls
/// back to the first non-bare worktree when the default branch has no worktree.
/// A bare container is ONE logical repo == this worktree.
pub fn default_worktree(container: &Path) -> Result<PathBuf> {
    debug!("default_worktree: container={}", container.display());
    let worktrees = resolve_worktrees(container)?;
    let default_branch = default_branch(container);
    debug!("default_worktree: default_branch={default_branch:?}");

    if let Some(branch) = default_branch.as_deref() {
        if let Some(worktree) = worktrees
            .iter()
            .find(|w| !w.bare && w.branch.as_deref() == Some(branch))
        {
            debug!(
                "default_worktree: matched default branch worktree at {}",
                worktree.path.display()
            );
            return Ok(worktree.path.clone());
        }
    }

    match worktrees.iter().find(|w| !w.bare) {
        Some(worktree) => {
            debug!(
                "default_worktree: falling back to first worktree at {}",
                worktree.path.display()
            );
            Ok(worktree.path.clone())
        }
        None => Err(eyre!(
            "bare container {} has no linked worktree",
            container.display()
        )),
    }
}

/// The container's default branch, read from the shared db's symbolic HEAD.
/// Returns `None` if HEAD is detached or git cannot resolve it.
fn default_branch(container: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(container)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Origin remote URL via `git remote get-url origin`, run at `path`. Works
/// uniformly for flat repos, linked worktrees, and bare containers (each of
/// which resolves to the same shared config).
pub fn origin_url(path: &Path) -> Result<String> {
    debug!("origin_url: path={}", path.display());
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .output()?;
    if !output.status.success() {
        return Err(eyre!(
            "git remote get-url origin failed in {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests;
