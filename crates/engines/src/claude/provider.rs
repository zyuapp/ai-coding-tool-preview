use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::app_plugin::app_plugin_root;
use crate::binary::{engine_binary_path, CLAUDE_BINARY};
use crate::brief::{run_brief, BriefDialect, RunBrief};
use crate::catalog::{claude_effort, model_takes_effort};
use crate::pooled_provider::{PooledProvider, SessionEngine};
use crate::provider::RunInput;
use crate::session_pool::{SessionHooks, SessionPool};
use crate::tools::{grants_tool, role_limits, withheld_tools, ToolReach};
use crate::types::{AgentEngine, ComputerUseRunConfig};

use super::query::{ClaudeOptions, SdkMcpServer, SystemPrompt};
use super::session::{claude_permission_mode, ClaudeSession};

const CLAUDE_ENTRYPOINT: &str = "aicodingtool";

pub const CLAUDE_DIALECT: BriefDialect = BriefDialect {
    native_work: "Claude background tasks, sessions, or agents; native TaskOutput, Agent, and SendMessage cannot access them",
    native_worktree: Some("the Agent tool's worktree isolation"),
    coordinator_reads: "Your own tools read and search; none edits files or runs shell commands.",
    own_checkout_commits: None,
};

const CHROME_INSTRUCTIONS: &str = "The user's own Chrome answers the mcp__claude-in-chrome__ tools, and those tools drive the windows and tabs they already have on screen: when they ask for the external browser, for their browser, or for Chrome by name, use them rather than the AICodingTool browser panel or an open command through Bash. Everything else stays in the panel, which reads a page without disturbing what the user is looking at.";

/// Claude's tools that change nothing and start no subagent: all a read-only run is given.
pub const READ_ONLY_TOOLS: [&str; 12] = [
    "Read",
    "Grep",
    "Glob",
    "LSP",
    "WebSearch",
    "WebFetch",
    "Skill",
    "ToolSearch",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "EnterPlanMode",
    "ExitPlanMode",
];

/// The Claude Code the user installed.
pub fn claude_executable() -> Option<String> {
    engine_binary_path(&CLAUDE_BINARY).map(|path| path.to_string_lossy().into_owned())
}

/// How every Claude process is started: the command, this app's name on its requests, and no
/// Artifact skills, which the app cannot show.
pub fn claude_process() -> anyhow::Result<ClaudeOptions> {
    let executable =
        claude_executable().ok_or_else(|| anyhow::anyhow!("Claude is not installed."))?;
    let mut env: HashMap<String, String> = act_platform::process::environment();
    env.insert("CLAUDE_CODE_ENTRYPOINT".into(), CLAUDE_ENTRYPOINT.into());
    env.insert("CLAUDE_CODE_DISABLE_ARTIFACT".into(), "1".into());
    Ok(ClaudeOptions {
        executable,
        env,
        ..Default::default()
    })
}

/// The app's own plugin, when launch said where it is.
pub fn app_plugin_dirs() -> Vec<String> {
    app_plugin_root().into_iter().collect()
}

pub fn setting_sources(projectless: bool) -> Vec<String> {
    if projectless {
        vec!["user".into()]
    } else {
        vec!["user".into(), "project".into(), "local".into()]
    }
}

/// Whether the run drives the user's own Chrome. A read-only run never does.
fn drives_chrome(input: &RunInput) -> bool {
    input.chrome_browser && !role_limits(input.coordination_role).read_only
}

/// Everything a session is built with. A run that disagrees with any of it needs a session of its own.
fn session_key(input: &RunInput, brief: &RunBrief) -> String {
    let mut identity = brief.identity.as_array().cloned().unwrap_or_default();
    identity.push(json!(grants_tool(
        ToolReach::Workspace,
        Some(input.scope())
    )));
    identity.push(json!(drives_chrome(input)));
    Value::Array(identity).to_string()
}

/// The process options of a run's session.
fn run_options(input: &RunInput, brief: &RunBrief) -> anyhow::Result<ClaudeOptions> {
    let mut options = claude_process()?;
    let continuation = input.continuation_value().map(String::from);
    let mut mcp_servers = Map::new();
    if let ComputerUseRunConfig::Available { mcp } = &input.computer_use {
        mcp_servers.insert(
            "cua-driver".into(),
            json!({ "type": "stdio", "command": mcp.command, "args": mcp.args, "env": mcp.env }),
        );
    }
    let mut append = brief.instructions.clone();
    if drives_chrome(input) {
        append.push(CHROME_INSTRUCTIONS.to_string());
    }
    options.cwd = Some(input.workspace_root.clone());
    if role_limits(input.coordination_role).read_only {
        options.tools = Some(
            READ_ONLY_TOOLS
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
        );
    }
    options.disallowed_tools = withheld_tools(input.channel);
    options.fork_session = input.fork_continuation && continuation.is_some();
    options.resume = continuation;
    options.permission_mode = Some(claude_permission_mode(input.policy).into());
    options.allow_dangerously_skip_permissions =
        grants_tool(ToolReach::Workspace, Some(input.scope()));
    options.model = Some(input.model.clone());
    if model_takes_effort(AgentEngine::Claude, &input.model) {
        options.effort = Some(claude_effort(input.effort).as_str().into());
    }
    options.betas = vec!["context-1m-2025-08-07".into()];
    if drives_chrome(input) {
        options.extra_args.push(("chrome".into(), None));
    }
    options.mcp_servers = mcp_servers;
    options.sdk_mcp_servers = brief
        .tools
        .iter()
        .map(|set| SdkMcpServer {
            name: set.server.clone(),
            version: "1.0.0".into(),
            always_load: true,
            tools: set.tools.clone(),
        })
        .collect();
    options.system_prompt = SystemPrompt::Preset {
        append: Some(append.join("\n\n")),
    };
    options.setting_sources = Some(setting_sources(input.projectless));
    options.all_skills = true;
    options.plugin_dirs = app_plugin_dirs();
    options.forward_subagent_text = true;
    options.include_partial_messages = true;
    Ok(options)
}

/// How Claude Code opens a thread's session: one warm process per thread.
pub struct ClaudeSessions;

/// Runs threads on Claude Code.
pub type ClaudeAgentProvider = PooledProvider<ClaudeSessions>;

impl PooledProvider<ClaudeSessions> {
    pub fn new(pool: SessionPool) -> Self {
        PooledProvider::with_engine(pool, ClaudeSessions)
    }
}

impl SessionEngine for ClaudeSessions {
    type Session = ClaudeSession;

    fn session_key(&self, input: &RunInput) -> String {
        session_key(input, &run_brief(input, &CLAUDE_DIALECT))
    }

    fn open(&self, key: String, hooks: SessionHooks) -> Arc<ClaudeSession> {
        ClaudeSession::new(key, hooks)
    }

    fn start(&self, session: &Arc<ClaudeSession>, seed: &RunInput) {
        let brief = run_brief(seed, &CLAUDE_DIALECT);
        let options_input = seed.clone();
        session.open(
            seed,
            brief.tools.clone(),
            Box::new(move |can_use_tool, hooks| {
                let mut options = run_options(&options_input, &brief)?;
                options.can_use_tool = Some(can_use_tool);
                options.hooks = hooks;
                Ok(options)
            }),
        );
    }

    fn stop_process(session: &Arc<ClaudeSession>, process_id: &str) -> bool {
        session.stop_process(process_id);
        true
    }
}
