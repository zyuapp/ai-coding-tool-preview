//! Runs: resolving where one happens, starting it on the engines, the commands sent while it goes,
//! and what the engines report back (src/host/run-effects.ts, src/main/run-host.ts).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use act_core::domain::engine::AgentEngine;
use act_core::domain::run::RunStatus;
use act_core::effects::{EffectWork, RunCommand, StartRunCommand};
use act_core::events::{AgentEvent, WorkspaceEvent};
use act_core::domain::platform::WorkspaceRecord;
use act_engines::coordinator::{AgentEvent as EngineEvent, CoordinatorOptions, StartRun};
use act_engines::types::ComputerUseRunConfig;
use act_engines::worker::{AgentWorker, RunCommand as EngineCommand};
use act_core::domain::session_resources::ResourceStop;
use act_engines::session_resources::SessionResourceMessage;
use act_host::automation::AutomationError;
use act_host::session_monitor::{MonitorHooks, SessionMonitor};
use act_host::workspaces::{WorkspaceKind, WorkspaceResolution};
use act_store::profile::Profile;
use serde_json::{json, Value};

use super::{convert, Host, Inbound, Sink};

/// How far each run has got, so a report after its end is dropped and a start that never reached
/// the engines still ends.
#[derive(Default)]
struct Ledger {
    /// Starts still resolving their checkout, by thread then run.
    starting: HashMap<String, String>,
    /// The last sequence each run reported, and whether it ended.
    sequences: HashMap<String, (i64, bool)>,
    /// The run each thread's session runs now, and the policy it runs under.
    sessions: HashMap<String, (String, act_core::domain::run::ExecutionPolicy)>,
}

pub struct Runs {
    worker: Arc<AgentWorker>,
    ledger: Arc<Mutex<Ledger>>,
    sink: Sink,
    monitor: SessionMonitor,
    /// Threads stopped for memory, each with the session and moment it was stopped at.
    resource_stops: Arc<Mutex<HashMap<String, String>>>,
}

const MEMORY_STOP: &str = "Stopped due to high memory usage.";

const TERMINAL: [&str; 3] = ["succeeded", "failed", "cancelled"];

impl Runs {
    pub fn new(profile: &Profile, sink: Sink) -> Runs {
        let ledger: Arc<Mutex<Ledger>> = Arc::default();
        let resource_stops: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let emit_sink = sink.clone();
        let emit_ledger = ledger.clone();
        let emit_stops = resource_stops.clone();
        let emit = Arc::new(move |event: EngineEvent| {
            let Ok(value) = serde_json::to_value(&event) else { return };
            // A session stopped for memory says nothing more until its thread starts again.
            if value.get("taskId").and_then(Value::as_str).is_some_and(|task_id| emit_stops.lock().unwrap().contains_key(task_id)) {
                return;
            }
            if !accept(&emit_ledger, &value) {
                return;
            }
            match serde_json::from_value::<AgentEvent>(value.clone()) {
                Ok(event) => emit_sink.send(Inbound::Agent(event)),
                Err(error) => eprintln!("dropped an unreadable agent event {}: {error}", value.get("type").and_then(Value::as_str).unwrap_or("?")),
            }
        });
        let generated = profile.generated_images().to_string_lossy().into_owned();
        let _guard = act_host::runtime().enter();
        act_engines::app_plugin::set_app_plugin_root(crate::resources::app_plugin_root());
        let options = CoordinatorOptions { tool_label: Some(Arc::new(|name: &str| act_core::domain::tool_call::tool_label(name))), ..CoordinatorOptions::default() };
        let worker = Arc::new(AgentWorker::new(emit, options, Some(generated)));
        let monitor = memory_monitor(profile, &worker, &ledger, &resource_stops, &sink);
        let reporting = monitor.clone();
        act_engines::session_resources::report_session_resources(move |message| match message {
            SessionResourceMessage::Process { task_id, session_id, pid } => reporting.process(&task_id, &session_id, pid),
            SessionResourceMessage::State { task_id, session_id, busy } => reporting.state(&task_id, &session_id, busy),
            SessionResourceMessage::Released { task_id, session_id } => reporting.released(&task_id, &session_id),
        });
        Runs { worker, ledger, sink, monitor, resource_stops }
    }

