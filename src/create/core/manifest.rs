//! Proposal artifact: the persisted, reviewable object a `gx create ... llm`
//! propose pass writes, and a later `gx apply <change-id>` (Phase 5) reads.
//!
//! Layout under `$XDG_DATA_HOME/gx/proposals/<change-id>/` (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, Data Model):
//!
//! - `manifest.json` - the CANONICAL reviewed object (this module's
//!   [`ProposalManifest`]): change-id, prompt, resolved agent command,
//!   created-at, and per repo {slug, base_sha, outcome, per-file {path, action,
//!   mode, blob sha256 + size}}.
//! - `<org>/<repo>.patch` - the unified diff, DISPLAY ONLY. Apply never reads it.
//! - `<org>/<repo>/files/<path>` - the full post-change content of each
//!   added/modified file, the APPLY PAYLOAD. Kept as files, not inlined into
//!   the manifest, so state stays small and scannable while payloads can be
//!   large or binary.
//!
//! **The confirm token binds the applied bytes** (panel must-fix): the token is
//! a truncated SHA-256 over the exact `manifest.json` bytes, and the manifest
//! carries every blob's `sha256`, so no blob can change after review without
//! invalidating the token. Apply (Phase 5) re-hashes the persisted
//! `manifest.json` and each blob under `RepoLock` before writing; any mismatch
//! is a loud refusal.
//!
//! Field naming matches the sibling `changes/<id>.json` artifact
//! ([`crate::state::ChangeState`]): serde default (snake_case), `version` field
//! + `deny_unknown_fields` so an older gx reading a newer manifest fails loudly.

use crate::config::xdg_data_dir;
use crate::hash::sha256_hex;
use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use log::debug;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Schema version stamped on every proposal manifest. Bumped when the manifest
/// shape changes; `deny_unknown_fields` + this field make a version skew fail
/// loudly rather than silently mis-load.
pub const PROPOSAL_MANIFEST_VERSION: u32 = 1;

/// How many hex chars of the manifest SHA-256 the confirm token carries. 16 hex
/// chars = 64 bits, enough to bind the reviewed manifest to the applied one
/// while staying short enough for an MCP client to echo back (Phase 9).
pub const TOKEN_HEX_LEN: usize = 16;

/// What happened to one file inside a repo's proposal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileAction {
    Add,
    Modify,
    Delete,
}

/// One file's entry in a repo's proposal. For `Delete` there is no blob, so
/// `sha256` is `None` and `size` is 0; for `Add`/`Modify` the blob's SHA-256
/// and byte size bind the persisted payload to the manifest (and thus the token).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Repo-relative path (forward-slashed, UTF-8; non-UTF-8 paths are rejected
    /// at propose so this is always valid UTF-8).
    pub path: String,
    pub action: FileAction,
    /// Git file mode of the post-change file, e.g. `100644` or `100755`. A
    /// mode-only change still captures the (unchanged) blob so apply has no
    /// special case (design payload matrix).
    pub mode: String,
    /// SHA-256 hex of the post-change content; `None` for deletions.
    pub sha256: Option<String>,
    /// Byte size of the post-change content; 0 for deletions.
    pub size: u64,
}

/// The outcome of proposing a change for one repo.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProposalOutcome {
    /// The agent produced a captured, appliable change.
    Proposed,
    /// The agent ran clean but produced no diff (valid, not an error).
    Empty,
    /// A loud per-repo failure (agent nonzero/timeout, unreadable worktree, or
    /// a rejected payload kind); `error` names it.
    Failed,
}

/// One repo's proposal entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepoProposal {
    pub slug: String,
    /// The pristine head the proposal was generated against; apply refuses if
    /// the repo has drifted past it (post-pull drift check, Phase 5).
    pub base_sha: String,
    pub outcome: ProposalOutcome,
    /// Present iff `outcome == Failed`.
    pub error: Option<String>,
    /// Empty for `Empty`/`Failed`; sorted by path for `Proposed`.
    pub files: Vec<FileEntry>,
}

/// The canonical reviewed object persisted as `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProposalManifest {
    pub version: u32,
    pub change_id: String,
    /// The prompt handed to the agent (previewed in logs, never inlined full).
    pub prompt: String,
    /// The fully resolved agent command line (config `agent-command`).
    pub agent_command: String,
    pub created_at: DateTime<Utc>,
    /// Per-repo entries, sorted by slug for a deterministic canonical form.
    pub repos: Vec<RepoProposal>,
}

