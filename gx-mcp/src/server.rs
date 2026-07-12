//! gx-mcp server: stdio MCP surface fronting the gx cores.
//!
//! Phase 8 scaffold serves ZERO tools -- the initialize handshake and an
//! empty tool list are the whole surface. Phase 9 wires the curated
//! read-only/mutating tool set (`#[tool]` methods added to the
//! `#[tool_router]` impl block below) with per-tool config gating and the
//! confirm-token protocol.

use log::{debug, info};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{tool_handler, tool_router, ServerHandler};

/// gx-mcp's rmcp server handle. Cloned per connection by rmcp; cheap because
/// `ToolRouter` is itself cheap to clone (it holds no per-call state).
#[derive(Clone)]
pub struct GxMcpServer {
    tool_router: ToolRouter<Self>,
}

impl GxMcpServer {
    /// Build the server and its (currently empty) tool router.
    pub fn new() -> Self {
        info!("GxMcpServer::new");
        let tool_router = Self::tool_router();
        debug!(
            "GxMcpServer::new: tool_router built with {} tools",
            tool_router.list_all().len()
        );
        Self { tool_router }
    }
}

impl Default for GxMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

// No #[tool] methods yet -- Phase 9 adds the curated tool surface here.
// The empty impl still gives `#[tool_handler]` below a real `tool_router`
// field to route through (zero tools registered, not zero routing).
#[tool_router]
impl GxMcpServer {}

#[tool_handler]
impl ServerHandler for GxMcpServer {
    fn get_info(&self) -> ServerInfo {
        let tool_count = self.tool_router.list_all().len();
        info!("GxMcpServer::get_info: MCP client requested server info (tool_count={tool_count})");
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut info = ServerInfo::new(capabilities);
        info.instructions = Some(format!(
            "gx-mcp: MCP surface for gx fleet campaigns. Scaffold build, \
             {tool_count} tools registered."
        ));
        debug!("GxMcpServer::get_info: {info:?}");
        info
    }
}
