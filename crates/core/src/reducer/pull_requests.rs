//! The pull request the checkout in front belongs to, those a coordinator's threads work on, and
//! which ask each answer belongs to.

use std::collections::HashSet;

use crate::commands::AppCommand;
use crate::domain::coordination::{is_coordinator, stuck_verdict, works_under_coordinator};
use crate::domain::pull_request::{link_matches, link_of, PullRequestAnswer, PullRequestLink, PullRequestRead, PullRequestRef, PullRequestState};
use crate::domain::thread::dismissed;
use crate::effects::{Effect, EffectWork};
use crate::events::{WorkspaceEvent, WorkspaceInput};
use crate::reducer::dock::current_workspace_id;
use crate::reducer::Step;
use crate::state::WorkspaceState;
use crate::util::{now, Map};
use crate::view::pull_request::{awaits_pull_request, branch_of, member_checkouts};
use crate::workspace::coordinated_status::own_pull_request;
use crate::workspace::coordination::{coordination_members, remotely_led_threads};
use crate::workspace::location::thread_workspace_id;

pub fn reduce_pull_requests(state: &mut WorkspaceState, input: &WorkspaceInput) -> Step {
    match input {
        WorkspaceInput::Command(AppCommand::PullRequestRead { task_id }) => {
            let thread_id = task_id.clone().or_else(|| state.current_id.clone()).filter(|id| !id.is_empty());
            let workspace_id = match task_id.as_ref().filter(|id| !id.is_empty()) {
                Some(task_id) => thread_workspace_id(state, state.thread(task_id)),
                None => current_workspace_id(state),
            };
            let Some(workspace_id) = workspace_id else {
                state.pull_request = None;
                return Step::none();
            };
            let branch = branch_of(state.environments.get(&workspace_id));
            let held = state.pull_request.as_ref();
            let kept = held.filter(|held| held.workspace_id == workspace_id && held.branch == branch).map(|held| held.answer.clone()).unwrap_or(PullRequestAnswer::None);
            let read = held.map(|held| held.read).unwrap_or(0) + 1;
            state.pull_request = Some(PullRequestRead { workspace_id: workspace_id.clone(), branch: branch.clone(), read, answer: kept, thread_ids: thread_id.map(|id| vec![id]), answered: false });
            Step::effect(EffectWork::ReadPullRequest { workspace_id, branch, read, members: false })
        }
        WorkspaceInput::Command(AppCommand::PullRequestReadMembers { task_id }) => {
            let lead_id = task_id.clone().or_else(|| state.current_id.clone());
            let lead = lead_id.as_deref().and_then(|id| state.thread(id)).filter(|lead| is_coordinator(Some(lead))).map(|lead| lead.id.clone());
            let ours: Vec<String> = match &lead {
                Some(lead) => member_checkouts(state, lead, Some(awaits_pull_request)).keys().cloned().collect(),
                None => Vec::new(),
            };
            let everyone = every_member_checkout(state);
            state.member_pull_requests.retain(|workspace_id, _| everyone.contains_key(workspace_id));
            let mut effects = Vec::new();
            for workspace_id in ours {
                let threads = everyone.get(&workspace_id).cloned().unwrap_or_default();
                effects.push(checkout_read(state, &workspace_id, threads));
            }
            if let Some(lead) = &lead {
                effects.extend(remote_reads(state, lead));
            }
            Step::effects(effects)
        }
        WorkspaceInput::Command(AppCommand::PullRequestReadMember { task_id }) => {
            if !state.thread(task_id).is_some_and(|thread| thread.archived_at.is_none() && works_under_coordinator(&state.threads, Some(thread)) && awaits_pull_request(thread)) {
                return Step::none();
            }
            match member_read(state, task_id) {
                Some(effect) => Step::effects(vec![effect]),
                None => Step::none(),
            }
        }
        WorkspaceInput::Event(WorkspaceEvent::PullRequestAnswered { workspace_id, branch, read, members, answer }) => {
            let held = state.pull_request.clone();
            let answers_held = !members && held.as_ref().is_some_and(|held| held.read == *read && &held.workspace_id == workspace_id && &held.branch == branch);
            let member = state.member_pull_requests.get(workspace_id).cloned();
            let answers_member = *members && member.as_ref().is_some_and(|member| member.read == *read && &member.branch == branch);
            if !answers_held && !answers_member {
                return Step::none();
            }
            let current = answers_held && held.as_ref().is_some_and(|held| held.answer != *answer);
            let changed_member = answers_member && member.as_ref().is_some_and(|member| !member.answered || member.answer != *answer);
            let thread_ids = if answers_held { held.as_ref().and_then(|held| held.thread_ids.clone()) } else { member.as_ref().and_then(|member| member.thread_ids.clone()) };
            linked(state, thread_ids.as_deref(), answer);
            if current {
                if let Some(held) = state.pull_request.as_mut() {
                    held.answer = answer.clone();
                }
            }
            if changed_member {
                if let Some(member) = state.member_pull_requests.get_mut(workspace_id) {
                    member.answer = answer.clone();
                    member.answered = true;
                }
            }
            Step::none()
        }
        _ => Step::none(),
    }
}

