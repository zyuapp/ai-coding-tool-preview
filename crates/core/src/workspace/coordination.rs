//! Who works under a coordinator and where each stands as its roster, what Stop all reaches, and how
//! the sidebar groups them.

use crate::shared::Shared;
use std::collections::{HashMap, HashSet};

use crate::domain::coordination::{
    coordinated_ids, coordinator_of, is_coordinator, lead_id_of, remote_lead_of, waiting_on_user, with_coordinated, CoordinationRole, ThreadProposal, WaitingOnUser,
};
use crate::domain::thread::{newest_unread_finding, wants_attention, Thread};
use crate::domain::thread_role::ThreadRole;
use crate::state::{PairedComputer, PendingRun, RunOrigin, WorkspaceState};
use crate::workspace::activity::{doing_work, thread_activity, unstopped_session_work, ThreadActivity};
use crate::workspace::order::{activity_sections, ActivitySections};

/// Which kind of run the agent process starts for a thread.
pub fn coordination_role(threads: &[Shared<Thread>], thread: &Thread) -> Option<CoordinationRole> {
    if is_coordinator(Some(thread)) {
        return Some(CoordinationRole::Coordinator);
    }
    (coordinator_of(threads, Some(thread)).is_some() || remote_lead_of(Some(thread)).is_some()).then_some(CoordinationRole::Member)
}

/// A thread working under a coordinator: one of this computer's own, or one a paired computer holds.
#[derive(Clone, Debug)]
pub struct CoordinationMember<'a> {
    pub thread: &'a Thread,
    /// The paired computer holding it, or None for one of this computer's own.
    pub computer: Option<&'a PairedComputer>,
}

impl<'a> CoordinationMember<'a> {
    pub fn holder(&self) -> Option<&'a WorkspaceState> {
        self.computer.and_then(|computer| computer.state.as_deref())
    }
}

/// Threads in a paired computer's list that a coordinator elsewhere started and that still work under it.
pub fn remotely_led_threads(threads: &[Shared<Thread>]) -> Vec<&Thread> {
    threads.iter().map(|thread| &**thread).filter(|thread| remote_lead_of(Some(thread)).is_some() && thread.archived_at.is_none()).collect()
}

/// Every thread working under a live coordinator: this computer's, then each paired computer's.
pub fn coordination_members<'a>(state: &'a WorkspaceState, lead_id: &str) -> Vec<CoordinationMember<'a>> {
    let live = is_coordinator(state.thread(lead_id));
    if !live {
        return Vec::new();
    }
    let mut members: Vec<CoordinationMember> = crate::domain::coordination::coordinated_threads(&state.threads, lead_id).into_iter().map(|thread| CoordinationMember { thread, computer: None }).collect();
    for computer in &state.computers.paired {
        let Some(holder) = computer.state.as_deref() else { continue };
        for thread in remotely_led_threads(&holder.threads) {
            if thread.remote_lead.as_ref().is_some_and(|lead| lead.thread_id == lead_id) {
                members.push(CoordinationMember { thread, computer: Some(computer) });
            }
        }
    }
    members
}

/// A thread under a coordinator as the computer holding it last said it stands.
#[derive(Clone, Debug)]
pub struct RosterMember<'a> {
    pub member: CoordinationMember<'a>,
    pub working: bool,
    pub blocked: bool,
    pub ranked: bool,
}

/// Where a coordinator's threads stand.
#[derive(Clone, Debug)]
pub struct CoordinatorRoster<'a> {
    pub members: Vec<RosterMember<'a>>,
    pub threads: Vec<&'a Thread>,
    pub waiting: WaitingOnUser,
    pub blocked: bool,
    pub running: bool,
    /// The threads under it that Dismiss on its row files away with it: none waiting on an approval.
    pub filed: Vec<RosterMember<'a>>,
    pub dismissable: bool,
}

