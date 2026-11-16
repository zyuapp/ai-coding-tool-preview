//! What a coordinator's Overview, cards and Needs-you card show of it and its threads, and what a
//! thread under a coordinator shows of it.

use std::collections::HashSet;

use crate::domain::coordination::{coordination_decisions, coordinator_of, is_coordinator, open_decisions, remote_lead_of, thread_model_of, waiting_on_user, Decision, MemoryNote, NextStep, OverviewSection, ThreadBrief, ThreadModelChoice, ThreadProposal, WaitingOnUser};
use crate::domain::engine::{current_model, AgentEngine};
use crate::domain::platform::ComputerStatus;
use crate::domain::project::Project;
use crate::domain::thread::{thread_created_at, Thread};
use crate::reducer::computers::led_start_refusal;
use crate::state::{ApprovalView, PairedComputer, RunPhase, WorkspaceState};
use crate::view::computers::thread_host_of;
use crate::view::sidebar::ThreadHost;
use crate::workspace::activity::thread_activity;
use crate::workspace::coordinated_status::{checkout_answer, coordinated_thread_status, group_of, next_steps, own_pull_request, CoordinatedThreadStatus, OwnPullRequest};
use crate::workspace::coordination::{coordination_members, waiting_line, CoordinationMember};

/// One thread under a coordinator as its Overview row and its card show it.
#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatedThreadView<'a> {
    pub thread: &'a Thread,
    pub status: CoordinatedThreadStatus,
    pub summary: Option<String>,
    pub pull_request: Option<OwnPullRequest>,
    pub steps: Vec<NextStep>,
    /// When a thread the coordinator started began; None for one moved under it by hand.
    pub started_at: Option<i64>,
    /// The paired computer holding the thread, or None for one of this computer's own.
    pub host: Option<ThreadHost>,
    /// Working as the computer holding it last said, that computer being out of reach now.
    pub last_heard: bool,
}

/// A thread the coordinator started, and when, which is where its card sits in its conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadStart {
    pub id: String,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoordinationDecisionView<'a> {
    pub thread: &'a Thread,
    pub decision: &'a Decision,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoordinationApprovalView<'a> {
    pub thread: &'a Thread,
    pub approval: &'a ApprovalView,
}

/// Where a thread asked for on a paired computer would run, and why it cannot right now.
#[derive(Clone, Debug, PartialEq)]
pub struct ElsewhereView<'a> {
    pub computer: ThreadHost,
    pub projects: Vec<&'a Project>,
    pub project: Option<String>,
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposalView<'a> {
    pub proposal: &'a ThreadProposal,
    pub starting: bool,
    pub elsewhere: Option<ElsewhereView<'a>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatorMemoryView<'a> {
    pub purpose: Option<&'a str>,
    pub notes: &'a [MemoryNote],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelUsage {
    pub engine: AgentEngine,
    pub model: String,
    pub tokens: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadUsage<'a> {
    pub thread: &'a Thread,
    pub computer: Option<&'a PairedComputer>,
    pub tokens: i64,
}

/// What a coordinator and the threads under it have spent, most first.
#[derive(Clone, Debug, PartialEq)]
pub struct CoordinationUsage<'a> {
    pub total: i64,
    pub coordinator: i64,
    pub by_model: Vec<ModelUsage>,
    pub by_thread: Vec<ThreadUsage<'a>>,
}

/// The coordinator on another computer a thread works under, with its id when a paired computer here holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteLeadView {
    pub title: String,
    pub computer: String,
    pub thread_id: Option<String>,
}

/// What the open thread shows of coordination.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoordinationView<'a> {
    pub coordinator: bool,
    pub thread_model: Option<ThreadModelChoice>,
    pub memory: Option<CoordinatorMemoryView<'a>>,
    pub members: Vec<CoordinatedThreadView<'a>>,
    /// The threads it started, oldest first.
    pub starts: Vec<ThreadStart>,
    pub approvals: Vec<CoordinationApprovalView<'a>>,
    pub decisions: Vec<CoordinationDecisionView<'a>>,
    pub proposals: Vec<ProposalView<'a>>,
    pub usage: Option<CoordinationUsage<'a>>,
    pub lead: Option<&'a Thread>,
    pub remote_lead: Option<RemoteLeadView>,
    pub brief: Option<&'a ThreadBrief>,
    /// On a coordinator, what its Needs-you card holds waiting on the user.
    pub waiting: Option<WaitingOnUser>,
    /// On a thread under a coordinator, what of its own waits on the user; None when nothing does.
    pub asking: Option<String>,
}