/// Asks again about the pull request of the checkout a thread under a coordinator works in.
pub fn member_pull_request_read(state: &mut WorkspaceState, task_id: &str) -> Option<Effect> {
    member_read(state, task_id)
}

fn member_read(state: &mut WorkspaceState, task_id: &str) -> Option<Effect> {
    let workspace_id = thread_workspace_id(state, state.thread(task_id))?;
    let threads = every_member_checkout(state).get(&workspace_id).cloned().unwrap_or_else(|| vec![task_id.to_string()]);
    Some(checkout_read(state, &workspace_id, threads))
}

fn checkout_read(state: &mut WorkspaceState, workspace_id: &str, threads: Vec<String>) -> Effect {
    let held = state.member_pull_requests.get(workspace_id);
    let branch = branch_of(state.environments.get(workspace_id));
    let kept = held.filter(|held| held.branch == branch);
    let read = PullRequestRead {
        workspace_id: workspace_id.into(),
        branch: branch.clone(),
        read: held.map(|held| held.read).unwrap_or(0) + 1,
        answer: kept.map(|kept| kept.answer.clone()).unwrap_or(PullRequestAnswer::None),
        thread_ids: Some(threads),
        answered: kept.is_some_and(|kept| kept.answered),
    };
    let number = read.read;
    state.member_pull_requests.insert(workspace_id.into(), read);
    EffectWork::ReadPullRequest { workspace_id: workspace_id.into(), branch, read: number, members: true }.into()
}

/// Each paired computer holding a coordinator's threads is asked to read their checkouts there.
fn remote_reads(state: &WorkspaceState, lead_id: &str) -> Vec<Effect> {
    let mut asks: Map<Vec<crate::events::WorkspaceInput>> = Map::new();
    for member in coordination_members(state, lead_id) {
        let Some(computer) = member.computer else { continue };
        if computer.link.status != crate::domain::platform::ComputerStatus::Connected || !awaits_pull_request(member.thread) {
            continue;
        }
        let read = AppCommand::PullRequestReadMember { task_id: member.thread.id.clone() };
        if crate::validation::supports_computer_command(computer.link.capabilities.as_deref(), &read) {
            asks.entry(computer.link.id.clone()).or_default().push(read.into());
        }
    }
    asks.into_iter().map(|(id, inputs)| crate::reducer::computers::forward(&id, inputs)).collect()
}

/// Every checkout a thread under any live coordinator works in, with those threads.
fn every_member_checkout(state: &WorkspaceState) -> Map<Vec<String>> {
    let mut checkouts: Map<Vec<String>> = Map::new();
    for lead in &state.threads {
        if !is_coordinator(Some(lead)) {
            continue;
        }
        for (workspace_id, threads) in member_checkouts(state, &lead.id, None) {
            checkouts.entry(workspace_id).or_default().extend(threads.into_iter().map(|thread| thread.id.clone()));
        }
    }
    for thread in remotely_led_threads(&state.threads) {
        if let Some(workspace_id) = thread_workspace_id(state, Some(thread)) {
            checkouts.entry(workspace_id).or_default().push(thread.id.clone());
        }
    }
    checkouts
}

/// Records a found pull request on the threads it answers for; one whose own just merged resolves.
fn linked(state: &mut WorkspaceState, thread_ids: Option<&[String]>, answer: &PullRequestAnswer) {
    let PullRequestAnswer::Found { pull_request } = answer else { return };
    let Some(thread_ids) = thread_ids.filter(|ids| !ids.is_empty()) else { return };
    let ids: HashSet<&str> = thread_ids.iter().map(String::as_str).collect();
    let mut resolving = HashSet::new();
    let mut linking = HashSet::new();
    for thread in &state.threads {
        if !ids.contains(thread.id.as_str()) || link_matches(thread.pull_request.as_ref(), pull_request) {
            continue;
        }
        linking.insert(thread.id.clone());
        let mut after = thread.clone();
        after.pull_request = Some(link_of(pull_request));
        let merged = !merged_already(thread.pull_request.as_ref(), pull_request) && own_pull_request(&after).is_some_and(|own| own.state == PullRequestState::Merged);
        if merged && after.resolved_at.is_none() && after.archived_at.is_none() && stuck_verdict(&after).is_none() && works_under_coordinator(&state.threads, Some(&after)) {
            resolving.insert(thread.id.clone());
        }
    }
    for thread in state.threads.iter_mut().filter(|thread| linking.contains(&thread.id)) {
        thread.pull_request = Some(link_of(pull_request));
    }
    if resolving.is_empty() {
        return;
    }
    let at = now();
    for thread in state.threads.iter_mut().filter(|thread| resolving.contains(&thread.id)) {
        thread.resolved_at = Some(at);
    }
    dismissed(&mut state.threads, &resolving);
}

fn merged_already(link: Option<&PullRequestLink>, pull_request: &PullRequestRef) -> bool {
    link.is_some_and(|link| link.state == Some(PullRequestState::Merged) && link.number == pull_request.number && link.url == pull_request.url)
}