    pub fn start(&self, host: Arc<Host>, command: StartRunCommand) {
        self.monitor.set_enabled(command.memory_protection);
        self.resource_stops.lock().unwrap().remove(&command.task_id);
        {
            let mut ledger = self.ledger.lock().unwrap();
            if ledger.sequences.contains_key(&command.run_id) {
                return;
            }
            if let Some(superseded) = ledger.starting.insert(command.task_id.clone(), command.run_id.clone()) {
                drop(ledger);
                self.synthetic_end(&command.task_id, &superseded, "cancelled", "The run was superseded before it started.");
            }
        }
        let ledger = self.ledger.clone();
        let monitor = self.monitor.clone();
        host.clone().spawn(async move {
            let resolved = match monitor.before_start(&command.task_id).await {
                Ok(()) => resolve_start(&host, &command).await,
                Err(message) => Err(message),
            };
            let still_wanted = ledger.lock().unwrap().starting.get(&command.task_id) == Some(&command.run_id);
            if !still_wanted {
                return;
            }
            {
                let mut ledger = ledger.lock().unwrap();
                ledger.starting.remove(&command.task_id);
                if resolved.is_ok() {
                    ledger.sessions.insert(command.task_id.clone(), (command.run_id.clone(), command.policy));
                }
            }
            match resolved {
                Ok(start) => host.runs.worker.handle(EngineCommand::Start(Box::new(start))),
                Err(message) => host.runs.synthetic_end(&command.task_id, &command.run_id, "failed", &message),
            }
        });
    }

    /// Reports an end for a run the engines never took, so the thread does not wait on it forever.
    fn synthetic_end(&self, task_id: &str, run_id: &str, status: &str, message: &str) {
        let sequence = self.ledger.lock().unwrap().sequences.get(run_id).map(|(sequence, _)| *sequence).unwrap_or(0);
        let mut events = Vec::new();
        if sequence == 0 {
            events.push(json!({ "type": "run.started", "taskId": task_id, "runId": run_id, "sequence": 1 }));
        }
        events.push(json!({ "type": "run.status", "taskId": task_id, "runId": run_id, "sequence": sequence.max(1) + 1, "status": status, "message": message }));
        for value in events {
            if accept(&self.ledger, &value) {
                if let Ok(event) = serde_json::from_value::<AgentEvent>(value) {
                    self.sink.send(Inbound::Agent(event));
                }
            }
        }
    }

    pub fn command(&self, command: RunCommand) {
        let engine_command = match command {
            RunCommand::MemoryProtection { enabled } => return self.monitor.set_enabled(enabled),
            RunCommand::Cancel { task_id, run_id } => {
                let pending = {
                    let mut ledger = self.ledger.lock().unwrap();
                    if ledger.starting.get(&task_id) == Some(&run_id) {
                        ledger.starting.remove(&task_id);
                        true
                    } else {
                        false
                    }
                };
                if pending {
                    self.synthetic_end(&task_id, &run_id, "cancelled", "The run was cancelled before it started.");
                    return;
                }
                EngineCommand::Cancel { task_id, run_id }
            }
            RunCommand::AnswerQuestion { task_id, run_id, request_id, question_id, text } => EngineCommand::AnswerQuestion { task_id, run_id, request_id, question_id, text },
            RunCommand::Approval { task_id, run_id, approval_id, allow } => EngineCommand::Approval { task_id, run_id, approval_id, allow },
            RunCommand::Steer { task_id, run_id, message_id, prompt } => EngineCommand::Steer { task_id, run_id, message_id, prompt },
            RunCommand::StopProcess { task_id, process_id } => EngineCommand::StopProcess { task_id, process_id },
            RunCommand::Label { task_id, title } => EngineCommand::Label { task_id, title },
            RunCommand::Archive { task_id, channel, continuation, archived } => {
                let (Ok(channel), Ok(continuation)) = (convert(&channel), continuation.as_ref().map(convert).transpose()) else { return };
                EngineCommand::Archive { task_id, channel, continuation, archived }
            }
            RunCommand::Role { task_id, coordination_role } => {
                let Ok(coordination_role) = coordination_role.as_ref().map(convert).transpose() else { return };
                EngineCommand::Role { task_id, coordination_role }
            }
        };
        let _guard = act_host::runtime().enter();
        self.worker.handle(engine_command);
    }

