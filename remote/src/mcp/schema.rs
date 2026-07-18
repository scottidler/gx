//! Request/response wire types for the gx-mcp tool surface (design doc
//! `2026-07-12-llm-propose-apply-and-mcp-server.md`, API Design > MCP tools).
//!
//! These are the MCP-facing schema, deliberately DECOUPLED from gx's internal
//! types: a tool maps gx's structured core results into these so the wire
//! contract (what a driver sees) never drifts with an internal refactor.
//! Request types derive `JsonSchema` (rmcp generates each tool's input schema
//! from it); response types derive `Serialize` (returned via `Content::json`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ requests

/// `status` / `repo-discover`: filter the discovered fleet by these patterns
/// (empty = every repo under the server's CWD).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusRequest {
    #[serde(default)]
    #[schemars(description = "Repo slug patterns to match; empty matches all discovered repos")]
    pub patterns: Vec<String>,
    /// Off by default: remote-tracking status needs network round-trips, so a
    /// fleet `status` stays local-only unless the driver opts in.
    #[serde(default)]
    #[schemars(
        description = "Fetch remote-tracking (ahead/behind) status; slower, off by default"
    )]
    pub fetch_remote: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RepoDiscoverRequest {
    #[serde(default)]
    #[schemars(description = "Repo slug patterns to match; empty matches all discovered repos")]
    pub patterns: Vec<String>,
}

/// A tool that takes no arguments (`change-list`, `review-status`, `doctor`).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoArgs {}

// ---- intel-catalog read-only tools (design doc 2026-07-17-gx-intel-catalog) --

/// `query`: repo metadata rows under a clamped `root` (default = the server's
/// CWD), filtered by the optional `where{}` fields. The `where{}` object is
/// flattened to top-level optional args here: it keeps the MCP input schema
/// flat and sidesteps the `where` Rust keyword, with identical filter semantics.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryRequest {
    #[serde(default)]
    #[schemars(
        description = "Subtree root to scope to (default: server CWD); clamped to catalog.root"
    )]
    pub root: Option<String>,
    #[serde(default)]
    #[schemars(description = "Only repos with uncommitted changes (true) or clean (false)")]
    pub dirty: Option<bool>,
    #[serde(default)]
    #[schemars(description = "Only repos on this branch")]
    pub branch: Option<String>,
    #[serde(default)]
    #[schemars(description = "Only repos in this org/user")]
    pub org: Option<String>,
    #[serde(default)]
    #[schemars(description = "Only repos whose primary language guess is this")]
    pub lang: Option<String>,
    #[serde(default)]
    #[schemars(description = "Only repos whose local behind-count exceeds this")]
    pub behind_gt: Option<i64>,
}

/// `search`: live `rg` over the working trees under a clamped `root`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchRequest {
    #[serde(default)]
    #[schemars(
        description = "Subtree root to scope to (default: server CWD); clamped to catalog.root"
    )]
    pub root: Option<String>,
    #[schemars(description = "The rg pattern to search for")]
    pub pattern: String,
    #[serde(default)]
    #[schemars(description = "An optional rg --glob to restrict matched files (e.g. '*.rs')")]
    pub glob: Option<String>,
}

/// `read`: one file's contents inside the repo `slug`, path clamped to the repo.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadRequest {
    #[schemars(description = "The repo slug (<org|user>/<name>) whose file to read")]
    pub slug: String,
    #[schemars(description = "Repo-relative file path (no .. / absolute / .git escapes)")]
    pub path: String,
    #[serde(default)]
    #[schemars(
        description = "1-based first line to return (with end_line, reads a bounded range)"
    )]
    pub start_line: Option<usize>,
    #[serde(default)]
    #[schemars(description = "1-based last line to return (inclusive)")]
    pub end_line: Option<usize>,
}

