//! The `gx create ... llm` PROPOSE pass (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Phase 4 + "Chunk A"
//! propose): fleet-parallel, orchestration-level (NOT inside the per-repo
//! `Change` match, since propose/present/confirm is a fleet barrier).
//!
//! Per repo: take the `RepoLock`, add a DETACHED temp worktree of the pristine
//! head OUTSIDE the real worktree, run the configured agent under a wall-clock
//! timeout with a PROCESS-GROUP kill on expiry, `git add -A` + diff the worktree
//! to capture the change, persist the proposal artifact + blobs, and remove the
//! temp worktree on EVERY path (including errors). Nothing under the real
//! worktree is ever touched - that byte-identity is the whole point and is
//! asserted by a test.
//!
//! The payload-fidelity matrix is enforced HERE, at propose (panel must-fix):
//! regular files (any content incl. binary) and executable-bit/mode-only
//! changes are SUPPORTED; symlinks, gitlinks/submodules, and non-UTF-8 paths are
//! REJECTED as a loud per-repo `failed` outcome NAMING THE PATH.
//!
//! Every propose worktree lives under a gx-owned tmp root
//! ([`worktree_tmp_root`]), not the bare OS temp dir (Phase 7, ringer addendum
//! #7): a crashed prior run's leftover worktree is therefore IDENTIFIABLE and
//! self-healed by [`prune_leftover_worktrees`] at the top of every propose.

use super::manifest::{
    self, FileAction, FileEntry, ProposalManifest, ProposalOutcome, RepoProposal,
};
use crate::config::{xdg_data_dir, Config};
use crate::lock::{ChangeLock, RepoLock};
use crate::repo::Repo;
use crate::state::{ChangeState, StateManager};
use crate::{git, hash};
use eyre::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How often the timeout loop polls the child for exit.
const POLL_INTERVAL_MS: u64 = 50;

/// Longest prompt/agent-output preview written to logs (never inline the full
/// prompt or agent stdout - the logging rule's sensitive/large-payload clause).
const PREVIEW_LEN: usize = 200;

/// The result of a propose pass over a fleet, for the caller (CLI wrapper today;
/// the present gate + MCP `create-propose` later) to render.
#[derive(Debug, Clone)]
pub struct ProposeSummary {
    pub change_id: String,
    /// Confirm token = truncated SHA-256 over the canonical `manifest.json`
    /// bytes (binds every captured blob, since the manifest carries their
    /// hashes). Phase 5/9 require this back before applying.
    pub token: String,
    pub manifest_path: PathBuf,
    pub proposed: usize,
    pub empty: usize,
    pub failed: usize,
    /// Every repo's outcome (for the present step / summary), in canonical order.
    pub repos: Vec<RepoProposal>,
}

/// How the agent process finished.
enum AgentResult {
    Exited(i32),
    Signaled(i32),
    TimedOut,
}