    /// What a thread's live run may give its automation, refused once that run has ended.
    pub fn automation_authority(&self, task_id: &str) -> super::toolkit::Authority {
        let ledger = self.ledger.clone();
        let task_id = task_id.to_string();
        Arc::new(move || {
            let ledger = ledger.lock().unwrap();
            let Some((run_id, policy)) = ledger.sessions.get(&task_id) else { return Err(AutomationError::new("This task has no active run to change its automation.")) };
            if ledger.sequences.get(run_id).is_some_and(|(_, ended)| *ended) {
                return Err(AutomationError::new("This run is no longer authorized to change the task's automation."));
            }
            convert(policy).map_err(AutomationError::new)
        })
    }

    pub fn reload_settings(&self) {
        let _guard = act_host::runtime().enter();
        self.worker.handle(EngineCommand::ReloadSettings);
    }

    pub fn close(&self) {
        let _guard = act_host::runtime().enter();
        self.monitor.release_all();
        self.worker.close();
    }
}

/// Watches the engines' memory: a thread it stops ends its run and its session, and hears nothing
/// more from them until it starts again (src/main/run-host.ts).
fn memory_monitor(profile: &Profile, worker: &Arc<AgentWorker>, ledger: &Arc<Mutex<Ledger>>, stops: &Arc<Mutex<HashMap<String, String>>>, sink: &Sink) -> SessionMonitor {
    let workers: Arc<OnceLock<std::sync::Weak<AgentWorker>>> = Arc::default();
    let _ = workers.set(Arc::downgrade(worker));
    let terminate = move |task_id: &str, session_id: &str, idle_only: bool| {
        if let Some(worker) = workers.get().and_then(std::sync::Weak::upgrade) {
            let _guard = act_host::runtime().enter();
            worker.terminate_session(task_id, session_id, idle_only);
        }
    };
    let terminate = Arc::new(terminate);
    let stopping = terminate.clone();
    let (stop_ledger, stop_stops, stop_sink) = (ledger.clone(), stops.clone(), sink.clone());
    let (changed_stops, changed_sink) = (stops.clone(), sink.clone());
    let hooks = MonitorHooks {
        stop: Box::new(move |task_id, session_id, stop| {
            stop_stops.lock().unwrap().insert(task_id.to_string(), format!("{session_id}:{}", stop.at));
            stopped_event(&stop_sink, task_id, &stop);
            let ending = {
                let mut ledger = stop_ledger.lock().unwrap();
                ledger.starting.remove(task_id);
                ledger.sessions.get(task_id).map(|(run_id, _)| run_id.clone()).and_then(|run_id| ledger.sequences.get(&run_id).filter(|(_, ended)| !ended).map(|(sequence, _)| (run_id, *sequence)))
            };
            if let Some((run_id, sequence)) = ending {
                let value = json!({ "type": "run.status", "taskId": task_id, "runId": run_id, "sequence": sequence + 1, "status": "cancelled", "message": MEMORY_STOP });
                if accept(&stop_ledger, &value) {
                    if let Ok(event) = serde_json::from_value::<AgentEvent>(value) {
                        stop_sink.send(Inbound::Agent(event));
                    }
                }
            }
            stopping(task_id, session_id, false);
        }),
        release: Box::new(move |task_id, session_id| terminate(task_id, session_id, true)),
        changed: Box::new(move |task_id, stop| {
            if changed_stops.lock().unwrap().get(task_id).is_some_and(|stopped| stopped.ends_with(&format!(":{}", stop.at))) {
                stopped_event(&changed_sink, task_id, &stop);
            }
        }),
    };
    let _guard = act_host::runtime().enter();
    SessionMonitor::new(Some(profile.user_data.join("session-processes.json")), hooks)
}

fn stopped_event(sink: &Sink, task_id: &str, stop: &ResourceStop) {
    let event = act_core::events::ThreadEvent { task_id: task_id.into(), report: act_core::events::ThreadReport::ResourceStopped { stop: stop.clone() } };
    sink.send(Inbound::Agent(AgentEvent::Thread(event)));
}

