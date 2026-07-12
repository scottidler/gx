//! gx-mcp server: the curated stdio MCP surface fronting the gx cores.
//!
//! Ten tools (design doc `2026-07-12-llm-propose-apply-and-mcp-server.md`,
//! API Design > MCP tools):
//! - read-only (default ENABLED): `status`, `repo-discover`, `change-list`,
//!   `change-get`, `review-status`, `doctor`.
//! - mutating (default DISABLED): `create-propose`, `create-apply`,
//!   `undo-plan`, `undo-execute`.
//!
//! Gating (ringer #5 config): each tool has an `enabled:` under `mcp.tools` in
//! `gx.yml`; read-only default true, mutating default false. A DISABLED tool is
//! `disable_route`'d out of the router at construction, so it is ABSENT from
//! `tools/list` AND its call is REJECTED ("tool not found") — writes impossible
//! by default. Enabling a mutating tool is the operator's explicit grant.
//!
//! Trust model (ringer #6): the confirm token proves the caller received the
//! exact plan/proposal bytes; it does NOT prove human review, and stdio-local
//! is the only caller auth. Enabling a mutating tool grants that client the
//! same authority as a shell with `--yes`; the token prevents stale-plan
//! execution, not unreviewed execution.
//!
//! Every gx core is blocking (git/gh shell-outs, rayon pools), so each tool
//! runs its body under `tokio::task::spawn_blocking` — the async runtime is
//! never blocked (rust.md).

use std::sync::Arc;

use gx::config::Config;
use log::{debug, info, warn};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};

use crate::gate;
use crate::logic;
use crate::schema::{
    ChangeGetRequest, CreateApplyRequest, CreateProposeRequest, NoArgs, RepoDiscoverRequest,
    StatusRequest, UndoExecuteRequest, UndoPlanRequest,
};

/// gx-mcp's rmcp server handle. Cloned per connection by rmcp; cheap because
/// both fields are cheap to clone (`Arc<Config>` + a `ToolRouter` of `Arc`ed
/// routes).
#[derive(Clone)]
pub struct GxMcpServer {
    config: Arc<Config>,
    tool_router: ToolRouter<Self>,
}

impl GxMcpServer {
    /// Build the server, applying per-tool config gating: every tool the config
    /// reports as disabled is removed from the router (absent from `tools/list`,
    /// rejected by `call`). Read-only tools default enabled, mutating disabled.
    pub fn new(config: Config) -> Self {
        info!("GxMcpServer::new");
        let mut tool_router = Self::tool_router();
        for tool in gate::ALL {
            if !gate::tool_enabled(&config, tool) {
                debug!(
                    "GxMcpServer::new: gating off disabled tool {}",
                    gate::name(tool)
                );
                tool_router.disable_route(gate::name(tool));
            }
        }
        debug!(
            "GxMcpServer::new: {} tool(s) enabled after gating",
            tool_router.list_all().len()
        );
        Self {
            config: Arc::new(config),
            tool_router,
        }
    }

    /// The kebab-case names of the currently-enabled tools (for `get_info`).
    fn enabled_tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }
}

/// Run a blocking tool body off the async runtime and map its result to a
/// `CallToolResult`. A core error is a caller-visible tool-level error (the
/// driver sees WHY a mutation was refused — token mismatch, drift, ...); a
/// panicked task is a protocol-level internal error.
async fn run_blocking<T, F>(f: F) -> Result<CallToolResult, ErrorData>
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> eyre::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(CallToolResult::success(vec![ContentBlock::json(&value)?])),
        Ok(Err(e)) => {
            warn!("gx-mcp tool refused/failed: {e:#}");
            Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "{e:#}"
            ))]))
        }
        Err(join) => Err(ErrorData::internal_error(
            format!("tool task panicked: {join}"),
            None,
        )),
    }
}

#[tool_router]
impl GxMcpServer {
    // ---- read-only ------------------------------------------------------

