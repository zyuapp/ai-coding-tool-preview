//! The rules a new thread a run asks for starts by: what it runs on, whom it works under, and where.

use act_core::commands::TaskSend;
use act_core::domain::coordination::{coordinator_of, is_coordinator, remote_lead_of, thread_model_of, ProposalComputer, ProposalPlace, ProposalRequest, RemoteLead, ThreadProposal};
use act_core::domain::engine::{default_effort_for, default_model_in, effort_for_model, engine_has_model, engine_label, model_has_effort, model_takes_effort, resolve_model, thread_model, unknown_model, AgentEngine};
use act_core::domain::project::{find_project, folder_name, matching_projects, project_name, Project};
use act_core::domain::thread::{clamp_title, Thread};
use act_core::domain::thread_role::ThreadRole;
use act_core::state::PairedComputer;
use act_core::workspace::location::{named_place, project_for};
use act_core::WorkspaceState;

pub const REMOTE_LED_HERE: &str = "This thread works under a coordinator on another computer, so the threads it starts run on this computer.";
pub const COORDINATOR_WORKTREE_START: &str = "A coordinator stays in its project's checkout, since it changes nothing itself. Start it without worktree or worktreeId; the threads it starts take worktree: true.";
pub const BRIEF_REQUIRED: &str = "A coordinator hands every thread it starts a brief: pass intent (the user's own words), doneWhen, and delivers.";

/// A send ready to start here, or one for the paired computer the thread belongs on, naming the
/// coordinator it would work under there.
#[derive(Debug, Clone)]
pub enum NewThreadStart<'a> {
    Here(Box<TaskSend>),
    Elsewhere { computer: &'a PairedComputer, command: Box<TaskSend>, lead: Option<&'a Thread> },
}

fn holder_of(state: &WorkspaceState, holds: impl Fn(&WorkspaceState) -> bool) -> Option<&PairedComputer> {
    if holds(state) {
        return None;
    }
    state.computers.paired.iter().find(|computer| computer.state.as_deref().is_some_and(&holds))
}

/// The computer holding the checkout or project a new thread is named into by id: `Some(None)` for
/// this computer, None when no id names one.
pub fn computer_of_place<'a>(state: &'a WorkspaceState, project: Option<&str>, worktree_id: Option<&str>) -> Option<Option<&'a PairedComputer>> {
    if let Some(worktree_id) = worktree_id {
        if state.worktrees.iter().any(|worktree| worktree.id == worktree_id) {
            return Some(None);
        }
        if let Some(holder) = holder_of(state, |held| held.worktrees.iter().any(|worktree| worktree.id == worktree_id)) {
            return Some(Some(holder));
        }
    }
    if let Some(project) = project {
        if state.projects.iter().any(|item| item.id == project) {
            return Some(None);
        }
        if let Some(holder) = holder_of(state, |held| held.projects.iter().any(|item| item.id == project)) {
            return Some(Some(holder));
        }
    }
    None
}

/// The project on a paired computer a new thread starts in: the one named, else the one at the asking
/// thread's project's path, else the one with its folder's name.
pub fn project_on_computer<'a>(computer: &'a PairedComputer, named: Option<&str>, own: Option<&Project>) -> Result<&'a Project, String> {
    let projects: &[Project] = computer.state.as_deref().map(|state| state.projects.as_slice()).unwrap_or(&[]);
    let name = &computer.link.name;
    if let Some(named) = named {
        return find_project(projects, named).map_err(|error| format!("On {name}: {error}"));
    }
    let Some(own) = own else { return Err(format!("Name the project on {name} to start the thread in.")) };
    let at_root = matching_projects(projects, &own.root);
    let matches = if at_root.is_empty() { matching_projects(projects, &folder_name(&own.root)) } else { at_root };
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    Err(format!("{} project on {name} matches {}. Name the project to start the thread in.", if matches.is_empty() { "No" } else { "More than one" }, project_name(own)))
}