/// Drops a run's report that comes after its end or out of order, as the run ledger does.
fn accept(ledger: &Mutex<Ledger>, value: &Value) -> bool {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    if !kind.starts_with("run.") {
        return true;
    }
    let (Some(run_id), Some(sequence)) = (value.get("runId").and_then(Value::as_str), value.get("sequence").and_then(Value::as_i64)) else { return true };
    let mut ledger = ledger.lock().unwrap();
    let entry = ledger.sequences.entry(run_id.to_string()).or_insert((0, false));
    if entry.1 || sequence <= entry.0 {
        return false;
    }
    entry.0 = sequence;
    if kind == "run.status" && value.get("status").and_then(Value::as_str).is_some_and(|status| TERMINAL.contains(&status)) {
        entry.1 = true;
    }
    true
}

/// A terminal status, for the automation that may be waiting on the run.
pub fn terminal_status(event: &AgentEvent) -> Option<(String, RunStatus)> {
    let value = serde_json::to_value(event).ok()?;
    if value.get("type")?.as_str()? != "run.status" {
        return None;
    }
    let status = value.get("status")?.as_str()?;
    if !TERMINAL.contains(&status) {
        return None;
    }
    Some((value.get("runId")?.as_str()?.to_string(), serde_json::from_value(Value::String(status.into())).ok()?))
}

async fn resolve_start(host: &Arc<Host>, command: &StartRunCommand) -> Result<StartRun, String> {
    let workspaces = &host.services.workspaces;
    let resolution = workspaces.resolve(&command.workspace_id).await.map_err(|error| error.to_string())?;
    let workspace = match resolution {
        WorkspaceResolution::Available { workspace } => workspace,
        WorkspaceResolution::Unavailable { reason, .. } => return Err(format!("Workspace is unavailable ({}).", reason.as_str())),
    };
    let project_root = match &command.project_workspace_id {
        Some(id) => match workspaces.resolve(id).await {
            Ok(WorkspaceResolution::Available { workspace: project }) if project.kind == WorkspaceKind::Project => Some(project.root.clone()),
            _ => None,
        },
        None => None,
    };
    let root = workspace.root.clone();
    let computer_use = if command.computer_use_tools == Some(false) {
        ComputerUseRunConfig::Unavailable { message: "Computer use is turned off in Settings.".into() }
    } else if command.coordination_role == Some(act_core::domain::coordination::CoordinationRole::Coordinator) {
        ComputerUseRunConfig::Unavailable { message: "A coordinator changes nothing itself.".into() }
    } else {
        host.system.computer_use_for_run().await
    };
    let mut toolkit = super::toolkit::AppToolkit::new(command.task_id.clone(), host.requests.clone(), host.automations.scheduler_now(), host.runs.automation_authority(&command.task_id));
    toolkit.browser_tools = command.browser_tools != Some(false);
    toolkit.simulator_tools = command.simulator_tools != Some(false);
    toolkit.browser = host.browser_bridge(&command.task_id);
    toolkit.simulator = host.simulator_bridge(&command.task_id);
    let toolkit: Arc<dyn act_engines::brief::RunToolkit> = Arc::new(toolkit);
    Ok(StartRun {
        channel: convert(&command.channel)?,
        task_id: command.task_id.clone(),
        title: command.title.clone(),
        run_id: command.run_id.clone(),
        prompt: command.prompt.clone(),
        project_root: project_root.filter(|project| project != &root),
        projectless: workspace.kind == WorkspaceKind::Projectless,
        workspace_root: root,
        computer_use,
        policy: convert(&command.policy)?,
        engine: convert(&command.engine)?,
        model: command.model.clone(),
        effort: convert(&command.effort)?,
        fast_mode: command.fast_mode,
        operation: command.operation.as_ref().map(convert).transpose()?,
        chrome_browser: command.chrome_browser,
        continuation: command.continuation.as_ref().map(convert).transpose()?,
        fork_continuation: command.fork_continuation.unwrap_or(false),
        fresh_session: command.fresh_session,
        unattended: command.unattended,
        coordination_role: command.coordination_role.as_ref().map(convert).transpose()?,
        toolkit: Some(toolkit),
    })
}

