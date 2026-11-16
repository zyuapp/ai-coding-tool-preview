//! A coordinator's suggestions, and the threads it asked for on other computers: filed, started or
//! allowed, turned down, and settled.

use std::collections::HashSet;

use crate::commands::{AppCommand, ProposalStartPlace, TaskSend};
use crate::domain::coordination::{coordinator_of, is_coordinator, with_coordination_note, with_proposal_carried, with_proposal_error, with_proposals, without_proposals, RemoteLead, ThreadProposal};
use crate::domain::message_origin::RemoteSender;
use crate::effects::{Effect, EffectWork, ForwardProposal};
use crate::events::{CommandResult, WorkspaceEvent, WorkspaceInput};
use crate::reducer::computers::{led_start_refusal, send_for};
use crate::reducer::sending::{memory_prompt, reduce_sending, SendSaid};
use crate::reducer::Step;
use crate::domain::thread::Thread;
use crate::state::WorkspaceState;
use crate::util::now;
use crate::validation::supports_computer_command;
use crate::workspace::coordination_messages::request_headline;
use crate::workspace::notices::{announced, coordination_note, NoticeReason};
use crate::workspace::outstanding_stops::{caught_start_computer, catches_start, with_start_landed, without_caught_start};
use crate::workspace::thread_starts::{new_thread_send, proposal_send, NewThreadStart};

const ASKER_GONE: &str = "The thread that asked for this no longer works under this coordinator.";

pub fn reduce_proposals(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Event(WorkspaceEvent::CoordinationProposed { task_id, proposals }) => {
            let Some(lead) = state.thread(task_id).filter(|lead| is_coordinator(Some(lead))) else { return Step::none() };
            let held: HashSet<String> = lead.proposals.iter().flatten().map(|proposal| proposal.id.clone()).collect();
            let mut proposed = lead.clone();
            with_proposals(&mut proposed, proposals);
            if proposed.proposals == lead.proposals {
                return Step::none();
            }
            let asked: Vec<ThreadProposal> = proposed.proposals.iter().flatten().filter(|proposal| !held.contains(&proposal.id) && proposal.from.is_some()).cloned().collect();
            if let Some(lead) = state.thread_mut(task_id) {
                lead.proposals = proposed.proposals;
            }
            let Some(lead) = state.thread(task_id) else { return Step::none() };
            let effects = asked.iter().flat_map(|proposal| match &proposal.place.computer {
                Some(computer) => announced(state, lead, &request_headline(proposal, &computer.name), NoticeReason::Request),
                None => Vec::new(),
            }).collect();
            Step::effects(effects)
        }
        WorkspaceInput::Command(AppCommand::ProposalDismiss { task_id, proposal_id, proposal_ids }) => {
            let Some(lead) = state.thread(task_id).filter(|lead| is_coordinator(Some(lead))) else { return Step::none() };
            let declined = targeted_proposals(state, lead, proposal_id.as_deref(), proposal_ids.as_deref());
            let Some(first) = declined.first() else { return Step::none() };
            let titles: Vec<String> = declined.iter().map(|proposal| format!("\"{}\"", proposal.title)).collect();
            let text = match &first.place.computer {
                Some(denied) if first.carried => format!("The user gave up on starting {} on {}. If it started there after all, you will hear from it.", titles[0], denied.name),
                Some(denied) => format!("The user did not allow {} to run on {}.", titles[0], denied.name),
                None if titles.len() == 1 => format!("The user declined the suggested thread {}.", titles[0]),
                None => format!("The user declined the suggested threads {} and {}.", titles[..titles.len() - 1].join(", "), titles[titles.len() - 1]),
            };
            let note = coordination_note(task_id, text, now(), false, None, true);
            let ids: HashSet<String> = declined.iter().map(|proposal| proposal.id.clone()).collect();
            if let Some(lead) = state.thread_mut(task_id) {
                without_proposals(lead, &ids);
                with_coordination_note(lead, note);
            }
            Step::none()
        }
        WorkspaceInput::Command(AppCommand::ProposalStart { task_id, proposal_id, proposal_ids, place }) => {
            let Some(lead) = state.thread(task_id).filter(|lead| is_coordinator(Some(lead))) else { return Step::none() };
            let waiting: Vec<ThreadProposal> = targeted_proposals(state, lead, proposal_id.as_deref(), proposal_ids.as_deref());
            let mut step = Step::none();
            for proposal in waiting {
                let started = if proposal.place.computer.is_some() { start_elsewhere(state, task_id, &proposal.id, place.as_ref()) } else { start_proposal(state, task_id, &proposal.id) };
                let refused = started.result.clone().filter(|result| !result.is_ok());
                let kept = step.result.take();
                step = step.then(Step { result: None, ..started });
                step.result = refused.or(kept);
            }
            step
        }
        WorkspaceInput::Event(WorkspaceEvent::ProposalSettled { task_id, proposal_id, thread_id, message, refused }) => proposal_settled(state, task_id, proposal_id, thread_id.as_deref(), message.as_deref(), *refused),
        _ => Step::none(),
    }
}