/// What a coordinator and every thread under it spent, or None before any of them has spent anything.
pub fn coordination_usage<'a>(members: &[CoordinationMember<'a>], lead: &'a Thread) -> Option<CoordinationUsage<'a>> {
    let mut by_model: Vec<ModelUsage> = Vec::new();
    let mut spent = |thread: &Thread| -> i64 {
        let mut tokens = 0;
        for (recorded, count) in thread.usage.iter().flatten() {
            let model = current_model(recorded).to_string();
            match by_model.iter_mut().find(|entry| entry.engine == thread.engine && entry.model == model) {
                Some(entry) => entry.tokens += count,
                None => by_model.push(ModelUsage { engine: thread.engine, model, tokens: *count }),
            }
            tokens += count;
        }
        tokens
    };
    let coordinator = spent(lead);
    let mut by_thread: Vec<ThreadUsage> = members
        .iter()
        .filter_map(|member| {
            let tokens = spent(member.thread);
            (tokens != 0).then_some(ThreadUsage { thread: member.thread, computer: member.computer, tokens })
        })
        .collect();
    let total = by_thread.iter().map(|entry| entry.tokens).sum::<i64>() + coordinator;
    if total == 0 {
        return None;
    }
    by_model.retain(|entry| entry.tokens != 0);
    by_model.sort_by_key(|entry| std::cmp::Reverse(entry.tokens));
    by_thread.sort_by_key(|entry| std::cmp::Reverse(entry.tokens));
    Some(CoordinationUsage { total, coordinator, by_model, by_thread })
}

fn member_approval<'a>(state: &'a WorkspaceState, member: &CoordinationMember<'a>) -> Option<&'a ApprovalView> {
    let holder = member.holder().unwrap_or(state);
    let run = holder.active_runs.get(&member.thread.id)?;
    (run.phase == RunPhase::AwaitingApproval).then(|| holder.approvals.get(&run.run_id)).flatten()
}

fn member_view<'a>(state: &'a WorkspaceState, member: &CoordinationMember<'a>, busy: &HashSet<String>, blocked: &HashSet<String>) -> CoordinatedThreadView<'a> {
    let thread = member.thread;
    let there = member.holder().map(thread_activity);
    let (working, waiting) = match &there {
        Some(activity) => (&activity.working, &activity.blocked),
        None => (busy, blocked),
    };
    let standing = coordinated_thread_status(thread, working, waiting);
    let steps = next_steps(thread, standing.status, checkout_answer(member.holder().unwrap_or(state), thread));
    let host = member.computer.map(thread_host_of);
    CoordinatedThreadView {
        thread,
        status: standing.status,
        summary: standing.summary,
        pull_request: own_pull_request(thread),
        steps,
        started_at: thread.brief.as_ref().map(|_| thread_created_at(thread)),
        last_heard: standing.status == CoordinatedThreadStatus::Working && host.as_ref().is_some_and(|host| host.offline),
        host,
    }
}

fn elsewhere_view<'a>(proposal: &ThreadProposal, paired: &'a [PairedComputer]) -> Option<ElsewhereView<'a>> {
    let place = proposal.place.computer.as_ref()?;
    let computer = paired.iter().find(|item| item.link.id == place.id);
    let projects: Vec<&Project> = computer.and_then(|computer| computer.state.as_deref()).map(|held| held.projects.iter().filter(|project| !project.unlisted || Some(&project.id) == proposal.place.project.as_ref()).collect()).unwrap_or_default();
    let project = projects.iter().find(|item| Some(&item.id) == proposal.place.project.as_ref()).or(projects.first()).map(|project| project.id.clone());
    let refusal = led_start_refusal(computer).or_else(|| project.is_none().then(|| format!("That folder is no longer on {}.", computer.map(|computer| computer.link.name.as_str()).unwrap_or(&place.name))));
    let host = match computer {
        Some(computer) => thread_host_of(computer),
        None => ThreadHost { id: place.id.clone(), name: place.name.clone(), offline: true, reconnecting: false },
    };
    Some(ElsewhereView { computer: host, projects, project, refusal })
}

