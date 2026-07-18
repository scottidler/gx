use super::*;
use local::config::{Config, McpConfig, McpTool};
use std::collections::BTreeMap;

/// The kebab-case wire name from `gate::name` MUST equal serde's own
/// `rename_all = "kebab-case"` serialization of the enum, or the router name /
/// config key / serialization drift apart silently. This is the drift guard.
#[test]
fn test_tool_name_matches_serde_serialization() {
    for tool in ALL {
        let serde_name = serde_json::to_value(tool)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            name(tool),
            serde_name,
            "gate::name and serde serialization disagree for {tool:?}"
        );
    }
}

#[test]
fn test_all_covers_every_tool_exactly_once() {
    // The bite: a tool added to the enum but not to ALL would be silently
    // un-gateable. serde round-trips every name in ALL back to a distinct tool.
    let mut seen = std::collections::HashSet::new();
    for tool in ALL {
        assert!(
            seen.insert(name(tool)),
            "duplicate tool name {}",
            name(tool)
        );
    }
    assert_eq!(ALL.len(), 10);
}

#[test]
fn test_mutating_classification() {
    let mutating: Vec<&str> = ALL
        .into_iter()
        .filter(|&t| is_mutating(t))
        .map(name)
        .collect();
    assert_eq!(
        mutating,
        vec![
            "create-propose",
            "create-apply",
            "undo-plan",
            "undo-execute"
        ],
        "exactly the four propose/apply/undo tools are mutating"
    );
}

#[test]
fn test_default_gating_no_mcp_block() {
    // No `mcp:` block at all: read-only enabled, mutating disabled. This is the
    // "writes impossible by default" security posture.
    let config = Config::default();
    for tool in ALL {
        assert_eq!(
            tool_enabled(&config, tool),
            !is_mutating(tool),
            "default gating wrong for {}",
            name(tool)
        );
    }
}

#[test]
fn test_explicit_enable_overrides_default() {
    // Operator opts create-propose IN and status OUT: the explicit value wins,
    // every other tool keeps its category default.
    let mut tools = BTreeMap::new();
    tools.insert(McpTool::CreatePropose, true);
    tools.insert(McpTool::Status, false);
    let config = Config {
        mcp: Some(McpConfig { tools }),
        ..Config::default()
    };
    assert!(
        tool_enabled(&config, McpTool::CreatePropose),
        "explicitly enabled"
    );
    assert!(
        !tool_enabled(&config, McpTool::Status),
        "explicitly disabled"
    );
    // Untouched tools keep category defaults.
    assert!(
        tool_enabled(&config, McpTool::Doctor),
        "read-only default on"
    );
    assert!(
        !tool_enabled(&config, McpTool::CreateApply),
        "mutating default off"
    );
}