/// Run the propose pass across pre-filtered repos: generate + persist a proposal
/// per repo (fleet-parallel), write the canonical manifest, record `Proposed`
/// state, and return a summary. Never prints; never mutates the real worktree.
pub fn execute_propose(
    repos: &[Repo],
    change_id: &str,
    prompt: &str,
    config: &Config,
    parallel_jobs: usize,
) -> Result<ProposeSummary> {
    debug!(
        "execute_propose: change_id={change_id} repos={} prompt=\"{}\"",
        repos.len(),
        preview(prompt)
    );

    // Cross-process safety: propose writes `changes/<id>.json` AND the proposal
    // artifacts, so hold the change lock exactly as a committing create does.
    let _change_lock = ChangeLock::acquire(change_id)
        .map_err(|e| eyre::eyre!("Cannot start propose for {change_id}: {e}"))?;

    let agent_command = config.llm_agent_command();
    let timeout = Duration::from_secs(config.llm_timeout_seconds());
    let proposal_dir = manifest::proposal_dir(change_id)?;

    // Self-heal a crashed prior run's leftover temp worktrees (ringer addendum
    // #7) BEFORE this run creates any of its own: everything under the root
    // right now necessarily predates this call, so pruning here can never
    // race this run's own in-flight worktrees.
    let tmp_root = worktree_tmp_root()?;
    prune_leftover_worktrees(&tmp_root);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(parallel_jobs.max(1))
        .build()
        .context("Failed to create thread pool")?;

    let repo_proposals: Vec<RepoProposal> = pool.install(|| {
        repos
            .par_iter()
            .map(|repo| {
                propose_single_repo(
                    repo,
                    prompt,
                    &agent_command,
                    timeout,
                    &proposal_dir,
                    &tmp_root,
                )
            })
            .collect()
    });

    // Canonical manifest (sorts repos + files); token binds its exact bytes.
    let manifest = ProposalManifest::new(
        change_id.to_string(),
        prompt.to_string(),
        agent_command,
        repo_proposals,
    );
    let (manifest_path, token) = manifest::write_manifest(&proposal_dir, &manifest)?;

    // Record `Proposed` state for the repos that produced an appliable change.
    // Empty/failed repos are NOT put in change state: there is nothing to apply
    // or undo for them, and recording a failed PROPOSE as `Failed` would
    // conflate it with a failed CREATE (which may have pushed a branch). The
    // manifest is the record of empty/failed outcomes for the present step.
    let path_by_slug: BTreeMap<&str, &Path> = repos
        .iter()
        .map(|r| (r.slug.as_str(), r.path.as_path()))
        .collect();
    let mut state = ChangeState::new(change_id.to_string(), Some(preview(prompt)));
    let mut proposed = 0usize;
    let mut empty = 0usize;
    let mut failed = 0usize;
    for rp in &manifest.repos {
        match rp.outcome {
            ProposalOutcome::Proposed => {
                proposed += 1;
                let files = rp.files.iter().map(|f| f.path.clone()).collect();
                let local_path = path_by_slug
                    .get(rp.slug.as_str())
                    .map(|p| p.to_string_lossy().to_string());
                state.mark_proposed(&rp.slug, rp.base_sha.clone(), files, local_path);
            }
            ProposalOutcome::Empty => empty += 1,
            ProposalOutcome::Failed => failed += 1,
        }
    }
    if !state.repositories.is_empty() {
        let manager = StateManager::new()
            .context("Cannot record proposal state: durable state store unavailable")?;
        manager
            .save(&state)
            .context("Failed to save proposal change state")?;
        info!(
            "execute_propose: recorded {proposed} proposed repo(s) for {change_id} ({empty} empty, {failed} failed)"
        );
    } else {
        debug!(
            "execute_propose: no repo produced an appliable change ({empty} empty, {failed} failed); no change state written"
        );
    }

    Ok(ProposeSummary {
        change_id: change_id.to_string(),
        token,
        manifest_path,
        proposed,
        empty,
        failed,
        repos: manifest.repos,
    })
}