/// What a thread `caller` asks for starts on. A thread that will work under a coordinator runs on what
/// the coordinator's threads run on; any other inherits the caller's own model and effort. `asked` is
/// the computer the request named: `Some(None)` for this one, None when it named none.
pub fn new_thread_send<'a>(state: &'a WorkspaceState, caller: &'a Thread, command: &TaskSend, asked: Option<Option<&'a PairedComputer>>) -> Result<NewThreadStart<'a>, String> {
    let lead = if is_coordinator(Some(caller)) { Some(caller) } else { coordinator_of(&state.threads, Some(caller)) };
    let remote = if lead.is_some() { None } else { remote_lead_of(Some(caller)) };
    let catalog = state.catalog();
    let inherited = match lead {
        Some(lead) => thread_model_of(&catalog, lead).map(|choice| (choice.engine, Some(choice.model), choice.effort)),
        None => Some((Some(caller.engine), thread_model(&catalog, caller.engine, caller.model.as_deref()), caller.effort)),
    };
    let as_engine = match &command.model {
        None => command.engine,
        Some(model) => AgentEngine::parse(model),
    };
    let named = match (&command.model, as_engine) {
        (Some(model), None) => Some(resolve_model(&catalog, model, &state.favorite_models, command.engine)?),
        _ => None,
    };
    let choice: Option<(Option<AgentEngine>, Option<String>)> = match as_engine {
        Some(engine) => default_model_in(&catalog, engine).map(|model| (Some(engine), Some(model))),
        None => match named {
            Some(named) => Some((Some(named.engine), Some(named.model))),
            None => inherited.as_ref().map(|(engine, model, _)| (*engine, model.clone())),
        },
    };
    let Some((engine, Some(model))) = choice else {
        return Err(match as_engine {
            Some(engine) => format!("{} has no models to start a thread on. Connect a provider first.", engine_label(engine)),
            None => format!("Pass a model: {} has yet to name the one this thread runs on.", engine_label(lead.unwrap_or(caller).engine)),
        });
    };
    let engine = match engine {
        Some(engine) if engine_has_model(&catalog, engine, &model) => engine,
        _ => return Err(unknown_model(&model, None)),
    };
    let role = (command.role == Some(ThreadRole::Coordinator)).then_some(act_core::domain::coordination::CoordinationRole::Coordinator);
    if role.is_some() && (command.worktree == Some(true) || command.worktree_id.is_some()) {
        return Err(COORDINATOR_WORKTREE_START.into());
    }
    if let Some(effort) = command.effort {
        if !model_has_effort(&catalog, engine, &model, effort, role) {
            let effort_name = super::projection::wire_name(&effort);
            return Err(format!("The {model} model does not support {effort_name} effort{}.", if role.is_some() { " for a coordinator" } else { "" }));
        }
    }
    let inherited_effort = inherited.as_ref().and_then(|(_, _, effort)| *effort).unwrap_or_else(|| default_effort_for(engine));
    let effort = command.effort.unwrap_or_else(|| effort_for_model(&catalog, engine, &model, inherited_effort, role));
    let mut chosen = command.clone();
    chosen.brief = if lead.is_some_and(|lead| lead.id == caller.id) { command.brief.clone() } else { None };
    chosen.engine = Some(engine);
    chosen.model = Some(model.clone());
    chosen.effort = model_takes_effort(&catalog, engine, &model).then_some(effort);
    if lead.is_some_and(|lead| lead.id == caller.id) && chosen.brief.is_none() {
        return Err(BRIEF_REQUIRED.into());
    }

    let placed_on = computer_of_place(state, chosen.project.as_deref(), chosen.worktree_id.as_deref()).flatten();
    if let (Some(placed), Some(asked)) = (placed_on, asked) {
        if asked.map(|computer| &computer.link.id) != Some(&placed.link.id) {
            return Err(format!("That project or worktree is on {0}. Pass computer \"{0}\" to start the thread there.", placed.link.name));
        }
    }
    let elsewhere = match asked {
        Some(asked) => asked,
        None => placed_on,
    };
    if elsewhere.is_some() && remote.is_some() {
        return Err(REMOTE_LED_HERE.into());
    }
    if let Some(computer) = elsewhere {
        return Ok(NewThreadStart::Elsewhere { computer, command: Box::new(chosen), lead });
    }
    if chosen.project.is_none() {
        chosen.project = caller.project_id.clone();
    }
    if role.is_none() && chosen.worktree.is_none() && chosen.worktree_id.is_none() {
        chosen.worktree_id = caller.worktree_id.clone();
    }
    if let Some(lead) = lead {
        chosen.coordinator_id = Some(lead.id.clone());
    } else if let Some(remote) = remote {
        chosen.lead = Some(RemoteLead { thread_id: remote.thread_id.clone(), title: remote.title.clone(), computer: remote.computer.clone(), proposal_id: None });
    }
    chosen.from = Some(caller.id.clone());
    Ok(NewThreadStart::Here(Box::new(chosen)))
}

