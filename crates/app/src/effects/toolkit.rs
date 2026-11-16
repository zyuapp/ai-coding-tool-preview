//! A run's reach into the app: the host's tool servers bound to the window's bridges for the thread
//! it runs as, served the way every engine serves them (src/main/agent-worker.mts, run-tools.mts).

use std::sync::{Arc, Mutex};

use act_engines::brief::{BriefDialect, BriefRun, Offers, RunToolkit};
use act_engines::provider::{ProviderEvent, RunInput};
use act_engines::tools::{BoundTool, ServedToolSet, ServerGrants, ToolResult};
use act_engines::types::{ComputerUseRunConfig, CoordinationRole, RunOperation};
use act_host::automation::scheduler::AutomationScheduler;
use act_host::automation::{AutomationError, ExecutionPolicy};
use act_host::thread_requests::{RequestHost, RunBridges};
use act_host::tools::automation::{AutomationBridge, ScheduledAutomations};
use act_host::tools::browser::BrowserBridge;
use act_host::tools::server::{tool_servers, Grants, ToolRun};
use act_host::tools::simulator::SimulatorBridge;
use act_host::tools::types::CoordinationRole as ToolRole;
use futures::FutureExt;
use serde_json::{Map, Value};

/// Whether the thread may still change its automation, and the ceiling it may give it.
pub type Authority = Arc<dyn Fn() -> Result<ExecutionPolicy, AutomationError> + Send + Sync>;

/// What one thread's runs are offered. The browser and simulator bridges are the window's own.
pub struct AppToolkit {
    pub task_id: String,
    pub requests: Arc<dyn RequestHost>,
    pub scheduler: Option<AutomationScheduler>,
    pub authority: Authority,
    pub browser: Option<Arc<dyn BrowserBridge>>,
    pub simulator: Option<Arc<dyn SimulatorBridge>>,
    /// Off when the user turned the panel's tools off in Settings.
    pub browser_tools: bool,
    pub simulator_tools: bool,
    /// The host's tools last served, by server, which the brief reads what the channel left of.
    served: Mutex<Vec<(&'static str, Vec<act_host::tools::BoundTool>)>>,
}

impl AppToolkit {
    pub fn new(task_id: String, requests: Arc<dyn RequestHost>, scheduler: Option<AutomationScheduler>, authority: Authority) -> Self {
        AppToolkit { task_id, requests, scheduler, authority, browser: None, simulator: None, browser_tools: true, simulator_tools: true, served: Mutex::new(Vec::new()) }
    }

    fn tool_run(&self, run: &RunInput) -> ToolRun {
        let bridges = RunBridges::new(self.requests.clone(), self.task_id.clone());
        let role = run.coordination_role.map(|role| match role {
            CoordinationRole::Coordinator => ToolRole::Coordinator,
            CoordinationRole::Member => ToolRole::Member,
        });
        let setup: Option<Arc<dyn Fn() + Send + Sync>> = match run.computer_use {
            ComputerUseRunConfig::SetupRequired => {
                let emit = run.hooks.emit.clone();
                Some(Arc::new(move || emit(ProviderEvent::ComputerUseSetupRequired)))
            }
            _ => None,
        };
        let automations: Option<Arc<dyn AutomationBridge>> = self.scheduler.clone().map(|scheduler| {
            let authority = self.authority.clone();
            Arc::new(ScheduledAutomations { scheduler, task_id: self.task_id.clone(), authority }) as Arc<dyn AutomationBridge>
        });
        ToolRun {
            computer_use_setup: setup,
            automations,
            findings: Some(bridges.findings()),
            threads: Some(bridges.threads()),
            coordination: role.map(|_| bridges.coordination()),
            browser: self.browser_tools.then(|| self.browser.clone().unwrap_or_else(|| bridges.browser())),
            simulator: self.simulator_tools.then(|| self.simulator.clone().unwrap_or_else(|| bridges.simulator())),
            terminal: Some(bridges.terminal()),
            coordination_role: role,
            review: matches!(run.operation, Some(RunOperation::Review { .. })),
            ..ToolRun::default()
        }
    }
}

fn grants(grants: Grants) -> ServerGrants {
    match grants {
        Grants::All => ServerGrants::All,
        Grants::ReadOnly => ServerGrants::ReadOnly,
        Grants::None => ServerGrants::None,
    }
}

/// One host tool as an engine serves it.
fn served(tool: act_host::tools::BoundTool, read_only: bool) -> BoundTool {
    let name = tool.name.clone();
    let description = tool.description.clone();
    let input_schema = Value::Object(tool.input.json_schema());
    let tool = Arc::new(tool);
    BoundTool {
        name,
        description,
        input_schema,
        read_only,
        handler: Arc::new(move |arguments: Value| {
            let tool = tool.clone();
            async move {
                let arguments = match arguments {
                    Value::Object(fields) => Some(fields),
                    Value::Null => None,
                    _ => Some(Map::new()),
                };
                let result = tool.call(arguments).await;
                if result.is_error {
                    ToolResult::error(result.text)
                } else {
                    ToolResult::text(result.text)
                }
            }
            .boxed()
        }),
    }
}

impl RunToolkit for AppToolkit {
    fn tool_sets(&self, run: &RunInput) -> Vec<ServedToolSet> {
        let tool_run = self.tool_run(run);
        let mut kept = Vec::new();
        let mut sets = Vec::new();
        for server in tool_servers() {
            let Some(tools) = (server.serve)(&tool_run) else { continue };
            let read_only = |name: &str| server.definitions.iter().any(|definition| definition.name == name && definition.read_only);
            sets.push(ServedToolSet { server: server.name.to_string(), grants: grants(server.grants), tools: tools.iter().cloned().map(|tool| { let only = read_only(&tool.name); served(tool, only) }).collect() });
            kept.push((server.name, tools));
        }
        *self.served.lock().unwrap() = kept;
        sets
    }

    fn brief(&self, offers: Offers<'_>, dialect: &BriefDialect, run: &BriefRun<'_>) -> Vec<String> {
        let dialect = act_host::tools::BriefDialect {
            native_work: dialect.native_work.to_string(),
            native_worktree: dialect.native_worktree.map(str::to_string),
            coordinator_reads: dialect.coordinator_reads.to_string(),
            own_checkout_commits: dialect.own_checkout_commits.map(str::to_string),
        };
        let run = act_host::tools::BriefRun {
            coordination_role: run.coordination_role.map(|role| match role {
                CoordinationRole::Coordinator => ToolRole::Coordinator,
                CoordinationRole::Member => ToolRole::Member,
            }),
            workspace_root: run.workspace_root.to_string(),
            project_root: run.project_root.map(str::to_string),
        };
        let served = self.served.lock().unwrap();
        tool_servers()
            .into_iter()
            .flat_map(|server| {
                let left: Option<Vec<act_host::tools::BoundTool>> = offers(server.name, None).then(|| {
                    served.iter().find(|(name, _)| *name == server.name).map(|(_, tools)| tools.iter().filter(|tool| offers(server.name, Some(&tool.name))).cloned().collect()).unwrap_or_default()
                });
                (server.brief)(&act_host::tools::server::Offers::new(left.as_deref()), &dialect, &run)
            })
            .collect()
    }
}
