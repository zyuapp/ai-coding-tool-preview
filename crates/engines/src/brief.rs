//! What every engine tells a run beyond its prompt, and what a warm session must agree on.

use serde_json::{json, Value};

pub use act_core::domain::engine::BriefDialect;

use crate::provider::RunInput;
use crate::tools::{offered_on, ServedToolSet};
use crate::types::{ComputerUseRunConfig, CoordinationRole, RunChannel};

/// Shared by every provider when opening or resuming a side chat.
pub const SIDE_CHAT_INSTRUCTIONS: &str = include_str!("text/side_chat_instructions.txt");

/// Appended after inherited history, before the first question, without starting a separate turn.
pub const SIDE_CHAT_BOUNDARY: &str = include_str!("text/side_chat_boundary.txt");

/// A side chat takes the boundary once, as it opens on a conversation of its own.
pub fn needs_side_chat_boundary(input: &RunInput) -> bool {
    input.channel == RunChannel::Side
        && (input.continuation_value().is_none() || input.fork_continuation)
}

/// What a server's brief may read of the run besides what it was offered.
#[derive(Debug, Clone)]
pub struct BriefRun<'a> {
    pub coordination_role: Option<CoordinationRole>,
    pub workspace_root: &'a str,
    pub project_root: Option<&'a str>,
}

/// Whether the run is offered a server at all (`tool` None), or that tool of it.
pub type Offers<'a> = &'a dyn Fn(&str, Option<&str>) -> bool;

/// The app's tool servers as one run reaches them. The host builds one per run.
pub trait RunToolkit: Send + Sync {
    /// Every tool set the run is offered, in server order, before its channel withholds any. A tool
    /// may report into the run through `run.hooks.emit`, as computer-use setup does.
    fn tool_sets(&self, run: &RunInput) -> Vec<ServedToolSet>;
    /// What the servers tell the run, read from what its channel left it, in server order.
    fn brief(&self, offers: Offers<'_>, dialect: &BriefDialect, run: &BriefRun<'_>) -> Vec<String>;
}

/// The brief every engine gives a run, the tools it offers, and what a warm session must agree on.
#[derive(Debug, Clone)]
pub struct RunBrief {
    pub instructions: Vec<String>,
    pub tools: Vec<ServedToolSet>,
    pub identity: Value,
}

const LINK_INSTRUCTIONS: &str = "Only Markdown links are clickable in your output. Link web pages as [label](https://example.com), workspace files as [label](/absolute/path:line), pushed commits as [short hash](https://github.com/owner/repo/commit/<hash>) with their repository's real host and path, and other threads as [title](aicodingtool://thread/<id>). Omit the line when it is unavailable.";
const COMPUTER_USE_INSTRUCTIONS: &str = "When a requested outcome lives in another application's interface, use the provided computer-use MCP tools. Never invoke a separately installed cua-driver through Bash. Observe the exact target before every action and verify the result afterward. Prefer accessibility targets, then screenshot coordinates, and use foreground delivery only after background delivery fails.";
const WORKFLOW_INSTRUCTIONS: &str = "For ordinary AICodingTool operations, prefer the AICodingTool tools. They are defaults, not restrictions on the user's workflow: follow the user's explicitly requested target and method, and verify the result in the requested target before reporting success.";

/// Every app tool a run reaches, grouped by server and short of what the channel withholds.
pub fn run_tools(input: &RunInput) -> Vec<ServedToolSet> {
    input
        .toolkit
        .as_ref()
        .map(|toolkit| {
            toolkit
                .tool_sets(input)
                .into_iter()
                .map(|set| offered_on(input.channel, set))
                .collect()
        })
        .unwrap_or_default()
}

fn role_id(role: Option<CoordinationRole>) -> Value {
    match role {
        Some(CoordinationRole::Coordinator) => json!("coordinator"),
        Some(CoordinationRole::Member) => json!("member"),
        None => Value::Null,
    }
}

/// The brief every engine gives a run: the same words, gated on the same tools.
pub fn run_brief(input: &RunInput, dialect: &BriefDialect) -> RunBrief {
    let tools = run_tools(input);
    let offers = |server: &str, tool: Option<&str>| {
        tools
            .iter()
            .find(|set| set.server == server)
            .is_some_and(|set| tool.is_none_or(|name| set.tools.iter().any(|t| t.name == name)))
    };
    let run = BriefRun {
        coordination_role: input.coordination_role,
        workspace_root: &input.workspace_root,
        project_root: input.project_root.as_deref(),
    };
    let mut instructions = Vec::new();
    if !matches!(input.computer_use, ComputerUseRunConfig::Unavailable { .. }) {
        instructions.push(COMPUTER_USE_INSTRUCTIONS.to_string());
    }
    instructions.push(LINK_INSTRUCTIONS.to_string());
    if let Some(toolkit) = &input.toolkit {
        instructions.extend(toolkit.brief(&offers, dialect, &run));
    }
    if !tools.is_empty() {
        instructions.push(WORKFLOW_INSTRUCTIONS.to_string());
    }
    if input.channel == RunChannel::Side {
        instructions.push(SIDE_CHAT_INSTRUCTIONS.to_string());
    }
    let computer_use = match &input.computer_use {
        ComputerUseRunConfig::Available { mcp } => serde_json::to_value(mcp).unwrap_or(Value::Null),
        other => json!(other.status()),
    };
    let identity = json!([
        input.channel,
        input.workspace_root,
        input.projectless,
        computer_use,
        role_id(input.coordination_role),
        instructions,
        tools
            .iter()
            .map(|set| json!([
                set.server,
                set.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>()
            ]))
            .collect::<Vec<_>>(),
    ]);
    RunBrief {
        instructions,
        tools,
        identity,
    }
}