/// `deps`: pass exactly ONE of `dependency` (repos using it) or `slug` (its deps).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DepsRequest {
    #[serde(default)]
    #[schemars(description = "Find repos that depend on this dependency name")]
    pub dependency: Option<String>,
    #[serde(default)]
    #[schemars(description = "Find the dependency list of this repo slug")]
    pub slug: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChangeGetRequest {
    #[schemars(description = "The change id (e.g. GX-2026-07-12T...)")]
    pub change_id: String,
    /// Restrict the returned proposal diffs to one repo slug (omit for all).
    #[serde(default)]
    #[schemars(description = "Return only this repo's full proposal diff; omit for every repo")]
    pub slug: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateProposeRequest {
    #[schemars(description = "The prompt handed to the agent per repo")]
    pub prompt: String,
    #[serde(default)]
    #[schemars(description = "Repo slug patterns the campaign targets; empty matches all")]
    pub patterns: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateApplyRequest {
    #[schemars(description = "The change id whose persisted proposal to apply")]
    pub change_id: String,
    #[schemars(
        description = "The confirm token returned by create-propose (binds the reviewed bytes)"
    )]
    pub token: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoPlanRequest {
    #[schemars(description = "The change id to plan an undo for")]
    pub change_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoExecuteRequest {
    #[schemars(description = "The change id to undo")]
    pub change_id: String,
    #[schemars(description = "The token returned by undo-plan (refused if state changed since)")]
    pub token: String,
}

// ----------------------------------------------------------------- responses

#[derive(Debug, Serialize)]
pub struct RepoRef {
    pub slug: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct RepoStatusSummary {
    pub slug: String,
    pub branch: Option<String>,
    pub clean: bool,
    pub remote: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChangeSummary {
    pub change_id: String,
    pub status: String,
    pub description: Option<String>,
    pub repos: usize,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct RepoChangeSummary {
    pub slug: String,
    pub status: String,
    pub branch: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoProposalDetail {
    pub slug: String,
    pub outcome: String,
    pub files: Vec<String>,
    /// The full unified diff for this repo (change-get is the full-diff fetch,
    /// unlike create-propose which returns only summaries). `None` if the
    /// display patch is missing.
    pub patch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProposalDetail {
    pub change_id: String,
    pub prompt: String,
    pub repos: Vec<RepoProposalDetail>,
}

#[derive(Debug, Serialize)]
pub struct ChangeDetail {
    pub change_id: String,
    pub status: String,
    pub description: Option<String>,
    pub repos: Vec<RepoChangeSummary>,
    /// Present iff a persisted proposal exists on disk for this change.
    pub proposal: Option<ProposalDetail>,
}

#[derive(Debug, Serialize)]
pub struct ReviewRepo {
    pub slug: String,
    pub status: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReviewChange {
    pub change_id: String,
    pub repos: Vec<ReviewRepo>,
}

/// Per-repo propose SUMMARY (files + diff-stat), never the full diff: fleet-
/// sized diffs blow the protocol response limit, so `change-get` fetches a
/// single repo's full patch when a driver needs it.
#[derive(Debug, Serialize)]
pub struct RepoProposeSummary {
    pub slug: String,
    pub outcome: String,
    pub files: Vec<String>,
    pub files_changed: usize,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProposeOut {
    pub change_id: String,
    pub token: String,
    pub proposed: usize,
    pub empty: usize,
    pub failed: usize,
    pub repos: Vec<RepoProposeSummary>,
}

#[derive(Debug, Serialize)]
pub struct RepoApplyOut {
    pub slug: String,
    pub status: String,
    pub pr_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApplyOut {
    pub change_id: String,
    pub applied: usize,
    pub drifted_or_failed: usize,
    pub repos: Vec<RepoApplyOut>,
}

#[derive(Debug, Serialize)]
pub struct UndoPlanEntry {
    pub slug: String,
    pub action: String,
    pub pr_number: Option<u64>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UndoPlanOut {
    pub change_id: String,
    pub token: String,
    pub actionable: usize,
    pub plan: Vec<UndoPlanEntry>,
}

#[derive(Debug, Serialize)]
pub struct UndoOutcomeOut {
    pub slug: String,
    pub outcome: String,
    pub pr_number: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct UndoExecuteOut {
    pub change_id: String,
    pub repos: Vec<UndoOutcomeOut>,
}