fn record(workspace: &act_host::workspaces::WorkspaceRecord) -> Result<WorkspaceRecord, String> {
    convert(workspace)
}

/// Turns a resolution request into the event that answers it (src/host/resolve-run-workspace.ts).
pub async fn resolve_run_workspace(host: &Arc<Host>, work: EffectWork) {
    let EffectWork::ResolveRunWorkspace { pending_id, picker, workspace, root, create_worktree, create_branch, checkout } = work else { return };
    let answer: Result<Value, String> = async {
        if let Some(branch) = &create_branch {
            let path = host.services.workspaces.available_root(&branch.workspace_id).await.map_err(|error| format!("Could not create the branch {}: {error}", branch.branch))?;
            act_host::git::create_branch(&path, &branch.branch).await.map_err(|error| format!("Could not create the branch {}: {error}", branch.branch))?;
        }
        if let Some(branch) = &checkout {
            let path = host.services.workspaces.available_root(&branch.workspace_id).await.map_err(|error| format!("Could not check out {}: {error}", branch.branch))?;
            act_host::git::checkout_branch(&path, &branch.branch).await.map_err(|error| format!("Could not check out {}: {error}", branch.branch))?;
        }
        if let Some(making) = &create_worktree {
            let request = act_host::worktrees::CreateWorktreeRequest { project_root: making.project_root.clone(), carry_changes: making.carry_changes, branch: making.branch.clone() };
            let created = host.services.worktrees.create(&request).await.map_err(|error| format!("Could not create the worktree: {error}"))?;
            let worktree = serde_json::to_value(&created).map_err(|error| error.to_string())?;
            return Ok(json!({ "type": "run.resolved", "pendingId": pending_id, "workspace": { "id": worktree["workspaceId"], "kind": "worktree", "root": worktree["root"] }, "worktree": worktree }));
        }
        if let Some(workspace) = &workspace {
            return Ok(json!({ "type": "run.resolved", "pendingId": pending_id, "workspace": workspace }));
        }
        if !picker {
            let projectless = host.services.workspaces.projectless().await.map_err(|error| error.to_string())?;
            return Ok(json!({ "type": "run.resolved", "pendingId": pending_id, "workspace": record(&projectless)? }));
        }
        let Some(root) = root else { return Err(act_core::reducer::errors::REOPEN_PROJECT_ERROR.to_string()) };
        let registered = host.services.workspaces.register_project(std::path::Path::new(&root)).await.map_err(|error| error.to_string())?;
        Ok(json!({ "type": "run.resolved", "pendingId": pending_id, "workspace": record(&registered)? }))
    }
    .await;
    match answer {
        Ok(event) => host.sink.json(event),
        Err(message) => host.sink.event(WorkspaceEvent::RunUnresolved { pending_id, message }),
    }
}

/// A title costs a model turn, so it lands on its own rather than holding back the send that asked for it.
pub async fn suggest_title(sink: &Sink, task_id: String, engine: AgentEngine, model: String, text: String, attachments: Vec<String>) {
    let Ok(engine) = convert(&engine) else { return };
    if let Some(title) = act_engines::engines::suggest_title(engine, &text, &attachments, Some(&model)).await {
        sink.event(WorkspaceEvent::TitleSuggested { task_id, title });
    }
}

pub async fn subagent_metadata(sink: &Sink, task_id: String, subagent_id: String, engine: AgentEngine, session_id: Option<String>) {
    let Ok(engine) = convert(&engine) else { return };
    let Some(metadata) = act_engines::engines::subagent_metadata(engine, &subagent_id, session_id.as_deref()).await else { return };
    let Ok(Value::Object(mut fields)) = serde_json::to_value(&metadata) else { return };
    if !fields.contains_key("model") && !fields.contains_key("effort") {
        return;
    }
    fields.insert("type".into(), json!("subagent.metadata"));
    fields.insert("taskId".into(), json!(task_id));
    fields.insert("id".into(), json!(subagent_id));
    sink.json(json!({ "type": "thread.event", "event": Value::Object(fields) }));
}