/// Where the threads under a coordinator stand. Only the paired computers `on` count, every one when None.
pub fn coordinator_roster<'a>(state: &'a WorkspaceState, lead: &'a Thread, here: &ThreadActivity, on: Option<&[&PairedComputer]>) -> CoordinatorRoster<'a> {
    let base = if is_coordinator(Some(lead)) { coordination_members(state, &lead.id) } else { Vec::new() };
    let mut members = Vec::new();
    for member in base {
        if let (Some(computer), Some(on)) = (member.computer, on) {
            if !on.iter().any(|shown| shown.link.id == computer.link.id) {
                continue;
            }
        }
        let there = member.holder().map(thread_activity);
        let activity = there.as_ref().unwrap_or(here);
        let id = member.thread.id.clone();
        members.push(RosterMember { working: doing_work(activity, &id), blocked: activity.blocked.contains(&id), ranked: activity.ranked.contains(&id), member });
    }
    let threads: Vec<&Thread> = members.iter().map(|member| member.member.thread).collect();
    let filed: Vec<RosterMember> = members.iter().filter(|member| !member.blocked).cloned().collect();
    let filed_threads: Vec<&Thread> = filed.iter().map(|member| member.member.thread).collect();
    CoordinatorRoster {
        waiting: waiting_on_user(lead, &threads, &state.starting_proposals),
        blocked: members.iter().any(|member| member.blocked),
        running: members.iter().any(|member| member.working),
        dismissable: can_dismiss(lead, &filed_threads, &here.blocked),
        threads,
        filed,
        members,
    }
}

/// Threads under a coordinator a paired computer last said were working, which it has said nothing of since.
pub fn unheard_work<'a>(state: &'a WorkspaceState, lead: &'a Thread) -> Vec<(String, String, Option<&'a PairedComputer>)> {
    let Some(working) = &lead.remote_working else { return Vec::new() };
    working
        .iter()
        .filter_map(|(thread_id, heard)| {
            let computer = state.computers.paired.iter().find(|item| item.link.id == heard.computer);
            if computer.is_some_and(|computer| computer.state.is_some()) {
                None
            } else {
                Some((thread_id.clone(), heard.title.clone(), computer))
            }
        })
        .collect()
}

/// A thread under a coordinator on a paired computer whose line is up, and what of it that computer can stop.
#[derive(Clone, Debug)]
pub struct RemoteWork<'a> {
    pub thread: &'a Thread,
    pub computer: &'a PairedComputer,
    pub run_id: Option<String>,
    pub process_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionWork {
    pub task_id: String,
    pub process_ids: Vec<String>,
}

/// The work Stop all stops under a coordinator, its own included.
#[derive(Clone, Debug)]
pub struct CoordinatorWork<'a> {
    pub threads: Vec<&'a Thread>,
    pub starts: Vec<&'a PendingRun>,
    pub session_work: Vec<SessionWork>,
    pub elsewhere: Vec<RemoteWork<'a>>,
    pub landing: Vec<&'a ThreadProposal>,
}

/// What Stop all would stop under a coordinator.
pub fn coordinator_work<'a>(state: &'a WorkspaceState, lead: &'a Thread) -> CoordinatorWork<'a> {
    let coordinated = coordination_members(state, &lead.id);
    let mut threads = vec![lead];
    threads.extend(coordinated.iter().filter(|member| member.computer.is_none()).map(|member| member.thread));
    let ids: HashSet<&str> = threads.iter().map(|thread| thread.id.as_str()).collect();
    let starts: Vec<&PendingRun> = state
        .pending_runs
        .values()
        .filter(|pending| {
            pending.origin == RunOrigin::Composer
                && match &pending.task_id {
                    Some(task_id) => {
                        (task_id == &lead.id && pending.coordination.as_ref().is_some_and(|send| send.notes.is_some())) || (ids.contains(task_id.as_str()) && pending.queued_ids.is_some())
                    }
                    None => pending.coordination.as_ref().and_then(|send| send.coordinator_id.as_deref()) == Some(lead.id.as_str()),
                }
        })
        .collect();
    let session_work: Vec<SessionWork> = threads
        .iter()
        .filter_map(|thread| {
            let process_ids = unstopped_session_work(state, &thread.id);
            (!process_ids.is_empty()).then(|| SessionWork { task_id: thread.id.clone(), process_ids })
        })
        .collect();
    let elsewhere: Vec<RemoteWork> = coordinated
        .iter()
        .filter_map(|member| {
            let computer = member.computer?;
            if computer.link.status != crate::domain::platform::ComputerStatus::Connected {
                return None;
            }
            let holder = computer.state.as_deref()?;
            let thread = member.thread;
            let run = holder.active_runs.get(&thread.id);
            let process_ids: Vec<String> = unstopped_session_work(holder, &thread.id)
                .into_iter()
                .filter(|process_id| crate::validation::supports_computer_command(computer.link.capabilities.as_deref(), &crate::commands::AppCommand::RunStopProcess { task_id: Some(thread.id.clone()), process_id: process_id.clone() }))
                .collect();
            (run.is_some() || thread.limit_pause.is_some() || !process_ids.is_empty()).then(|| RemoteWork { thread, computer, run_id: run.map(|run| run.run_id.clone()), process_ids })
        })
        .collect();
    let landing: Vec<&ThreadProposal> = lead
        .proposals
        .iter()
        .flatten()
        .filter(|proposal| proposal.place.computer.is_some() && state.starting_proposals.contains(&proposal.id) && lead.caught_starts.as_ref().and_then(|caught| caught.get(&proposal.id)).is_none())
        .collect();
    CoordinatorWork { threads, starts, session_work, elsewhere, landing }
}