/// Propose a change for a single repo in a throwaway detached worktree. Returns
/// a `RepoProposal` for every outcome (proposed | empty | failed); never
/// panics, never mutates the real worktree, and removes the temp worktree on
/// every path.
fn propose_single_repo(
    repo: &Repo,
    prompt: &str,
    agent_command: &str,
    timeout: Duration,
    proposal_dir: &Path,
    tmp_root: &Path,
) -> RepoProposal {
    debug!(
        "propose_single_repo: slug={} path={}",
        repo.slug,
        repo.path.display()
    );

    // 1. Per-repo lock (same guarantee as create): no concurrent gx op on it.
    let _lock = match RepoLock::acquire(&repo.path) {
        Ok(lock) => lock,
        Err(e) => {
            return failed(
                &repo.slug,
                String::new(),
                format!("Repository is locked: {e}"),
            )
        }
    };

    // 2. Pristine head the proposal is generated against.
    let base_sha = match git::get_head_sha(&repo.path) {
        Ok(sha) => sha,
        Err(e) => {
            return failed(
                &repo.slug,
                String::new(),
                format!("Failed to read head sha: {e}"),
            )
        }
    };

    // 3. Temp dir OUTSIDE the real worktree, under the gx-owned tmp root (NOT
    //    the bare OS temp dir - ringer addendum #7): the worktree checkout
    //    goes in a child path git creates, the agent log sibling to it (so the
    //    log is never captured by `git add -A`). Living under `tmp_root` is
    //    what lets a later propose IDENTIFY this as a leftover if the process
    //    dies before step 5 below removes it.
    if let Err(e) = std::fs::create_dir_all(tmp_root) {
        return failed(
            &repo.slug,
            base_sha,
            format!("Failed to create propose tmp root: {e}"),
        );
    }
    let tmp = match tempfile::Builder::new().prefix("wt-").tempdir_in(tmp_root) {
        Ok(t) => t,
        Err(e) => {
            return failed(
                &repo.slug,
                base_sha,
                format!("Failed to create temp dir: {e}"),
            )
        }
    };
    let worktree = tmp.path().join("wt");
    if let Err(e) = git::worktree_add_detached(&repo.path, &worktree, &base_sha) {
        return failed(&repo.slug, base_sha, format!("Failed to add worktree: {e}"));
    }

    // 4. Run the agent and capture the change. Cleanup runs on EVERY path below.
    let outcome = run_and_capture(
        &worktree,
        tmp.path(),
        agent_command,
        prompt,
        timeout,
        &base_sha,
        proposal_dir,
        &repo.slug,
    );

    // 5. Remove the temp worktree in ALL paths (design step 6). `tmp` drops
    //    after, cleaning the filesystem; a failed removal only leaves a stale
    //    worktree registration, which `gx doctor` reports (Phase 5).
    if let Err(e) = git::worktree_remove(&repo.path, &worktree) {
        warn!(
            "propose_single_repo: failed to remove temp worktree {}: {e}",
            worktree.display()
        );
    }

    match outcome {
        Ok(files) if files.is_empty() => RepoProposal {
            slug: repo.slug.clone(),
            base_sha,
            outcome: ProposalOutcome::Empty,
            error: None,
            files: Vec::new(),
        },
        Ok(files) => RepoProposal {
            slug: repo.slug.clone(),
            base_sha,
            outcome: ProposalOutcome::Proposed,
            error: None,
            files,
        },
        Err(e) => failed(&repo.slug, base_sha, e.to_string()),
    }
}

/// `$XDG_DATA_HOME/gx/tmp/propose` - the gx-owned root every propose temp
/// worktree lives under (design Risks row: "worktrees under a gx-owned tmp
/// root"; ringer addendum #7). Housing every propose worktree here, rather
/// than the bare OS temp dir, is what makes a crashed prior run's leftovers
/// IDENTIFIABLE at the top of the next propose.
fn worktree_tmp_root() -> Result<PathBuf> {
    Ok(xdg_data_dir()
        .ok_or_else(|| eyre::eyre!("Could not determine data dir (set HOME or XDG_DATA_HOME)"))?
        .join("gx")
        .join("tmp")
        .join("propose"))
}

