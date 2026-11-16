//! A run's tools put on the loopback host, as the MCP server configs an engine is handed.

use super::http_host::{McpHttpHost, ServedTools};
use super::server::{run_tools, server_briefs, BriefDialect, BriefRun, ServedToolSet, ToolRun};
use super::types::RunChannel;

/// One server as an engine's MCP config names it: streamable HTTP with a bearer token.
pub struct ServedServer {
    pub name: &'static str,
    pub tool_names: Vec<String>,
    pub served: ServedTools,
}

impl ServedServer {
    /// The `Authorization` header value the engine sends.
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.served.token)
    }
}

/// Everything a run is offered by the app, and what it is told about it. Released when dropped.
pub struct ServedRun {
    pub servers: Vec<ServedServer>,
    pub instructions: Vec<String>,
}

impl ServedRun {
    pub fn release(&self) {
        for server in &self.servers {
            server.served.release();
        }
    }
}

impl Drop for ServedRun {
    fn drop(&mut self) {
        self.release();
    }
}

/// Serves each app server under a token of its own, so tool names read `mcp__<server>__<tool>` as
/// every grant is written against. Servers left with no tools on the channel are still described.
pub async fn serve_run(host: &McpHttpHost, run: &ToolRun, channel: RunChannel, dialect: &BriefDialect, brief: &BriefRun) -> std::io::Result<ServedRun> {
    let sets = run_tools(run, channel);
    let instructions = server_briefs(&sets, dialect, brief);
    let mut servers = Vec::new();
    for ServedToolSet { server, tools } in sets {
        let tool_names = tools.iter().map(|tool| tool.name.clone()).collect();
        servers.push(ServedServer { name: server, tool_names, served: host.serve(tools).await? });
    }
    Ok(ServedRun { servers, instructions })
}

/// Serves every tool of the run under one token, for an engine that lists app tools flat. None when
/// the run is offered no tool at all.
pub async fn serve_flat(host: &McpHttpHost, sets: Vec<ServedToolSet>) -> std::io::Result<Option<ServedTools>> {
    let tools: Vec<_> = sets.into_iter().flat_map(|set| set.tools).collect();
    if tools.is_empty() {
        return Ok(None);
    }
    host.serve(tools).await.map(Some)
}
