//! The rules a new thread starts by, whoever asks for it: what it runs on, whom it works under, and
//! where it starts.

use crate::commands::TaskSend;
use crate::domain::coordination::{coordinator_of, is_coordinator, remote_lead_of, thread_model_of, ProposalComputer, ProposalPlace, ProposalRequest, RemoteLead, ThreadProposal};
use crate::domain::engine::{default_effort_for, default_model_in, effort_for_model, engine_has_model, engine_label, model_has_effort, model_takes_effort, resolve_model, thread_model, unknown_model, AgentEngine};
use crate::domain::thread::{clamp_title, Thread};
use crate::domain::thread_role::ThreadRole;
use crate::reducer::computers::{computer_of_place, project_on_computer};
use crate::state::{PairedComputer, WorkspaceState};
use crate::util::random_id;
use crate::workspace::location::{named_place, project_for};

pub const REMOTE_LED_HERE: &str = "This thread works under a coordinator on another computer, so the threads it starts run on this computer.";
pub const COORDINATOR_WORKTREE_START: &str = "A coordinator stays in its project's checkout, since it changes nothing itself. Start it without worktree or worktreeId; the threads it starts take worktree: true.";
pub const BRIEF_REQUIRED: &str = "A coordinator hands every thread it starts a brief: pass intent (the user's own words), doneWhen, and delivers.";

/// A send ready to start here, or one for the paired computer the thread belongs on.
#[derive(Clone, Debug)]
pub enum NewThreadStart<'a> {
    Here(TaskSend),
    Elsewhere { computer: &'a PairedComputer, command: TaskSend, lead: Option<&'a Thread> },
}

/// What a thread `caller` asks for starts on, and where. `asked` is the computer the request named:
/// Some(None) for this one, None when it named none.
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
    let Some(engine) = engine.filter(|engine| engine_has_model(&catalog, *engine, &model)) else { return Err(unknown_model(&model, None)) };
    let role = (command.role == Some(ThreadRole::Coordinator)).then_some(crate::domain::coordination::CoordinationRole::Coordinator);
    if role.is_some() && (command.worktree == Some(true) || command.worktree_id.is_some()) {
        return Err(COORDINATOR_WORKTREE_START.into());
    }
    if let Some(effort) = command.effort {
        if !model_has_effort(&catalog, engine, &model, effort, role) {
            let effort_name = serde_json::to_value(effort).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default();
            return Err(format!("The {model} model does not support {effort_name} effort{}.", if role.is_some() { " for a coordinator" } else { "" }));
        }
    }
    let effort = command.effort.unwrap_or_else(|| effort_for_model(&catalog, engine, &model, inherited.as_ref().and_then(|(_, _, effort)| *effort).unwrap_or(default_effort_for(engine)), role));
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
        if asked.map(|computer| computer.link.id.as_str()) != Some(placed.link.id.as_str()) {
            return Err(format!("That project or worktree is on {}. Pass computer \"{}\" to start the thread there.", placed.link.name, placed.link.name));
        }
    }
    let elsewhere = asked.flatten().or(placed_on);
    if elsewhere.is_some() && remote.is_some() {
        return Err(REMOTE_LED_HERE.into());
    }
    if let Some(computer) = elsewhere {
        return Ok(NewThreadStart::Elsewhere { computer, command: chosen, lead });
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
    Ok(NewThreadStart::Here(chosen))
}

/// A thread `caller` asks its coordinator's user for, checked now by the rules its start will be.
pub fn thread_proposal(state: &WorkspaceState, caller: &Thread, request: &ProposalRequest, at: i64, asked: Option<Option<&PairedComputer>>) -> Result<ThreadProposal, String> {
    let command = TaskSend {
        text: Some(request.prompt.clone()),
        brief: request.brief.clone(),
        project: request.project.clone(),
        worktree: request.worktree,
        worktree_id: request.worktree_id.clone(),
        engine: request.engine,
        model: request.model.clone(),
        effort: request.effort,
        role: request.role,
        ..TaskSend::default()
    };
    let start = new_thread_send(state, caller, &command, asked)?;
    let (place, brief, from) = proposal_place(state, caller, &start)?;
    let started = match &start {
        NewThreadStart::Here(command) | NewThreadStart::Elsewhere { command, .. } => command,
    };
    let title = request.title.trim();
    let (engine, model) = match &request.model {
        Some(model) if AgentEngine::parse(model).is_none() && started.engine.is_some() && started.model.is_some() => (started.engine.map(|engine| engine.as_str().to_string()), started.model.clone()),
        Some(model) => (None, Some(model.clone())),
        None => (request.engine.map(|engine| engine.as_str().to_string()), None),
    };
    Ok(ThreadProposal {
        id: random_id(),
        title: if title.is_empty() { clamp_title(&request.prompt) } else { title.to_string() },
        named: !title.is_empty(),
        prompt: request.prompt.clone(),
        brief,
        place,
        engine,
        model,
        effort: request.effort,
        role: request.role,
        from,
        proposed_at: at,
        error: None,
        carried: false,
    })
}

type Placed = (ProposalPlace, Option<crate::domain::coordination::ThreadBrief>, Option<String>);