/// A thread a coordinator here allowed on another computer starting there, or failing to.
fn proposal_settled(state: &mut WorkspaceState, lead_id: &str, proposal_id: &str, thread_id: Option<&str>, message: Option<&str>, refused: bool) -> Step {
    let lead = state.thread(lead_id);
    if let Some(thread_id) = thread_id {
        let place = lead.and_then(|lead| lead.proposals.iter().flatten().find(|item| item.id == proposal_id)).and_then(|proposal| proposal.place.computer.as_ref().map(|computer| computer.id.clone())).or_else(|| caught_start_computer(lead, proposal_id));
        let computer = place.and_then(|id| state.computers.paired.iter().find(|item| item.link.id == id)).map(|computer| (computer.link.id.clone(), computer.link.capabilities.clone()));
        return landed_step(state, lead_id, proposal_id, thread_id, None, computer.as_ref().map(|(id, capabilities)| (id.as_str(), capabilities.as_deref())));
    }
    if lead.is_none() || !catches_start(lead, proposal_id) {
        return proposal_failure(state, lead_id, proposal_id, Some(message.unwrap_or("The thread did not start.").to_string()));
    }
    if refused {
        if let Some(lead) = state.thread_mut(lead_id) {
            without_caught_start(lead, proposal_id);
        }
    }
    state.starting_proposals.retain(|id| id != proposal_id);
    Step::none()
}

/// A thread a coordinator here allowed on a computer having landed there, as heard first.
pub fn landed_step(state: &mut WorkspaceState, lead_id: &str, proposal_id: &str, thread_id: &str, run_ended_at: Option<i64>, computer: Option<(&str, Option<&[String]>)>) -> Step {
    let lead = state.thread(lead_id);
    let carded = lead.is_some_and(|lead| lead.proposals.iter().flatten().any(|item| item.id == proposal_id));
    let caught = catches_start(lead, proposal_id);
    if !carded && !caught && !state.starting_proposals.iter().any(|id| id == proposal_id) {
        return Step::none();
    }
    state.starting_proposals.retain(|id| id != proposal_id);
    let Some(lead) = state.thread_mut(lead_id) else { return Step::none() };
    if carded {
        without_proposals(lead, &[proposal_id.to_string()].into());
    }
    if !caught {
        return Step::none();
    }
    if run_ended_at.is_some() {
        without_caught_start(lead, proposal_id);
        return Step::none();
    }
    with_start_landed(lead, proposal_id, thread_id);
    let cancel = AppCommand::RunCancel { task_id: Some(thread_id.into()) };
    match computer {
        Some((id, capabilities)) if supports_computer_command(capabilities, &cancel) => Step::effects(vec![crate::reducer::computers::forward(id, vec![cancel.into()])]),
        _ => Step::none(),
    }
}

fn failure(state: &mut WorkspaceState, lead_id: &str, proposal_id: &str, message: String) -> Step {
    proposal_failure(state, lead_id, proposal_id, Some(message.clone())).then(Step::result(CommandResult::failed(message)))
}

/// One suggestion starting by the same rules as every start, under the coordinator that suggested it.
fn start_proposal(state: &mut WorkspaceState, lead_id: &str, proposal_id: &str) -> Step {
    proposal_failure(state, lead_id, proposal_id, None);
    let Some(lead) = state.thread(lead_id).cloned() else { return Step::none() };
    let Some(proposal) = lead.proposals.iter().flatten().find(|item| item.id == proposal_id).cloned() else { return Step::none() };
    let send = match proposal_send(&proposal) {
        Ok(send) => send,
        Err(message) => return failure(state, lead_id, proposal_id, message),
    };
    let command = match new_thread_send(state, &lead, &send, Some(None)) {
        Ok(NewThreadStart::Here(command)) => command,
        Ok(NewThreadStart::Elsewhere { .. }) => return failure(state, lead_id, proposal_id, "Its project is no longer on this computer.".into()),
        Err(message) => return failure(state, lead_id, proposal_id, message),
    };
    let held_error = (state.action_error.clone(), state.action_error_page);
    let sent = reduce_sending(state, &AppCommand::TaskSend(Box::new(command)), Some(SendSaid { proposal_id: Some(proposal_id.into()), ..SendSaid::default() }));
    if sent.failed() {
        let message = sent.refusal.clone().or_else(|| sent.result.as_ref().and_then(|result| result.message().map(String::from))).unwrap_or_default();
        state.action_error = held_error.0;
        state.action_error_page = held_error.1;
        let kept = Step { refusal: None, result: None, ..sent };
        return kept.then(failure(state, lead_id, proposal_id, message));
    }
    let going = state.pending_runs.values().any(|pending| pending.coordination.as_ref().and_then(|send| send.proposal_id.as_deref()) == Some(proposal_id));
    if going {
        state.starting_proposals.push(proposal_id.into());
    }
    sent
}

