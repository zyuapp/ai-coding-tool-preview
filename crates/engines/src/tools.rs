//! The app's own tools as engines serve them. The host defines the tools and binds them to its
//! bridges; engines serve them to their agents and decide which run without asking.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{CoordinationRole, ExecutionPolicy, RunChannel};

/// One piece of a tool's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![ToolContent::Text { text: text.into() }],
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![ToolContent::Text { text: text.into() }],
            is_error: true,
        }
    }
}

pub type ToolHandler = Arc<dyn Fn(Value) -> BoxFuture<'static, ToolResult> + Send + Sync>;

/// A tool definition with its bridge supplied, which is all a host needs to serve it.
#[derive(Clone)]
pub struct BoundTool {
    pub name: String,
    pub description: String,
    /// A JSON Schema object describing the arguments.
    pub input_schema: Value,
    /// A call changes nothing the user can see.
    pub read_only: bool,
    pub handler: ToolHandler,
}

impl std::fmt::Debug for BoundTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundTool")
            .field("name", &self.name)
            .finish()
    }
}

/// Which of a server's tools run without approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerGrants {
    All,
    ReadOnly,
    None,
}

/// The tools a run is offered under one of the app's servers.
#[derive(Debug, Clone)]
pub struct ServedToolSet {
    pub server: String,
    pub grants: ServerGrants,
    pub tools: Vec<BoundTool>,
}

/// How every engine names a tool an MCP server offers, and what the app's grants are written against.
pub fn mcp_tool_name(server: &str, name: &str) -> String {
    format!("mcp__{server}__{name}")
}

pub const AUTOMATION_SERVER_NAME: &str = "aicodingtool-automation";
pub const THREAD_SERVER_NAME: &str = "aicodingtool-threads";
const COMPUTER_USE_TOOL_PREFIX: &str = "mcp__cua-driver__";

/// Asked through the UI instead, on every channel.
const ALWAYS_WITHHELD: [&str; 3] = ["AskUserQuestion", "EnterWorktree", "ExitWorktree"];

/// The app tools a channel never reaches, by server and tool. A side chat is discarded when it
/// closes, so it must not leave anything scheduled behind.
pub fn withheld_by_channel(channel: RunChannel) -> &'static [(&'static str, &'static str)] {
    match channel {
        RunChannel::Main => &[],
        RunChannel::Side => &[
            (AUTOMATION_SERVER_NAME, "schedule"),
            (AUTOMATION_SERVER_NAME, "update"),
            (AUTOMATION_SERVER_NAME, "stop"),
            (AUTOMATION_SERVER_NAME, "notify"),
            (AUTOMATION_SERVER_NAME, "nothing_to_report"),
            (THREAD_SERVER_NAME, "move_to_worktree"),
        ],
    }
}

/// The names Claude is told not to use on a channel.
pub fn withheld_tools(channel: RunChannel) -> Vec<String> {
    ALWAYS_WITHHELD
        .iter()
        .map(|name| name.to_string())
        .chain(
            withheld_by_channel(channel)
                .iter()
                .map(|(server, tool)| mcp_tool_name(server, tool)),
        )
        .collect()
}

/// A server's tools short of what the channel withholds.
pub fn offered_on(channel: RunChannel, set: ServedToolSet) -> ServedToolSet {
    let withheld = withheld_by_channel(channel);
    let tools = set
        .tools
        .into_iter()
        .filter(|tool| {
            !withheld
                .iter()
                .any(|(server, name)| *server == set.server && *name == tool.name)
        })
        .collect();
    ServedToolSet { tools, ..set }
}

/// What a tool reaches: only the app's own bridges, the desktop the person works at, or the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolReach {
    App,
    ComputerUse,
    Workspace,
}

/// What a thread's place in a team keeps from it, whichever engine runs it. Each engine holds it
/// to these its own way: a tool list, a sandbox, or permission rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoleLimits {
    /// Changes nothing itself: no edits, no command that could write, and not the user's own browser.
    pub read_only: bool,
    /// Hands work only to threads the user can see, never to subagents of the engine's own.
    pub no_subagents: bool,
    /// Asks the user only through the app's decisions, which the coordinator's Overview shows.
    pub no_native_questions: bool,
}

/// A coordinator reads, plans and delegates; it and its members put questions to the user as decisions.
pub fn role_limits(role: Option<CoordinationRole>) -> RoleLimits {
    let coordinator = role == Some(CoordinationRole::Coordinator);
    RoleLimits {
        read_only: coordinator,
        no_subagents: coordinator,
        no_native_questions: role.is_some(),
    }
}