/// Each live coordinator with something Stop all could stop.
pub fn stoppable_coordinators(state: &WorkspaceState) -> HashSet<String> {
    let mut stoppable = HashSet::new();
    for lead in &state.threads {
        if !is_coordinator(Some(lead)) {
            continue;
        }
        let work = coordinator_work(state, lead);
        if !work.starts.is_empty() || !work.session_work.is_empty() || !work.elsewhere.is_empty() || !work.landing.is_empty() || work.threads.iter().any(|thread| state.active_runs.contains_key(&thread.id) || thread.limit_pause.is_some()) {
            stoppable.insert(lead.id.clone());
        }
    }
    stoppable
}

/// Whether a finished thread under a coordinator is waiting behind it.
pub fn members_want_attention(members: &[&Thread]) -> bool {
    members.iter().any(|thread| thread.resolved_at.is_none() && wants_attention(thread))
}

fn counted(count: usize, one: &str, many: &str) -> Option<String> {
    match count {
        0 => None,
        1 => Some(one.to_string()),
        n => Some(format!("{n} {many}")),
    }
}

/// What a row says waits on the user, or None when nothing does.
pub fn waiting_line(waiting: &WaitingOnUser, owed_by: &str) -> Option<String> {
    let computers: HashSet<&String> = waiting.requests.iter().collect();
    let place = if computers.len() == 1 { format!("on {}", waiting.requests[0]) } else { "on other computers".into() };
    let parts: Vec<String> = [
        counted(waiting.decisions, "a decision", "decisions"),
        counted(waiting.requests.len(), &format!("a request to run a thread {place}"), &format!("requests to run threads {place}")),
        counted(waiting.suggestions, "a suggested thread", "suggested threads"),
        counted(waiting.blocked, "a blocked thread", "blocked threads"),
        counted(waiting.failed, "a failed thread", "failed threads"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return None;
    }
    let said = if parts.len() > 1 { format!("{} and {}", parts[..parts.len() - 1].join(", "), parts[parts.len() - 1]) } else { parts[0].clone() };
    let mut chars = said.chars();
    let first = chars.next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_default();
    let total = waiting.decisions + waiting.suggestions + waiting.requests.len() + waiting.blocked + waiting.failed;
    Some(format!("{first}{} {} waiting on {owed_by}", chars.as_str(), if total == 1 { "is" } else { "are" }))
}

/// A row's second line once nothing under it waits on an approval.
pub fn row_headline(thread: &Thread, waiting: Option<String>) -> Option<String> {
    waiting.or_else(|| newest_unread_finding(thread).map(|finding| finding.headline.clone()))
}

/// Whether Dismiss on a row files anything away.
pub fn can_dismiss(thread: &Thread, members: &[&Thread], blocked: &HashSet<String>) -> bool {
    let unblocked: Vec<&Thread> = members.iter().copied().filter(|member| !blocked.contains(&member.id)).collect();
    !blocked.contains(&thread.id) && (wants_attention(thread) || members_want_attention(&unblocked))
}

/// What a Dismiss of the rows `ids` files away: those rows and a coordinator's threads, none waiting on an approval.
pub fn dismissed_with(threads: &[Shared<Thread>], ids: &HashSet<String>, blocked: &HashSet<String>) -> HashSet<String> {
    with_coordinated(threads, ids).into_iter().filter(|id| !blocked.contains(id)).collect()
}

/// The activity lists with each coordinator standing for its threads.
pub fn coordination_sections<'a>(threads: &[&'a Thread], busy: &HashSet<String>, blocked: &HashSet<String>, starting: &[String]) -> ActivitySections<'a> {
    let members = coordinated_ids(threads);
    if members.is_empty() && !threads.iter().any(|thread| thread.decisions.as_ref().is_some_and(|d| !d.is_empty()) || thread.proposals.as_ref().is_some_and(|p| !p.is_empty())) {
        return activity_sections(threads, busy, blocked);
    }
    let mut ranked = busy.clone();
    let mut asking = blocked.clone();
    let mut attention = HashSet::new();
    // Each coordinator's threads, gathered in one pass as `coordinated_refs` would list them.
    let mut led_by: HashMap<&str, Vec<&'a Thread>> = HashMap::new();
    for thread in threads.iter().copied().filter(|thread| thread.archived_at.is_none() && thread.role != Some(ThreadRole::Coordinator)) {
        if let Some(lead_id) = lead_id_of(thread) {
            led_by.entry(lead_id).or_default().push(thread);
        }
    }
    for lead in threads.iter().copied().filter(|thread| is_coordinator(Some(thread))) {
        let led = led_by.get(lead.id.as_str()).cloned().unwrap_or_default();
        let coordination: Vec<&Thread> = std::iter::once(lead).chain(led.iter().copied()).collect();
        if coordination.iter().any(|thread| blocked.contains(&thread.id)) || waiting_on_user(lead, &led, starting).any() {
            asking.insert(lead.id.clone());
        }
        if coordination.iter().any(|thread| busy.contains(&thread.id)) {
            ranked.insert(lead.id.clone());
        }
        if members_want_attention(&led) {
            attention.insert(lead.id.clone());
        }
    }
    for thread in threads.iter().copied() {
        if thread.role != Some(ThreadRole::Coordinator) && waiting_on_user(thread, &[], starting).any() && !members.contains(&thread.id) {
            asking.insert(thread.id.clone());
        }
    }
    let listed: Vec<&Thread> = threads.iter().copied().filter(|thread| !members.contains(&thread.id)).collect();
    let mut sections = activity_sections(&listed, &ranked, &asking);
    if attention.is_empty() {
        return sections;
    }
    let promoted: Vec<&Thread> = sections.threads.iter().copied().filter(|thread| attention.contains(&thread.id)).collect();
    if promoted.is_empty() {
        return sections;
    }
    let moved: HashSet<&str> = promoted.iter().map(|thread| thread.id.as_str()).collect();
    sections.threads.retain(|thread| !moved.contains(thread.id.as_str()));
    sections.priority.extend(promoted);
    sections
}