impl ProposalManifest {
    /// Build a manifest, sorting repos (and each repo's files) into the
    /// canonical order used for the token hash.
    pub fn new(
        change_id: String,
        prompt: String,
        agent_command: String,
        mut repos: Vec<RepoProposal>,
    ) -> Self {
        repos.sort_by(|a, b| a.slug.cmp(&b.slug));
        for repo in &mut repos {
            repo.files.sort_by(|a, b| a.path.cmp(&b.path));
        }
        Self {
            version: PROPOSAL_MANIFEST_VERSION,
            change_id,
            prompt,
            agent_command,
            created_at: Utc::now(),
            repos,
        }
    }
}

/// `$XDG_DATA_HOME/gx/proposals` - the root under which every change's proposal
/// directory lives.
pub fn proposals_root() -> Result<PathBuf> {
    Ok(xdg_data_dir()
        .ok_or_else(|| eyre::eyre!("Could not determine data dir (set HOME or XDG_DATA_HOME)"))?
        .join("gx")
        .join("proposals"))
}

/// The proposal directory for a single change id.
pub fn proposal_dir(change_id: &str) -> Result<PathBuf> {
    Ok(proposals_root()?.join(change_id))
}

/// The display-patch path for a repo slug inside a proposal dir
/// (`<proposal_dir>/<slug>.patch`).
pub fn patch_path(proposal_dir: &Path, slug: &str) -> PathBuf {
    proposal_dir.join(format!("{slug}.patch"))
}

/// The apply-payload blob path for a repo-relative file inside a proposal dir
/// (`<proposal_dir>/<slug>/files/<rel_path>`).
pub fn blob_path(proposal_dir: &Path, slug: &str, rel_path: &str) -> PathBuf {
    proposal_dir.join(slug).join("files").join(rel_path)
}

/// Write raw blob bytes for an added/modified file (creating parent dirs),
/// atomically so a torn write can never be served to apply as a valid payload.
pub fn write_blob(proposal_dir: &Path, slug: &str, rel_path: &str, bytes: &[u8]) -> Result<()> {
    let path = blob_path(proposal_dir, slug, rel_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create blob dir: {}", parent.display()))?;
    }
    crate::file::atomic_write(&path, bytes)
        .with_context(|| format!("Failed to write blob: {}", path.display()))?;
    debug!(
        "write_blob: slug={slug} path={rel_path} bytes={}",
        bytes.len()
    );
    Ok(())
}

/// Write the display patch for a repo slug (creating parent dirs) atomically.
pub fn write_patch(proposal_dir: &Path, slug: &str, patch: &str) -> Result<()> {
    let path = patch_path(proposal_dir, slug);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create patch dir: {}", parent.display()))?;
    }
    crate::file::atomic_write(&path, patch.as_bytes())
        .with_context(|| format!("Failed to write patch: {}", path.display()))?;
    debug!("write_patch: slug={slug} bytes={}", patch.len());
    Ok(())
}

/// Serialize the manifest to its canonical `manifest.json` bytes, write them
/// atomically, and return `(manifest_path, token)`. The token is a truncated
/// SHA-256 over the EXACT bytes written, so apply re-hashing the persisted file
/// reproduces it iff nothing tampered with the manifest.
pub fn write_manifest(
    proposal_dir: &Path,
    manifest: &ProposalManifest,
) -> Result<(PathBuf, String)> {
    debug!(
        "write_manifest: change_id={} repos={}",
        manifest.change_id,
        manifest.repos.len()
    );
    std::fs::create_dir_all(proposal_dir)
        .with_context(|| format!("Failed to create proposal dir: {}", proposal_dir.display()))?;
    let bytes = serde_json::to_vec_pretty(manifest).context("Failed to serialize manifest")?;
    let path = proposal_dir.join("manifest.json");
    crate::file::atomic_write(&path, &bytes)
        .with_context(|| format!("Failed to write manifest: {}", path.display()))?;
    let token = compute_token(&bytes);
    Ok((path, token))
}

// NOTE (Phase 5 handoff): the READ side - `load_manifest(dir) ->
// ProposalManifest` (serde_json::from_slice over `manifest.json`) and
// `recompute_token(dir)` (read the bytes, `compute_token`) - is deliberately
// NOT added here. Adding an uncalled `pub fn` now trips the bin target's
// dead-code `-D warnings` (the `gx` bin duplicates the lib module tree and has
// no external consumer). Phase 5's apply path is their first real caller and
// adds them then; Phase 4 tests exercise the round-trip directly via
// `serde_json::from_slice` + `compute_token`.

/// The confirm token for a set of canonical manifest bytes: a truncated
/// SHA-256 hex. Truncation is a plain prefix of the (ASCII) hex string.
pub fn compute_token(manifest_bytes: &[u8]) -> String {
    let full = sha256_hex(manifest_bytes);
    full.get(..TOKEN_HEX_LEN)
        .map(str::to_string)
        .unwrap_or(full)
}

#[cfg(test)]
mod tests;
