use std::collections::HashMap;
use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};
use serde_json::{json, Map, Value};

use crate::app_plugin::app_plugin_root;
use crate::binary::{engine_binary_path, CLAUDE_BINARY};
use crate::brief::{run_brief, BriefDialect, RunBrief};
use crate::catalog::{claude_effort, model_takes_effort};
use crate::provider::{AgentProvider, ProviderResult, RunInput};
use crate::session_pool::{session_of, Filings, SessionOpener, SessionPool};
use crate::tools::{grants_tool, withheld_tools, ToolReach};
use crate::types::{AgentEngine, ComputerUseRunConfig, Continuation, CoordinationRole};

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

/// The only tools a coordinator's Claude is given: it reads, plans and delegates.
pub const COORDINATOR_TOOLS: [&str; 12] = [
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
    let mut env: HashMap<String, String> = std::env::vars().collect();
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

/// Whether the run drives the user's own Chrome. A coordinator never does.
fn drives_chrome(input: &RunInput) -> bool {
    input.chrome_browser && input.coordination_role != Some(CoordinationRole::Coordinator)
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
    if input.coordination_role == Some(CoordinationRole::Coordinator) {
        options.tools = Some(
            COORDINATOR_TOOLS
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

/// Runs threads on Claude Code, one warm process per thread.
#[derive(Clone)]
pub struct ClaudeAgentProvider {
    pool: SessionPool,
    filings: Filings,
}

impl ClaudeAgentProvider {
    pub fn new(pool: SessionPool) -> Self {
        ClaudeAgentProvider {
            pool,
            filings: Filings::default(),
        }
    }

    fn session(&self, task_id: &str) -> Option<Arc<ClaudeSession>> {
        session_of::<ClaudeSession>(&self.pool, task_id)
    }
}

impl AgentProvider for ClaudeAgentProvider {
    fn execute(&self, input: RunInput) -> BoxFuture<'static, ProviderResult> {
        let brief = run_brief(&input, &CLAUDE_DIALECT);
        let key = session_key(&input, &brief);
        let pool = self.pool.clone();
        let filing = self.filings.pending(&input.task_id);
        async move {
            if let Some(filing) = filing {
                filing.await;
            }
            let seed = input.clone();
            let session_key = key.clone();
            pool.execute(
                input,
                key,
                SessionOpener {
                    open: move |hooks| ClaudeSession::new(session_key, hooks),
                    start: move |session: &Arc<ClaudeSession>| {
                        let tool_sets = brief.tools.clone();
                        let options_input = seed.clone();
                        session.open(
                            &seed,
                            tool_sets,
                            Box::new(move |can_use_tool, hooks| {
                                let mut options = run_options(&options_input, &brief)?;
                                options.can_use_tool = Some(can_use_tool);
                                options.hooks = hooks;
                                Ok(options)
                            }),
                        );
                    },
                },
            )
            .await
        }
        .boxed()
    }

    fn stop_process(&self, task_id: &str, process_id: &str) -> bool {
        match self.session(task_id) {
            Some(session) => {
                session.stop_process(process_id);
                true
            }
            None => false,
        }
    }

    /// Claude keeps no record of its own to name, so the title stays the app's.
    fn label_thread(&self, _task_id: &str, _title: &str) -> bool {
        false
    }

    /// Claude keeps no archive of its own, so filing a thread away stays the app's.
    fn archive_thread(
        &self,
        _task_id: &str,
        _continuation: Option<Continuation>,
        _archived: bool,
    ) -> bool {
        false
    }

    fn retire_out_of_role(&self, task_id: &str, role: Option<CoordinationRole>) {
        tokio::spawn(self.pool.retire_out_of_role(task_id, role));
    }

    fn close_all(&self) {
        self.pool.close_all();
    }
}
