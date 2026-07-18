//! gx's embedded MCP server, migrated from the standalone `gx-mcp` binary onto
//! `mcp-io` (the house scaffolding lib). [`server::GxMcpServer`] is the rmcp
//! `ServerHandler` fronting gx's campaign cores; `gx mcp serve` (via
//! `mcp_io::McpCmd`, intercepted in `main::run`) brings it up over stdio.
//!
//! The submodules are the same four that used to live under `gx-mcp/src/`:
//! `server` (the handler + tool wiring), `logic` (the blocking core each tool
//! calls under `spawn_blocking`), `schema` (the per-tool request types), and
//! `gate` (per-tool config gating).

pub mod gate;
pub mod logic;
pub mod schema;
pub mod server;
