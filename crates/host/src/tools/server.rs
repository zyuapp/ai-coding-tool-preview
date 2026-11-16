//! The app's MCP servers, described once so every engine serves, grants and briefs them alike.

use std::sync::Arc;

use super::automation::{AutomationBridge, FindingBridge, AUTOMATION_SERVER_NAME};
use super::browser::BrowserBridge;
use super::coordination::CoordinationBridge;
use super::definition::{BoundTool, ToolName};
use super::simulator::SimulatorBridge;
use super::terminal::TerminalBridge;
use super::threads::{ThreadBridge, THREAD_SERVER_NAME};
use super::types::{CoordinationRole, RunChannel};
use super::{automation, browser, computer_use, coordination, simulator, terminal, threads};

/// Which of a server's tools run without approval: every one, only its read-only ones, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grants {
    All,
    ReadOnly,
    None,
}

/// The few words an engine needs said its own way.
#[derive(Debug, Clone, Default)]
pub struct BriefDialect {
    /// The engine's own sessions and helpers, which an app thread ID never names.
    pub native_work: String,
    /// The engine's own way to make a worktree, which the app would never hear about.
    pub native_worktree: Option<String>,
    /// How a coordinator reads on this engine, said so it is told only of tools it really has.
    pub coordinator_reads: String,
    /// How a member in a checkout of its own commits there, where the engine's sandbox needs it said.
    pub own_checkout_commits: Option<String>,
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

/// One of the app's own tools, by the server it is filed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithheldTool {
    pub server: &'static str,
    pub tool: &'static str,
}

/// Asked through the UI instead, on every channel.
pub const ALWAYS_WITHHELD: [&str; 3] = ["AskUserQuestion", "EnterWorktree", "ExitWorktree"];

/// A side chat is discarded when it closes, so it must not leave anything scheduled behind, raise a
/// finding no list shows, or move a checkout it shares with its thread.
pub fn withheld_by_channel(channel: RunChannel) -> &'static [WithheldTool] {
    const SIDE: [WithheldTool; 6] = [
        WithheldTool { server: AUTOMATION_SERVER_NAME, tool: "schedule" },
        WithheldTool { server: AUTOMATION_SERVER_NAME, tool: "update" },
        WithheldTool { server: AUTOMATION_SERVER_NAME, tool: "stop" },
        WithheldTool { server: AUTOMATION_SERVER_NAME, tool: "notify" },
        WithheldTool { server: AUTOMATION_SERVER_NAME, tool: "nothing_to_report" },
        WithheldTool { server: THREAD_SERVER_NAME, tool: "move_to_worktree" },
    ];
    match channel {
        RunChannel::Main => &[],
        RunChannel::Side => &SIDE,
    }
}

/// The names Claude is told not to use on a channel.
pub fn withheld_tools(channel: RunChannel) -> Vec<String> {
    ALWAYS_WITHHELD.iter().map(|name| name.to_string()).chain(withheld_by_channel(channel).iter().map(|entry| mcp_tool_name(entry.server, entry.tool))).collect()
}

/// A server's tools short of what the channel withholds.
pub fn offered_on(channel: RunChannel, server: &str, tools: Vec<BoundTool>) -> Vec<BoundTool> {
    let withheld = withheld_by_channel(channel);
    tools.into_iter().filter(|tool| !withheld.iter().any(|entry| entry.server == server && entry.tool == tool.name)).collect()
}

pub struct ServedToolSet {
    pub server: &'static str,
    pub tools: Vec<BoundTool>,
}

/// Every app tool a run reaches, grouped by server and already short of what the channel withholds.
pub fn run_tools(run: &ToolRun, channel: RunChannel) -> Vec<ServedToolSet> {
    tool_servers().into_iter().filter_map(|server| (server.serve)(run).map(|tools| ServedToolSet { server: server.name, tools: offered_on(channel, server.name, tools) })).collect()
}

/// What every server tells the run, gated on the tools it was left.
pub fn server_briefs(tools: &[ServedToolSet], dialect: &BriefDialect, run: &BriefRun) -> Vec<String> {
    tool_servers()
        .into_iter()
        .flat_map(|server| {
            let set = tools.iter().find(|set| set.server == server.name).map(|set| set.tools.as_slice());
            (server.brief)(&Offers::new(set), dialect, run)
        })
        .collect()
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
    use crate::automation::{AutomationDraft, AutomationPatch, AutomationView};
    use futures::future::BoxFuture;
    use futures::FutureExt;

    struct NoAutomation;

    impl AutomationBridge for NoAutomation {
        fn read(&self) -> BoxFuture<'_, Result<Option<AutomationView>, String>> {
            async { Ok(None) }.boxed()
        }
        fn list(&self) -> BoxFuture<'_, Result<Vec<AutomationView>, String>> {
            async { Ok(Vec::new()) }.boxed()
        }
        fn save(&self, _draft: AutomationDraft) -> BoxFuture<'_, Result<AutomationView, String>> {
            async { Err("no".to_string()) }.boxed()
        }
        fn update(&self, _patch: AutomationPatch) -> BoxFuture<'_, Result<AutomationView, String>> {
            async { Err("no".to_string()) }.boxed()
        }
        fn remove(&self) -> BoxFuture<'_, Result<bool, String>> {
            async { Ok(false) }.boxed()
        }
    }

    #[test]
    fn a_side_chat_reads_automations_but_is_not_told_to_schedule() {
        let run = ToolRun { automations: Some(Arc::new(NoAutomation)), ..Default::default() };
        let main = run_tools(&run, RunChannel::Main);
        let side = run_tools(&run, RunChannel::Side);
        let names = |sets: &[ServedToolSet]| sets[0].tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&main), vec!["schedule", "status", "update", "stop", "list_all"]);
        assert_eq!(names(&side), vec!["status", "list_all"]);
        let dialect = BriefDialect::default();
        assert_eq!(server_briefs(&main, &dialect, &BriefRun::default()).len(), 1);
        assert!(server_briefs(&side, &dialect, &BriefRun::default()).is_empty());
    }

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
