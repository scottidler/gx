//! MCP tool gating policy: which tools exist, which are mutating, their
//! kebab-case wire names, and whether a given `Config` enables one.
//!
//! This lives in the `mcp` module (beside the server that consumes it), not in
//! gx's `config.rs`: `config.rs` owns parsing the `mcp:` config block (ringer
//! #5, so a shared `gx.yml` validates under `deny_unknown_fields`), while the
//! gating POLICY belongs with its sole consumer, the embedded MCP server.
//!
//! Security posture (design Chunk B): read-only tools default ENABLED, mutating
//! tools default DISABLED. A tool absent from `mcp.tools` takes its category
//! default, so writes are impossible by default even with no `mcp:` block.

use local::config::{Config, McpTool};

/// Every tool in the curated surface, read-only first (stable order for
/// `get_info` / diagnostics).
pub const ALL: [McpTool; 14] = [
    McpTool::Status,
    McpTool::RepoDiscover,
    McpTool::ChangeList,
    McpTool::ChangeGet,
    McpTool::ReviewStatus,
    McpTool::Doctor,
    McpTool::Query,
    McpTool::Search,
    McpTool::Read,
    McpTool::Deps,
    McpTool::CreatePropose,
    McpTool::CreateApply,
    McpTool::UndoPlan,
    McpTool::UndoExecute,
];

/// True for the four mutating tools (propose/apply/undo-plan/undo-execute).
pub fn is_mutating(tool: McpTool) -> bool {
    matches!(
        tool,
        McpTool::CreatePropose | McpTool::CreateApply | McpTool::UndoPlan | McpTool::UndoExecute
    )
}

/// The kebab-case wire/config name for a tool. MUST match serde's
/// `rename_all = "kebab-case"` for `McpTool` (asserted by a test) so the router
/// name, the config key, and serialization never drift.
pub fn name(tool: McpTool) -> &'static str {
    match tool {
        McpTool::Status => "status",
        McpTool::RepoDiscover => "repo-discover",
        McpTool::ChangeList => "change-list",
        McpTool::ChangeGet => "change-get",
        McpTool::ReviewStatus => "review-status",
        McpTool::Doctor => "doctor",
        McpTool::Query => "query",
        McpTool::Search => "search",
        McpTool::Read => "read",
        McpTool::Deps => "deps",
        McpTool::CreatePropose => "create-propose",
        McpTool::CreateApply => "create-apply",
        McpTool::UndoPlan => "undo-plan",
        McpTool::UndoExecute => "undo-execute",
    }
}

/// Whether a tool is enabled for this config: the explicit `mcp.tools` value if
/// present, else the category default (mutating => disabled, read-only =>
/// enabled). An absent `mcp:` block means every tool takes its category default.
pub fn tool_enabled(config: &Config, tool: McpTool) -> bool {
    config
        .mcp
        .as_ref()
        .and_then(|mcp| mcp.tools.get(&tool).copied())
        .unwrap_or_else(|| !is_mutating(tool))
}

#[cfg(test)]
mod tests;