/// What a run may do and where it is read: together they decide what runs without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalScope {
    pub policy: ExecutionPolicy,
    pub channel: RunChannel,
}

/// What a tool reaches, read from its `mcp__server__tool` name against the app servers a run is
/// offered: a tool filed under a server that grants all, or a read-only tool of one that grants those.
pub fn tool_reach(tool_name: &str, sets: &[ServedToolSet]) -> ToolReach {
    for set in sets {
        let prefix = mcp_tool_name(&set.server, "");
        let Some(tool) = tool_name.strip_prefix(&prefix) else {
            continue;
        };
        match set.grants {
            ServerGrants::All => return ToolReach::App,
            ServerGrants::ReadOnly if set.tools.iter().any(|t| t.name == tool && t.read_only) => {
                return ToolReach::App
            }
            _ => {}
        }
    }
    if tool_name.starts_with(COMPUTER_USE_TOOL_PREFIX) {
        ToolReach::ComputerUse
    } else {
        ToolReach::Workspace
    }
}

/// Whether a tool runs without anybody approving it. A call with no run in scope has nobody to grant
/// anything. Computer use is granted by an autonomous run on the main channel.
pub fn grants_tool(reach: ToolReach, scope: Option<ApprovalScope>) -> bool {
    if reach == ToolReach::App {
        return true;
    }
    let Some(scope) = scope else { return false };
    if scope.policy == ExecutionPolicy::Bypass {
        return true;
    }
    reach == ToolReach::ComputerUse
        && scope.channel == RunChannel::Main
        && scope.policy == ExecutionPolicy::Autonomous
}

/// What a served set is reachable as over HTTP: the address and the bearer token naming the set.
pub struct ServedTools {
    pub url: String,
    pub token: String,
    release: Option<Box<dyn FnOnce() + Send>>,
}

impl ServedTools {
    pub fn new(url: String, token: String, release: impl FnOnce() + Send + 'static) -> Self {
        ServedTools {
            url,
            token,
            release: Some(Box::new(release)),
        }
    }

    /// Forgets the token and drops every connection made with it.
    pub fn release(mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl Drop for ServedTools {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// Serves tool sets over MCP to engines that reach tools by URL (Codex, OpenCode).
pub trait ToolHost: Send + Sync {
    fn serve(&self, tools: Vec<BoundTool>) -> BoxFuture<'static, anyhow::Result<ServedTools>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, read_only: bool) -> BoundTool {
        BoundTool {
            name: name.into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only,
            handler: Arc::new(|_| Box::pin(async { ToolResult::text("") })),
        }
    }

    #[test]
    fn reach_follows_server_grants() {
        let sets = vec![
            ServedToolSet {
                server: "aicodingtool-browser".into(),
                grants: ServerGrants::ReadOnly,
                tools: vec![tool("browser_read", true), tool("browser_click", false)],
            },
            ServedToolSet {
                server: "aicodingtool-threads".into(),
                grants: ServerGrants::All,
                tools: vec![tool("list_threads", false)],
            },
        ];
        assert_eq!(
            tool_reach("mcp__aicodingtool-browser__browser_read", &sets),
            ToolReach::App
        );
        assert_eq!(
            tool_reach("mcp__aicodingtool-browser__browser_click", &sets),
            ToolReach::Workspace
        );
        assert_eq!(
            tool_reach("mcp__aicodingtool-threads__anything", &sets),
            ToolReach::App
        );
        assert_eq!(
            tool_reach("mcp__cua-driver__click", &sets),
            ToolReach::ComputerUse
        );
        let main_auto = ApprovalScope {
            policy: ExecutionPolicy::Autonomous,
            channel: RunChannel::Main,
        };
        let side_auto = ApprovalScope {
            policy: ExecutionPolicy::Autonomous,
            channel: RunChannel::Side,
        };
        assert!(grants_tool(ToolReach::ComputerUse, Some(main_auto)));
        assert!(!grants_tool(ToolReach::ComputerUse, Some(side_auto)));
        assert!(!grants_tool(ToolReach::Workspace, None));
    }

    #[test]
    fn side_chats_lose_scheduling_tools() {
        let set = ServedToolSet {
            server: AUTOMATION_SERVER_NAME.into(),
            grants: ServerGrants::All,
            tools: vec![tool("schedule", false), tool("status", true)],
        };
        let offered = offered_on(RunChannel::Side, set);
        assert_eq!(
            offered
                .tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["status"]
        );
    }
}