/// A thread a coordinator, or a thread under it, asks the user for: checked now as its start will
/// be, and placed where that start resolves to.
pub fn thread_proposal(state: &WorkspaceState, caller: &Thread, request: &ProposalRequest, title: Option<&str>, at: i64, asked: Option<Option<&PairedComputer>>) -> Result<ThreadProposal, String> {
    let command = TaskSend {
        text: Some(request.prompt.clone()),
        project: request.project.clone(),
        worktree: request.worktree,
        worktree_id: request.worktree_id.clone(),
        model: request.model.clone(),
        engine: request.engine,
        effort: request.effort,
        role: request.role,
        brief: request.brief.clone(),
        ..TaskSend::default()
    };
    let start = new_thread_send(state, caller, &command, asked)?;
    let (place, brief, from) = proposal_place(state, caller, &start)?;
    let started = match &start {
        NewThreadStart::Here(command) | NewThreadStart::Elsewhere { command, .. } => command,
    };
    let (engine, model) = match (&request.model, started.engine, &started.model) {
        (Some(model), Some(engine), Some(started_model)) if AgentEngine::parse(model).is_none() => (Some(engine.as_str().to_string()), Some(started_model.clone())),
        (Some(model), _, _) => (None, Some(model.clone())),
        (None, _, _) => (request.engine.map(|engine| engine.as_str().to_string()), None),
    };
    let trimmed = title.map(str::trim).filter(|title| !title.is_empty());
    let mut proposal: ThreadProposal = serde_json::from_value(serde_json::json!({ "id": uuid::Uuid::new_v4().to_string(), "title": "", "prompt": request.prompt, "proposedAt": at })).map_err(|error| error.to_string())?;
    proposal.title = trimmed.map(str::to_string).unwrap_or_else(|| clamp_title(&request.prompt));
    proposal.named = trimmed.is_some();
    proposal.place = place;
    proposal.brief = brief;
    proposal.from = from;
    proposal.engine = engine;
    proposal.model = model;
    proposal.effort = request.effort;
    proposal.role = request.role;
    Ok(proposal)
}

type Placed = (ProposalPlace, Option<act_core::domain::coordination::ThreadBrief>, Option<String>);

fn proposal_place(state: &WorkspaceState, caller: &Thread, start: &NewThreadStart) -> Result<Placed, String> {
    match start {
        NewThreadStart::Elsewhere { computer, command, lead } => {
            let Some(lead) = lead else { return Err("Only a coordinator and the threads under it ask for threads on another computer.".into()) };
            if command.worktree_id.is_some() {
                return Err(format!("A worktree already on {} cannot be named. Pass worktree: true for a new one there.", computer.link.name));
            }
            let project = project_on_computer(computer, command.project.as_deref(), project_for(state, Some(caller)))?;
            let place = ProposalPlace { computer: Some(ProposalComputer { id: computer.link.id.clone(), name: computer.link.name.clone() }), project: Some(project.id.clone()), worktree: Some(command.worktree == Some(true)), worktree_id: None };
            Ok((place, command.brief.clone(), (caller.id != lead.id).then(|| caller.id.clone())))
        }
        NewThreadStart::Here(command) => {
            if command.brief.is_none() {
                return Err(BRIEF_REQUIRED.into());
            }
            let (named_project, worktree) = named_place(state, command.project.as_deref(), command.worktree_id.as_deref())?;
            let project = named_project.map(|project| project.id.clone()).or_else(|| worktree.map(|worktree| worktree.project_id.clone()));
            let place = match worktree {
                Some(worktree) => ProposalPlace { project, worktree_id: Some(worktree.id.clone()), ..ProposalPlace::default() },
                None => ProposalPlace { project, worktree: Some(command.worktree == Some(true)), ..ProposalPlace::default() },
            };
            Ok((place, command.brief.clone(), None))
        }
    }
}
