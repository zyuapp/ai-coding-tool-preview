//! The app's MCP servers, described once so every engine serves, grants and briefs them alike.

use std::sync::Arc;

pub use act_core::domain::engine::BriefDialect;

use super::automation::{AutomationBridge, FindingBridge};
use super::browser::BrowserBridge;
use super::coordination::CoordinationBridge;
use super::definition::{BoundTool, ToolName};
use super::simulator::SimulatorBridge;
use super::terminal::TerminalBridge;
use super::threads::ThreadBridge;
use super::types::CoordinationRole;
use super::{automation, browser, computer_use, coordination, simulator, terminal, threads};

/// Which of a server's tools run without approval: every one, only its read-only ones, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grants {
    All,
    ReadOnly,
    None,
}

/// What a server's brief may read of the run besides what it was offered.
#[derive(Debug, Clone, Default)]
pub struct BriefRun {
    pub coordination_role: Option<CoordinationRole>,
    pub workspace_root: String,
    pub project_root: Option<String>,
}

/// Everything of the run any server binds to. A bridge left out is a server not offered.
#[derive(Clone)]
pub struct ToolRun {
    /// Present while the user still has to enable computer use; called when the run asks them to.
    pub computer_use_setup: Option<Arc<dyn Fn() + Send + Sync>>,
    pub automations: Option<Arc<dyn AutomationBridge>>,
    pub findings: Option<Arc<dyn FindingBridge>>,
    pub threads: Option<Arc<dyn ThreadBridge>>,
    pub coordination: Option<Arc<dyn CoordinationBridge>>,
    pub browser: Option<Arc<dyn BrowserBridge>>,
    pub simulator: Option<Arc<dyn SimulatorBridge>>,
    pub terminal: Option<Arc<dyn TerminalBridge>>,
    pub coordination_role: Option<CoordinationRole>,
    /// A native review is already isolated in a thread of the engine's own.
    pub review: bool,
    /// The clock idle times are measured against, in epoch milliseconds.
    pub now: fn() -> i64,
}

impl Default for ToolRun {
    fn default() -> Self {
        Self {
            computer_use_setup: None,
            automations: None,
            findings: None,
            threads: None,
            coordination: None,
            browser: None,
            simulator: None,
            terminal: None,
            coordination_role: None,
            review: false,
            now: || chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// Whether the run is offered the server at all, or, given a name, that tool of it.
pub struct Offers<'a> {
    tools: Option<&'a [BoundTool]>,
}

impl<'a> Offers<'a> {
    pub fn new(tools: Option<&'a [BoundTool]>) -> Self {
        Self { tools }
    }

    pub fn any(&self) -> bool {
        self.tools.is_some()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.is_some_and(|tools| tools.iter().any(|tool| tool.name == name))
    }
}

pub struct ToolServer {
    pub name: &'static str,
    /// Every tool the server can offer, whichever of them a run is given.
    pub definitions: Vec<ToolName>,
    pub grants: Grants,
    /// The tools a run is given, before its channel withholds any, or None when it is not offered the server.
    pub serve: fn(&ToolRun) -> Option<Vec<BoundTool>>,
    /// What the run is told, read from what its channel left it.
    pub brief: fn(&Offers, &BriefDialect, &BriefRun) -> Vec<String>,
}

/// Every app tool server, in the order a run is offered them and told of them.
pub fn tool_servers() -> Vec<ToolServer> {
    vec![computer_use::server(), automation::server(), threads::server(), coordination::server(), browser::server(), simulator::server(), terminal::server()]
}

/// How every engine names a tool an MCP server offers, and what the app's grants are written against.
pub fn mcp_tool_name(server: &str, name: &str) -> String {
    format!("mcp__{server}__{name}")
}

/// What a tool reaches: only the app's bridges, the desktop through computer use, or the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolReach {
    App,
    ComputerUse,
    Workspace,
}

const COMPUTER_USE_TOOL_PREFIX: &str = "mcp__cua-driver__";

/// What a tool reaches, read from its `mcp__server__tool` name. Every engine grants app tools by this.
pub fn tool_reach(tool_name: &str) -> ToolReach {
    let app = tool_servers().iter().any(|server| match server.grants {
        Grants::All => tool_name.starts_with(&mcp_tool_name(server.name, "")),
        Grants::ReadOnly => server.definitions.iter().any(|tool| tool.read_only && mcp_tool_name(server.name, tool.name) == tool_name),
        Grants::None => false,
    });
    if app {
        ToolReach::App
    } else if tool_name.starts_with(COMPUTER_USE_TOOL_PREFIX) {
        ToolReach::ComputerUse
    } else {
        ToolReach::Workspace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reach_follows_each_servers_grants() {
        assert_eq!(tool_reach("mcp__aicodingtool-automation__schedule"), ToolReach::App);
        assert_eq!(tool_reach("mcp__aicodingtool-threads__list_threads"), ToolReach::App);
        assert_eq!(tool_reach("mcp__aicodingtool-threads__start_thread"), ToolReach::Workspace);
        assert_eq!(tool_reach("mcp__aicodingtool-terminal__terminal_read"), ToolReach::Workspace);
        assert_eq!(tool_reach("mcp__aicodingtool-browser__browser_read"), ToolReach::App);
        assert_eq!(tool_reach("mcp__cua-driver__click"), ToolReach::ComputerUse);
        assert_eq!(tool_reach("Bash"), ToolReach::Workspace);
    }
}
