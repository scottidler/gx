//! MCP tool gating policy: which tools exist, which are mutating, their
//! kebab-case wire names, and whether a given `Config` enables one.
//!
//! This lives in `gx-mcp` (not gx's `config.rs`) on purpose: gx's binary parses
//! the `mcp:` config block (ringer #5, so a shared `gx.yml` validates under
//! `deny_unknown_fields`) but never gates anything, so putting the policy in the
//! lib would be dead code in the `gx` bin target. The sole consumer owns it.
//!
//! Security posture (design Chunk B): read-only tools default ENABLED, mutating
//! tools default DISABLED. A tool absent from `mcp.tools` takes its category
//! default, so writes are impossible by default even with no `mcp:` block.

use crate::config::{Config, McpTool};

/// Every tool in the curated surface, read-only first (stable order for
/// `get_info` / diagnostics).
pub const ALL: [McpTool; 10] = [
    McpTool::Status,
    McpTool::RepoDiscover,
    McpTool::ChangeList,
    McpTool::ChangeGet,
    McpTool::ReviewStatus,
    McpTool::Doctor,
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