/// A computer's own threads ranked as its sidebar ranks them, coordinators standing for their threads elsewhere too.
pub fn own_coordination_sections<'a>(state: &'a WorkspaceState, own: &[&'a Thread], here: &ThreadActivity, computers: &[&PairedComputer]) -> (ActivitySections<'a>, Vec<&'a Thread>) {
    let mut elsewhere: Vec<RosterMember<'a>> = Vec::new();
    for lead in own.iter().copied().filter(|thread| is_coordinator(Some(thread))) {
        elsewhere.extend(coordinator_roster(state, lead, here, Some(computers)).members.into_iter().filter(|member| member.member.computer.is_some()));
    }
    if elsewhere.is_empty() {
        return (coordination_sections(own, &here.ranked, &here.blocked, &state.starting_proposals), own.to_vec());
    }
    let mut threads = own.to_vec();
    threads.extend(elsewhere.iter().map(|member| member.member.thread));
    let mut ranked = here.ranked.clone();
    let mut blocked = here.blocked.clone();
    for member in &elsewhere {
        if member.ranked {
            ranked.insert(member.member.thread.id.clone());
        }
        if member.blocked {
            blocked.insert(member.member.thread.id.clone());
        }
    }
    (coordination_sections(&threads, &ranked, &blocked, &state.starting_proposals), threads)
}

/// The activity lists narrowed to one project; a coordinator stays while any thread under it works there.
pub fn project_activity<'a>(sections: &ActivitySections<'a>, threads: &[&'a Thread], project_id: &str) -> ActivitySections<'a> {
    let members = coordinated_ids(threads);
    let mut leads: HashSet<String> = HashSet::new();
    for thread in threads {
        if members.contains(&thread.id) && thread.project_id.as_deref() == Some(project_id) {
            if let Some(lead) = lead_id_of(thread) {
                leads.insert(lead.to_string());
            }
        }
    }
    let keep = |thread: &&Thread| thread.project_id.as_deref() == Some(project_id) || leads.contains(&thread.id);
    ActivitySections {
        priority: sections.priority.iter().copied().filter(keep).collect(),
        running: sections.running.iter().copied().filter(keep).collect(),
        threads: sections.threads.iter().copied().filter(keep).collect(),
        snoozed: sections.snoozed.iter().copied().filter(keep).collect(),
    }
}