fn proposal_place(state: &WorkspaceState, caller: &Thread, start: &NewThreadStart) -> Result<Placed, String> {
    match start {
        NewThreadStart::Elsewhere { computer, command, lead } => {
            let Some(lead) = lead else { return Err("Only a coordinator and the threads under it ask for threads on another computer.".into()) };
            if command.worktree_id.is_some() {
                return Err(format!("A worktree already on {} cannot be named. Pass worktree: true for a new one there.", computer.link.name));
            }
            let found = project_on_computer(computer, command.project.as_deref(), project_for(state, Some(caller)))?;
            let place = ProposalPlace { project: Some(found.id), worktree: Some(command.worktree == Some(true)), worktree_id: None, computer: Some(ProposalComputer { id: computer.link.id.clone(), name: computer.link.name.clone() }) };
            Ok((place, command.brief.clone(), (caller.id != lead.id).then(|| caller.id.clone())))
        }
        NewThreadStart::Here(command) => {
            let Some(brief) = command.brief.clone() else { return Err(BRIEF_REQUIRED.into()) };
            let (project, worktree) = named_place(state, command.project.as_deref(), command.worktree_id.as_deref())?;
            let project = project.map(|project| project.id.clone()).or_else(|| worktree.map(|worktree| worktree.project_id.clone()));
            let place = match worktree {
                Some(worktree) => ProposalPlace { project, worktree: None, worktree_id: Some(worktree.id.clone()), computer: None },
                None => ProposalPlace { project, worktree: Some(command.worktree == Some(true)), worktree_id: None, computer: None },
            };
            Ok((place, Some(brief), None))
        }
    }
}

/// The send a suggestion asks for, as the lead would have started it, in the place it was suggested for.
pub fn proposal_send(proposal: &ThreadProposal) -> Result<TaskSend, String> {
    let engine = match &proposal.engine {
        None => None,
        Some(name) => Some(AgentEngine::parse(name).ok_or_else(|| format!("This version can't start threads on {name}. Update to start it here."))?),
    };
    let place = &proposal.place;
    Ok(TaskSend {
        text: Some(proposal.prompt.clone()),
        title: proposal.named.then(|| proposal.title.clone()),
        brief: proposal.brief.clone(),
        project: place.project.clone(),
        worktree_id: place.worktree_id.clone(),
        worktree: if place.worktree_id.is_some() { None } else { place.worktree },
        model: proposal.model.clone(),
        engine,
        effort: proposal.effort,
        role: proposal.role,
        ..TaskSend::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::coordination::{Delivery, ThreadBrief, ThreadModelChoice};
    use crate::domain::engine::AgentEngine;
    use crate::domain::project::Project;
    use crate::domain::run::{AgentEffort, ExecutionPolicy};

    fn task(id: &str, project: &str) -> Thread {
        let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        thread.project_id = Some(project.into());
        thread
    }

    fn coordinated() -> WorkspaceState {
        let mut state = WorkspaceState::default();
        state.projects.push(Project { id: "p".into(), root: "/repo".into(), name: None, workspace_id: Some("ws".into()), sort_index: None, unlisted: false });
        let mut lead = task("lead", "p");
        lead.role = Some(ThreadRole::Coordinator);
        lead.model = Some("sonnet".into());
        lead.effort = Some(AgentEffort::Low);
        lead.thread_model = Some(ThreadModelChoice { engine: None, model: "opus".into(), effort: Some(AgentEffort::High) });
        lead.worktree_id = Some("wt-lead".into());
        let mut worker = task("worker", "p");
        worker.parent_id = Some("lead".into());
        let mut solo = task("solo", "p");
        solo.model = Some("sonnet".into());
        solo.effort = Some(AgentEffort::Max);
        solo.worktree_id = Some("wt-solo".into());
        state.threads = vec![lead.into(), worker.into(), solo.into()];
        state
    }

    fn here(start: Result<NewThreadStart, String>) -> TaskSend {
        match start {
            Ok(NewThreadStart::Here(command)) => command,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_thread_under_a_coordinator_works_under_it_from_its_brief_on_what_its_threads_run_on() {
        let state = coordinated();
        let send = TaskSend { text: Some("Fix it".into()), ..TaskSend::default() };
        let brief = ThreadBrief { intent: "fix the login test".into(), done_when: "it passes".into(), delivers: Delivery::Commit };
        assert_eq!(new_thread_send(&state, state.thread("lead").unwrap(), &send, None).err().as_deref(), Some(BRIEF_REQUIRED));
        let started = here(new_thread_send(&state, state.thread("lead").unwrap(), &TaskSend { brief: Some(brief.clone()), ..send.clone() }, None));
        assert_eq!((started.engine, started.model.as_deref(), started.effort), (Some(AgentEngine::Claude), Some("opus"), Some(AgentEffort::High)));
        assert_eq!((started.project.as_deref(), started.worktree_id.as_deref(), started.coordinator_id.as_deref(), started.from.as_deref()), (Some("p"), Some("wt-lead"), Some("lead"), Some("lead")));
        let joined = here(new_thread_send(&state, state.thread("worker").unwrap(), &TaskSend { brief: Some(brief), ..send.clone() }, None));
        assert_eq!(joined.coordinator_id.as_deref(), Some("lead"));
        assert_eq!(joined.model.as_deref(), Some("opus"));
        assert!(joined.brief.is_none(), "a brief is the coordinator's to hand out");
        let solo = here(new_thread_send(&state, state.thread("solo").unwrap(), &send, None));
        assert_eq!((solo.model.as_deref(), solo.effort, solo.worktree_id.as_deref(), solo.coordinator_id.as_deref()), (Some("sonnet"), Some(AgentEffort::Max), Some("wt-solo"), None));
    }
}