    #[tool(
        name = "status",
        description = "Git status across the discovered fleet (local by default; set fetch_remote for ahead/behind)."
    )]
    async fn status(&self, params: Parameters<StatusRequest>) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: status");
        let config = self.config.clone();
        let StatusRequest {
            patterns,
            fetch_remote,
        } = params.0;
        run_blocking(move || logic::status(&config, &patterns, fetch_remote)).await
    }

    #[tool(
        name = "repo-discover",
        description = "Discover repos under the server's CWD, filtered by slug patterns."
    )]
    async fn repo_discover(
        &self,
        params: Parameters<RepoDiscoverRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: repo-discover");
        let config = self.config.clone();
        let patterns = params.0.patterns;
        run_blocking(move || logic::repo_discover(&config, &patterns)).await
    }

    #[tool(
        name = "change-list",
        description = "List every persisted gx change with its aggregate status."
    )]
    async fn change_list(&self, _params: Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: change-list");
        run_blocking(logic::change_list).await
    }

    #[tool(
        name = "change-get",
        description = "Get one change's per-repo state plus its full proposal diffs (optionally one repo)."
    )]
    async fn change_get(
        &self,
        params: Parameters<ChangeGetRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: change-get");
        let ChangeGetRequest { change_id, slug } = params.0;
        run_blocking(move || logic::change_get(&change_id, slug.as_deref())).await
    }

    #[tool(
        name = "review-status",
        description = "List PR-bearing changes and each repo's PR review state."
    )]
    async fn review_status(
        &self,
        _params: Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: review-status");
        run_blocking(logic::review_status).await
    }

    #[tool(
        name = "doctor",
        description = "Environment + artifact health: tool versions, orphaned/stuck artifacts."
    )]
    async fn doctor(&self, _params: Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: doctor");
        run_blocking(logic::doctor).await
    }

    // ---- mutating (default disabled) ------------------------------------

    #[tool(
        name = "create-propose",
        description = "Run the agent per matched repo and persist a reviewable proposal; returns per-repo summaries and a confirm token."
    )]
    async fn create_propose(
        &self,
        params: Parameters<CreateProposeRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: create-propose");
        let config = self.config.clone();
        let CreateProposeRequest { prompt, patterns } = params.0;
        run_blocking(move || logic::create_propose(&config, &prompt, &patterns)).await
    }

    #[tool(
        name = "create-apply",
        description = "Apply a persisted proposal; requires the confirm token from create-propose (refused on mismatch)."
    )]
    async fn create_apply(
        &self,
        params: Parameters<CreateApplyRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: create-apply");
        let config = self.config.clone();
        let CreateApplyRequest { change_id, token } = params.0;
        run_blocking(move || logic::create_apply(&config, &change_id, &token)).await
    }

    #[tool(
        name = "undo-plan",
        description = "Reconcile and return the undo plan for a change plus a token bound to that plan."
    )]
    async fn undo_plan(
        &self,
        params: Parameters<UndoPlanRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: undo-plan");
        let config = self.config.clone();
        let change_id = params.0.change_id;
        run_blocking(move || logic::undo_plan(&config, &change_id)).await
    }

    #[tool(
        name = "undo-execute",
        description = "Execute an undo; requires the token from undo-plan (refused if state changed since)."
    )]
    async fn undo_execute(
        &self,
        params: Parameters<UndoExecuteRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        info!("gx-mcp tool: undo-execute");
        let config = self.config.clone();
        let UndoExecuteRequest { change_id, token } = params.0;
        run_blocking(move || logic::undo_execute(&config, &change_id, &token)).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GxMcpServer {
    fn get_info(&self) -> ServerInfo {
        let enabled = self.enabled_tool_names();
        info!("GxMcpServer::get_info: {} enabled tool(s)", enabled.len());
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut info = ServerInfo::new(capabilities);
        info.instructions = Some(format!(
            "gx-mcp: MCP surface for gx fleet campaigns. Enabled tools: {}. \
             TRUST MODEL: enabling a mutating tool grants this client the same \
             authority as a shell running `gx ... --yes`. The confirm token \
             (create-propose -> create-apply, undo-plan -> undo-execute) \
             prevents STALE-PLAN execution, not UNREVIEWED execution; stdio-local \
             is the only caller auth. Mutating tools are disabled by default; \
             enable them under `mcp.tools` in gx.yml.",
            if enabled.is_empty() {
                "(none)".to_string()
            } else {
                enabled.join(", ")
            }
        ));
        debug!("GxMcpServer::get_info: {info:?}");
        info
    }
}
