//! The pull request the checkout in front belongs to, and those a coordinator's threads work on.

use crate::domain::pull_request::{pull_request_settled, PullRequestAnswer, PullRequestRead};
use crate::domain::thread::Thread;
use crate::events::ChangedFilesResult;
use crate::state::WorkspaceState;
use crate::util::Map;
use crate::workspace::coordinated_status::{own_pull_request, owns_pull_request, OwnPullRequest};
use crate::workspace::coordination::coordination_members;
use crate::workspace::location::thread_workspace_id;

/// The branch a checkout sits on, or None while Git has not said or could not.
pub fn branch_of(environment: Option<&ChangedFilesResult>) -> Option<String> {
    match environment {
        Some(ChangedFilesResult::Available { branch, .. }) => branch.clone(),
        _ => None,
    }
}

/// The answer held for a checkout, kept only while that checkout and branch are the ones on screen.
pub fn pull_request_for(held: Option<&PullRequestRead>, workspace_id: Option<&str>, environment: Option<&ChangedFilesResult>) -> PullRequestAnswer {
    match held {
        Some(held) if Some(held.workspace_id.as_str()) == workspace_id && held.branch == branch_of(environment) => held.answer.clone(),
        _ => PullRequestAnswer::None,
    }
}

/// What one thread's panel asks about: changes with the checkout, its branch, or the thread reading it.
pub fn thread_asking(workspace_id: Option<&str>, environment: Option<&ChangedFilesResult>, thread_id: Option<&str>) -> String {
    [workspace_id.unwrap_or(""), branch_of(environment).as_deref().unwrap_or(""), thread_id.unwrap_or("")].join("\0")
}

/// Whether reading a thread's checkout again can change how it stands under its coordinator.
pub fn awaits_pull_request(member: &Thread) -> bool {
    member.resolved_at.is_none() && owns_pull_request(member)
}

/// The checkouts on this computer a coordinator's threads work in, each with the threads in it.
pub fn member_checkouts<'a>(state: &'a WorkspaceState, lead_id: &str, counts: Option<fn(&Thread) -> bool>) -> Map<Vec<&'a Thread>> {
    let mut checkouts: Map<Vec<&Thread>> = Map::new();
    for member in coordination_members(state, lead_id) {
        if member.computer.is_some() || counts.is_some_and(|counts| !counts(member.thread)) {
            continue;
        }
        if let Some(workspace_id) = thread_workspace_id(state, Some(member.thread)) {
            checkouts.entry(workspace_id).or_default().push(member.thread);
        }
    }
    checkouts
}

/// A pull request one or more of a coordinator's threads have as their own.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberPullRequest<'a> {
    pub pull_request: OwnPullRequest,
    pub threads: Vec<&'a Thread>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemberPullRequestsView<'a> {
    /// Changes whenever a checkout joins, leaves, or moves branch, which is when to ask again.
    pub asking: String,
    /// True once no checkout's answer can change again.
    pub settled: bool,
    pub found: Vec<MemberPullRequest<'a>>,
}

impl Default for MemberPullRequestsView<'_> {
    fn default() -> Self {
        MemberPullRequestsView { asking: String::new(), settled: true, found: Vec::new() }
    }
}

/// What a coordinator's threads' pull requests are and when to ask about them again.
pub fn member_pull_requests_view<'a>(state: &'a WorkspaceState, lead: Option<&Thread>) -> MemberPullRequestsView<'a> {
    let Some(lead) = lead else { return MemberPullRequestsView::default() };
    let members = coordination_members(state, &lead.id);
    let mut found: Vec<MemberPullRequest> = Vec::new();
    for member in &members {
        let Some(pull_request) = own_pull_request(member.thread) else { continue };
        match found.iter_mut().find(|known| known.pull_request.url == pull_request.url) {
            Some(known) => known.threads.push(member.thread),
            None => found.push(MemberPullRequest { pull_request, threads: vec![member.thread] }),
        }
    }
    let checkouts = member_checkouts(state, &lead.id, Some(awaits_pull_request));
    let elsewhere: Vec<_> = members.iter().filter(|member| member.computer.is_some() && awaits_pull_request(member.thread)).collect();
    if found.is_empty() && checkouts.is_empty() && elsewhere.is_empty() {
        return MemberPullRequestsView::default();
    }
    let mut asking = Vec::new();
    let mut settled = true;
    for workspace_id in checkouts.keys() {
        asking.push(format!("{workspace_id}\0{}", branch_of(state.environments.get(workspace_id)).unwrap_or_default()));
        if !pull_request_settled(state.member_pull_requests.get(workspace_id).map(|read| &read.answer).unwrap_or(&PullRequestAnswer::None)) {
            settled = false;
        }
    }
    for member in elsewhere {
        let (Some(computer), Some(holder)) = (member.computer, member.holder()) else { continue };
        let workspace_id = thread_workspace_id(holder, Some(member.thread));
        let branch = workspace_id.as_ref().and_then(|id| branch_of(holder.environments.get(id))).unwrap_or_default();
        asking.push(format!("{}\0{}\0{branch}", computer.link.id, member.thread.id));
        if let Some(workspace_id) = &workspace_id {
            if !pull_request_settled(holder.member_pull_requests.get(workspace_id).map(|read| &read.answer).unwrap_or(&PullRequestAnswer::None)) {
                settled = false;
            }
        }
    }
    MemberPullRequestsView { asking: asking.join("\n"), settled, found }
}