/// Self-heal a crashed prior run's leftover temp worktrees (ringer addendum
/// #7). Called ONCE at the top of [`execute_propose`], before this run
/// creates any of its own entries under `tmp_root`: everything found there
/// necessarily predates this call (this run hasn't created anything yet), so
/// pruning can never race this run's own in-flight worktrees. Only ever reads/
/// removes entries under our OWN `tmp_root` - a repo's other worktrees (gx or
/// otherwise, living anywhere else) are never touched.
fn prune_leftover_worktrees(tmp_root: &Path) {
    let entries = match std::fs::read_dir(tmp_root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!(
                "prune_leftover_worktrees: cannot read {}: {e}",
                tmp_root.display()
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let leftover = entry.path();
        if leftover.is_dir() {
            prune_one_leftover(&leftover);
        }
    }
}

/// Prune a single leftover propose tmp entry: resolve the repo it belongs to
/// via the worktree's own `.git` back-pointer (survives the crash even though
/// gx's in-process mapping does not), remove the worktree registration there,
/// then remove the leftover directory itself. If the owning repo is currently
/// locked (a live gx operation in progress), leave the WHOLE leftover for a
/// later propose rather than yanking a worktree out from under it.
fn prune_one_leftover(leftover: &Path) {
    let worktree = leftover.join("wt");
    if worktree.exists() {
        match git::resolve_worktree_repo(&worktree) {
            Ok(Some(repo_root)) => match RepoLock::acquire(&repo_root) {
                Ok(_lock) => {
                    if let Err(e) = git::worktree_remove(&repo_root, &worktree) {
                        warn!(
                            "prune_one_leftover: git worktree remove failed for {}: {e}",
                            worktree.display()
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "prune_one_leftover: {} is locked; leaving {} for a later propose: {e}",
                        repo_root.display(),
                        leftover.display()
                    );
                    return;
                }
            },
            Ok(None) => debug!(
                "prune_one_leftover: {} has no resolvable owning repo; removing directly",
                worktree.display()
            ),
            Err(e) => warn!(
                "prune_one_leftover: failed to resolve owning repo for {}: {e}",
                worktree.display()
            ),
        }
    }
    match std::fs::remove_dir_all(leftover) {
        Ok(()) => info!(
            "prune_one_leftover: removed leftover propose worktree {}",
            leftover.display()
        ),
        Err(e) => warn!(
            "prune_one_leftover: failed to remove leftover {}: {e}",
            leftover.display()
        ),
    }
}

/// Build a `failed` outcome for a repo.
fn failed(slug: &str, base_sha: String, error: String) -> RepoProposal {
    warn!("propose: {slug} failed: {error}");
    RepoProposal {
        slug: slug.to_string(),
        base_sha,
        outcome: ProposalOutcome::Failed,
        error: Some(error),
        files: Vec::new(),
    }
}

/// Run the agent, then (only on a clean exit) capture the worktree change. A
/// nonzero exit, a signal, or a timeout is a loud error; the captured payload
/// is validated against the fidelity matrix BEFORE any artifact is written, so
/// a rejected repo persists nothing.
#[allow(clippy::too_many_arguments)]
fn run_and_capture(
    worktree: &Path,
    tmp_root: &Path,
    agent_command: &str,
    prompt: &str,
    timeout: Duration,
    base_sha: &str,
    proposal_dir: &Path,
    slug: &str,
) -> Result<Vec<FileEntry>> {
    let log_path = tmp_root.join("agent.log");
    match run_agent(worktree, agent_command, prompt, timeout, &log_path)? {
        AgentResult::Exited(0) => {}
        AgentResult::Exited(code) => {
            return Err(eyre::eyre!(
                "agent exited with status {code}: {}",
                log_preview(&log_path)
            ));
        }
        AgentResult::Signaled(sig) => {
            return Err(eyre::eyre!(
                "agent was killed by signal {sig}: {}",
                log_preview(&log_path)
            ));
        }
        AgentResult::TimedOut => {
            return Err(eyre::eyre!(
                "agent timed out after {}s (process group killed)",
                timeout.as_secs()
            ));
        }
    }
    capture_changes(worktree, base_sha, proposal_dir, slug)
}

/// Spawn the agent in its OWN process group (so a timeout can kill the whole
/// tree), redirect its stdio to a log file (no pipe-buffer deadlock), and
/// enforce `timeout` by polling. On expiry, `kill -KILL -<pgid>` fells the
/// entire group. The lock fd is O_CLOEXEC (Rust default), so the agent never
/// inherits it.
fn run_agent(
    worktree: &Path,
    agent_command: &str,
    prompt: &str,
    timeout: Duration,
    log_path: &Path,
) -> Result<AgentResult> {
    use std::os::unix::process::CommandExt;

    let mut argv = agent_command.split_whitespace();
    let program = argv
        .next()
        .ok_or_else(|| eyre::eyre!("agent-command is empty"))?;
    let args: Vec<&str> = argv.collect();
    debug!(
        "run_agent: program={program} args={args:?} timeout={}s cwd={} prompt=\"{}\"",
        timeout.as_secs(),
        worktree.display(),
        preview(prompt)
    );

    let log = std::fs::File::create(log_path)
        .with_context(|| format!("Failed to create agent log: {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .context("Failed to clone agent log handle")?;

    let mut cmd = Command::new(program);
    cmd.args(&args)
        .arg(prompt)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        // New process group with pgid == child pid; a timeout kill signals the
        // whole group, felling any grandchildren the agent spawned.
        .process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn agent `{program}`"))?;
    let pgid = child.id();

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().context("Failed to poll agent")? {
            Some(status) => return Ok(classify_status(status)),
            None => {
                if Instant::now() >= deadline {
                    warn!("run_agent: timeout reached; killing process group {pgid}");
                    kill_process_group(pgid);
                    // Reap so no zombie lingers; the exact status is irrelevant.
                    let _ = child.wait();
                    return Ok(AgentResult::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }
}

/// Classify a finished child's exit status into an [`AgentResult`].
fn classify_status(status: std::process::ExitStatus) -> AgentResult {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(code) => AgentResult::Exited(code),
        None => AgentResult::Signaled(status.signal().unwrap_or(-1)),
    }
}

/// SIGKILL an entire process group by pgid, via `/bin/kill -KILL -<pgid>`. std
/// exposes no group-kill, and `libc` would be a new dependency chunk A forbids;
/// the `kill` builtin/binary is always present on the Unix targets gx runs on.
fn kill_process_group(pgid: u32) {
    let status = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pgid}"))
        .status();
    if let Err(e) = status {
        warn!("kill_process_group: failed to signal group {pgid}: {e}");
    }
}

/// Stage the worktree, diff it against `base_sha`, enforce the payload-fidelity
/// matrix, and (only if everything passes) write the display patch + apply
/// blobs. Returns the per-file entries, or an empty vec for an empty diff.
fn capture_changes(
    worktree: &Path,
    base_sha: &str,
    proposal_dir: &Path,
    slug: &str,
) -> Result<Vec<FileEntry>> {
    debug!("capture_changes: slug={slug} base_sha={base_sha}");
    git::stage_all(worktree)?;
    let raw = git::diff_cached_raw_z(worktree, base_sha)?;
    if raw.is_empty() {
        debug!("capture_changes: slug={slug} empty diff");
        return Ok(Vec::new());
    }

    // Parse + validate EVERYTHING before writing anything, so a rejected repo
    // (symlink/gitlink/non-UTF-8) leaves no partial artifacts on disk. Collect
    // each entry's metadata plus its post-change bytes (for add/modify).
    let mut pending: Vec<(FileEntry, Option<Vec<u8>>)> = Vec::new();
    for (meta, path_bytes) in parse_raw_records(&raw)? {
        // A non-UTF-8 path is rejected NAMING the (lossy) path.
        let path = match std::str::from_utf8(&path_bytes) {
            Ok(p) => p.to_string(),
            Err(_) => {
                return Err(eyre::eyre!(
                    "rejected non-UTF-8 path: {}",
                    String::from_utf8_lossy(&path_bytes)
                ));
            }
        };
        let (src_mode, dst_mode) = (meta.src_mode.as_str(), meta.dst_mode.as_str());

        // Symlinks and gitlinks/submodules are rejected NAMING the path.
        if src_mode == MODE_SYMLINK || dst_mode == MODE_SYMLINK {
            return Err(eyre::eyre!("rejected symlink: {path}"));
        }
        if src_mode == MODE_GITLINK || dst_mode == MODE_GITLINK {
            return Err(eyre::eyre!("rejected gitlink/submodule: {path}"));
        }

        if dst_mode == MODE_ABSENT {
            // Deletion: no blob.
            pending.push((
                FileEntry {
                    path,
                    action: FileAction::Delete,
                    mode: src_mode.to_string(),
                    sha256: None,
                    size: 0,
                },
                None,
            ));
        } else {
            // Add or modify: read the post-change bytes (supports binary), hash
            // them, capture the destination mode (covers mode-only changes).
            let full = worktree.join(&path);
            let bytes = std::fs::read(&full)
                .with_context(|| format!("Failed to read proposed file: {path}"))?;
            let action = if src_mode == MODE_ABSENT {
                FileAction::Add
            } else {
                FileAction::Modify
            };
            let entry = FileEntry {
                path,
                action,
                mode: dst_mode.to_string(),
                sha256: Some(hash::sha256_hex(&bytes)),
                size: bytes.len() as u64,
            };
            pending.push((entry, Some(bytes)));
        }
    }

    // All entries passed the matrix. Persist: display patch first, then blobs.
    let patch = git::diff_cached_patch(worktree, base_sha)?;
    manifest::write_patch(proposal_dir, slug, &patch)?;

    let mut entries = Vec::with_capacity(pending.len());
    for (entry, bytes) in pending {
        if let Some(bytes) = bytes {
            manifest::write_blob(proposal_dir, slug, &entry.path, &bytes)?;
        }
        entries.push(entry);
    }
    debug!(
        "capture_changes: slug={slug} captured {} file(s)",
        entries.len()
    );
    Ok(entries)
}

/// Git file modes as they appear in `git diff --raw` output.
const MODE_ABSENT: &str = "000000";
const MODE_SYMLINK: &str = "120000";
const MODE_GITLINK: &str = "160000";

/// One parsed `--raw -z` record's mode metadata.
struct RawMeta {
    src_mode: String,
    dst_mode: String,
}

/// Parse `git diff --cached --raw -z` output into (meta, path-bytes) records.
///
/// The `-z` format emits, per changed file, a metadata field
/// (`:<srcmode> <dstmode> <srcsha> <dstsha> <STATUS>`) then a NUL, then the path
/// bytes, then a NUL. Renames/copies (two paths) do not occur: gx never passes
/// `-M`/`-C`, so a rename is reported as a delete + an add. Paths are kept as
/// raw bytes so a non-UTF-8 path survives to the rejection check.
fn parse_raw_records(raw: &[u8]) -> Result<Vec<(RawMeta, Vec<u8>)>> {
    let mut tokens = raw.split(|&b| b == 0).filter(|t| !t.is_empty());
    let mut out = Vec::new();
    while let Some(meta_bytes) = tokens.next() {
        let meta_str = std::str::from_utf8(meta_bytes)
            .context("git diff --raw metadata was not valid UTF-8")?;
        let path_bytes = tokens
            .next()
            .ok_or_else(|| eyre::eyre!("git diff --raw record missing its path"))?
            .to_vec();
        // Fields: ":<srcmode> <dstmode> <srcsha> <dstsha> <STATUS>".
        let fields: Vec<&str> = meta_str.split(' ').collect();
        if fields.len() < 5 {
            return Err(eyre::eyre!("unexpected git diff --raw record: {meta_str}"));
        }
        let src_mode = fields[0].trim_start_matches(':').to_string();
        let dst_mode = fields[1].to_string();
        out.push((RawMeta { src_mode, dst_mode }, path_bytes));
    }
    Ok(out)
}

/// A short single-line preview of a possibly-large/sensitive string (prompt),
/// for logs; never inline the full value (logging rule).
fn preview(s: &str) -> String {
    let one_line: String = s.chars().take(PREVIEW_LEN).collect();
    let one_line = one_line.replace('\n', " ");
    if s.chars().count() > PREVIEW_LEN {
        format!("{one_line}...")
    } else {
        one_line
    }
}

/// A short preview of the agent's captured stdout/stderr log (for error
/// context); missing/unreadable log is reported plainly.
fn log_preview(log_path: &Path) -> String {
    match std::fs::read(log_path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let trimmed = text.trim();
            preview(trimmed)
        }
        Err(_) => "(no agent output captured)".to_string(),
    }
}

#[cfg(test)]
mod tests;