/// A thread asked for on a paired computer, allowed by the user in the project there they picked.
fn start_elsewhere(state: &mut WorkspaceState, lead_id: &str, proposal_id: &str, chosen: Option<&ProposalStartPlace>) -> Step {
    proposal_failure(state, lead_id, proposal_id, None);
    let Some(lead) = state.thread(lead_id).cloned() else { return Step::none() };
    let Some(proposal) = lead.proposals.iter().flatten().find(|item| item.id == proposal_id).cloned() else { return Step::none() };
    let Some(place_computer) = proposal.place.computer.clone() else { return Step::none() };
    let computer = state.computers.paired.iter().find(|item| item.link.id == place_computer.id);
    if let Some(refusal) = led_start_refusal(computer) {
        return failure(state, lead_id, proposal_id, refusal);
    }
    let computer = computer.expect("refusal covers a missing computer").clone();
    let project = chosen.map(|chosen| chosen.project.clone()).or_else(|| proposal.place.project.clone());
    let Some(project) = project.filter(|project| computer.state.as_deref().is_some_and(|held| held.projects.iter().any(|item| &item.id == project))) else {
        return failure(state, lead_id, proposal_id, format!("That folder is no longer on {}.", computer.link.name));
    };
    let mut place = proposal.place.clone();
    place.project = Some(project.clone());
    place.worktree = Some(chosen.map(|chosen| chosen.worktree).unwrap_or(proposal.place.worktree == Some(true)));
    let asker = proposal.from.as_ref().and_then(|from| state.threads.iter().find(|item| &item.id == from && coordinator_of(&state.threads, Some(item)).is_some_and(|held| held.id == lead.id))).map(|item| Thread::clone(item));
    if proposal.from.is_some() && asker.is_none() {
        return failure(state, lead_id, proposal_id, ASKER_GONE.into());
    }
    let caller = asker.unwrap_or_else(|| lead.clone());
    let asked = match proposal_send(&ThreadProposal { place: place.clone(), ..proposal.clone() }) {
        Ok(asked) => asked,
        Err(message) => return failure(state, lead_id, proposal_id, message),
    };
    let command = match new_thread_send(state, &caller, &asked, Some(Some(&computer))) {
        Ok(NewThreadStart::Here(command)) | Ok(NewThreadStart::Elsewhere { command, .. }) => command,
        Err(message) => return failure(state, lead_id, proposal_id, message),
    };
    let name = if state.computers.name.is_empty() { "another computer".to_string() } else { state.computers.name.clone() };
    let memory = memory_prompt(lead.memory.as_ref());
    let send = send_for(
        &computer,
        &TaskSend {
            text: Some(proposal.prompt.clone()),
            project: Some(project),
            worktree: place.worktree,
            model: command.model.clone(),
            engine: command.engine,
            effort: command.effort,
            role: command.role,
            brief: command.brief.clone(),
            title: command.title.clone(),
            memory: (!memory.is_empty()).then_some(memory),
            lead: Some(RemoteLead { thread_id: lead.id.clone(), title: lead.title.clone(), computer: name.clone(), proposal_id: Some(proposal_id.into()) }),
            sender: Some(RemoteSender { thread_id: caller.id.clone(), title: caller.title.clone(), computer: name }),
            ..TaskSend::default()
        },
    );
    let send = AppCommand::TaskSend(Box::new(send));
    if !supports_computer_command(computer.link.capabilities.as_deref(), &send) {
        return failure(state, lead_id, proposal_id, format!("Update AI Coding Tool on {} to run threads there.", computer.link.name));
    }
    if let Some(lead) = state.thread_mut(lead_id) {
        without_caught_start(lead, proposal_id);
        with_proposal_carried(lead, proposal_id);
    }
    state.starting_proposals.push(proposal_id.into());
    let effect: Effect = EffectWork::ComputerForward { id: computer.link.id.clone(), inputs: vec![send.into()], project_add_request: None, draft: None, proposal: Some(ForwardProposal { task_id: lead_id.into(), proposal_id: proposal_id.into() }), silent: false }.into();
    Step::effects(vec![effect])
}

/// A suggestion's start failing, with why, or a new start clearing that. Either way it is no longer starting.
pub fn proposal_failure(state: &mut WorkspaceState, lead_id: &str, proposal_id: &str, error: Option<String>) -> Step {
    if let Some(lead) = state.thread_mut(lead_id) {
        with_proposal_error(lead, proposal_id, error);
    }
    state.starting_proposals.retain(|id| id != proposal_id);
    Step::none()
}

/// The suggestions a start or a dismissal is for: the one named, else those named here, else every one here.
fn targeted_proposals(state: &WorkspaceState, lead: &crate::domain::thread::Thread, proposal_id: Option<&str>, proposal_ids: Option<&[String]>) -> Vec<ThreadProposal> {
    lead.proposals
        .iter()
        .flatten()
        .filter(|proposal| match proposal_id {
            Some(id) => proposal.id == id,
            None => proposal.place.computer.is_none() && proposal_ids.is_none_or(|ids| ids.contains(&proposal.id)),
        })
        .filter(|proposal| !state.starting_proposals.contains(&proposal.id))
        .cloned()
        .collect()
}