/// What the open thread shows: a coordinator its threads, approvals, decisions, suggestions, usage and
/// memory; a thread its coordinator and brief.
pub fn coordination_view<'a>(state: &'a WorkspaceState, thread: Option<&'a Thread>, busy: &HashSet<String>, blocked: &HashSet<String>) -> CoordinationView<'a> {
    let Some(thread) = thread else { return CoordinationView::default() };
    if is_coordinator(Some(thread)) {
        let coordinated = coordination_members(state, &thread.id);
        let members: Vec<CoordinatedThreadView> = coordinated.iter().map(|member| member_view(state, member, busy, blocked)).collect();
        let mut starts: Vec<ThreadStart> = members.iter().filter_map(|view| view.started_at.map(|at| ThreadStart { id: view.thread.id.clone(), at })).collect();
        starts.sort_by_key(|start| start.at);
        let mut approvals = Vec::new();
        if let Some(own) = state.active_runs.get(&thread.id).filter(|run| run.phase == RunPhase::AwaitingApproval).and_then(|run| state.approvals.get(&run.run_id)) {
            approvals.push(CoordinationApprovalView { thread, approval: own });
        }
        for (view, member) in members.iter().zip(&coordinated) {
            if view.status != CoordinatedThreadStatus::Approval {
                continue;
            }
            if let Some(approval) = member_approval(state, member) {
                approvals.push(CoordinationApprovalView { thread: view.thread, approval });
            }
        }
        let member_threads: Vec<&Thread> = coordinated.iter().map(|member| member.thread).collect();
        let decisions = coordination_decisions(thread, &member_threads).into_iter().map(|(thread, decision)| CoordinationDecisionView { thread, decision }).collect();
        let memory = CoordinatorMemoryView { purpose: thread.memory.as_ref().and_then(|memory| memory.purpose.as_deref()), notes: thread.memory.as_ref().map(|memory| memory.notes.as_slice()).unwrap_or(&[]) };
        let proposals = thread.proposals.iter().flatten().map(|proposal| ProposalView { proposal, starting: state.starting_proposals.contains(&proposal.id), elsewhere: elsewhere_view(proposal, &state.computers.paired) }).collect();
        return CoordinationView {
            coordinator: true,
            thread_model: thread_model_of(&state.catalog(), thread),
            memory: Some(memory),
            usage: coordination_usage(&coordinated, thread),
            waiting: Some(waiting_on_user(thread, &member_threads, &state.starting_proposals)),
            brief: thread.brief.as_ref(),
            members,
            starts,
            approvals,
            decisions,
            proposals,
            ..CoordinationView::default()
        };
    }
    let lead = coordinator_of(&state.threads, Some(thread));
    let remote = if lead.is_some() { None } else { remote_lead_of(Some(thread)) };
    let decisions: Vec<CoordinationDecisionView> = if lead.is_some() || remote.is_some() { Vec::new() } else { open_decisions(thread).into_iter().map(|decision| CoordinationDecisionView { thread, decision }).collect() };
    if lead.is_none() && remote.is_none() && thread.brief.is_none() && decisions.is_empty() {
        return CoordinationView::default();
    }
    let remote_lead = remote.map(|remote| RemoteLeadView {
        title: remote.title.clone(),
        computer: remote.computer.clone(),
        thread_id: state.computers.paired.iter().any(|computer| computer.state.as_deref().is_some_and(|held| held.thread(&remote.thread_id).is_some())).then(|| remote.thread_id.clone()),
    });
    let asking = if lead.is_some() || remote.is_some() { waiting_line(&waiting_on_user(thread, &[], &state.starting_proposals), "you") } else { None };
    CoordinationView { lead, remote_lead, decisions, brief: thread.brief.as_ref(), asking, ..CoordinationView::default() }
}

/// A coordinator's threads sorted into its Overview's groups.
pub fn overview_groups<'a, 'b>(members: &'b [CoordinatedThreadView<'a>]) -> Vec<(OverviewSection, Vec<&'b CoordinatedThreadView<'a>>)> {
    crate::domain::coordination::OVERVIEW_GROUPS.iter().map(|group| (*group, members.iter().filter(|member| group_of(member.status) == *group).collect())).collect()
}

/// Whether a paired computer's line is up.
pub fn connected(computer: &PairedComputer) -> bool {
    computer.link.status == ComputerStatus::Connected
}
